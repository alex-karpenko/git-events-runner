use super::{get_secret_strings, SecretRef};
use crate::{Error, Result};
use git2::{
    CertificateCheckStatus, Cred, FetchOptions, RemoteCallbacks, Repository, RepositoryInitOptions,
};
use kube::{Client, CustomResource};
use rustls::{
    client::{danger::ServerCertVerifier, WebPkiServerVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    RootCertStore,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use strum_macros::Display;
use tracing::{debug, warn};

const URI_VALIDATION_REGEX: &str = r#"^git@[\w.-]+:[\w.-]+/[/\w.-]+$|^ssh://([\w.-]+@)?[\w.-]+(:[\d]{1,5})?(/([/\w.-]+)?)?$|^https?://[\w.-]+(:[\d]{1,5})?(/([/%&=\?\w.-]+)?)?$"#;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "GitRepo",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct GitRepoSpec {
    #[schemars(regex = "URI_VALIDATION_REGEX")]
    pub repo_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_config: Option<TlsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<GitAuthConfig>,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "ClusterGitRepo",
    group = "git-events-runner.rs",
    version = "v1alpha1"
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterGitRepoSpec {
    #[schemars(regex = "URI_VALIDATION_REGEX")]
    pub repo_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_config: Option<TlsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<GitAuthConfig>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    #[serde(default)]
    no_verify_ssl: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ca_cert: Option<TlsCaConfig>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TlsCaConfig {
    secret_ref: SecretRef,
    #[serde(default = "TlsCaConfig::default_ca_data_key")]
    key: String,
}

impl TlsCaConfig {
    fn default_ca_data_key() -> String {
        String::from("ca.crt")
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitAuthConfig {
    #[serde(rename = "type")]
    auth_type: GitAuthType,
    secret_ref: SecretRef,
    #[serde(default)]
    keys: GitAuthSecretKeys,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Display)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum GitAuthType {
    Basic,
    Token,
    Ssh,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitAuthSecretKeys {
    username: String,
    password: String,
    token: String,
    private_key: String,
}

impl Default for GitAuthSecretKeys {
    fn default() -> Self {
        Self {
            username: String::from("username"),
            password: String::from("password"),
            token: String::from("token"),
            private_key: String::from("ssh-privatekey"),
        }
    }
}

impl GitRepoGetter for GitRepo {}
impl GitRepoGetter for ClusterGitRepo {}

trait GitRepoInternals {
    fn auth_config(&self) -> &Option<GitAuthConfig>;
    fn tls_config(&self) -> &Option<TlsConfig>;
    fn repo_uri(&self) -> &String;
}

impl GitRepoInternals for GitRepo {
    fn auth_config(&self) -> &Option<GitAuthConfig> {
        &self.spec.auth_config
    }

    fn tls_config(&self) -> &Option<TlsConfig> {
        &self.spec.tls_config
    }

    fn repo_uri(&self) -> &String {
        &self.spec.repo_uri
    }
}

impl GitRepoInternals for ClusterGitRepo {
    fn auth_config(&self) -> &Option<GitAuthConfig> {
        &self.spec.auth_config
    }

    fn tls_config(&self) -> &Option<TlsConfig> {
        &self.spec.tls_config
    }

    fn repo_uri(&self) -> &String {
        &self.spec.repo_uri
    }
}

#[allow(private_bounds, async_fn_in_trait)]
pub trait GitRepoGetter: GitRepoInternals {
    async fn fetch_repo_ref(
        &self,
        client: Client,
        ref_name: &String,
        path: &String,
        secrets_ns: &str,
    ) -> Result<Repository> {
        // Determine which secrets are mandatory and fetch them
        let auth_secrets: HashMap<String, String> = if let Some(auth_config) = self.auth_config() {
            let mut secret_keys: HashMap<&String, String> = HashMap::new();
            match auth_config.auth_type {
                GitAuthType::Basic => {
                    secret_keys.insert(&auth_config.keys.username, "username".into());
                    secret_keys.insert(&auth_config.keys.password, "password".into());
                }
                GitAuthType::Token => {
                    secret_keys.insert(&auth_config.keys.token, "token".into());
                }
                GitAuthType::Ssh => {
                    secret_keys.insert(&auth_config.keys.private_key, "private_key".into());
                }
            }
            get_secret_strings(
                client.clone(),
                &auth_config.secret_ref.name,
                secret_keys,
                secrets_ns,
            )
            .await?
        } else {
            HashMap::new()
        };

        let tls_secrets: HashMap<String, String> = if let Some(tls_config) = self.tls_config() {
            let mut secret_keys: HashMap<&String, String> = HashMap::new();
            if let Some(ca_cert) = &tls_config.ca_cert {
                secret_keys.insert(&ca_cert.key, "ca.crt".into());
                get_secret_strings(
                    client.clone(),
                    &ca_cert.secret_ref.name,
                    secret_keys,
                    secrets_ns,
                )
                .await?
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // Create callback object with necessary secrets
        let mut callbacks = RemoteCallbacks::new();
        let mut init_opts = RepositoryInitOptions::new();
        let mut fetch_opt = FetchOptions::default();
        if let Some(auth_config) = self.auth_config() {
            match auth_config.auth_type {
                GitAuthType::Basic => {
                    callbacks.credentials(move |_url, _username_from_url, _allowed_types| {
                        Cred::userpass_plaintext(
                            auth_secrets["username"].as_str(),
                            auth_secrets["password"].as_str(),
                        )
                    });
                }
                GitAuthType::Token => {
                    fetch_opt.custom_headers(&[format!(
                        "Authorization: {}",
                        auth_secrets["token"]
                    )
                    .as_str()]);
                }
                GitAuthType::Ssh => {
                    callbacks.credentials(move |_url, username_from_url, _allowed_types| {
                        Cred::ssh_key_from_memory(
                            username_from_url.unwrap_or(""),
                            None,
                            auth_secrets["private_key"].clone().as_str(),
                            None, // TODO: add support to keys with password
                        )
                    });
                }
            }
        }

        if let Some(tls_config) = self.tls_config() {
            if tls_config.no_verify_ssl {
                callbacks.certificate_check(move |_cert, _hostname| {
                    debug!("certificate check callback: no_verify_ssl=true");
                    Ok(CertificateCheckStatus::CertificateOk)
                });
            } else {
                debug!("certificate check callback: no_verify_ssl=false");
                if tls_config.ca_cert.is_some() {
                    let mut ca = tls_secrets.get("ca.crt").unwrap().as_bytes();
                    let ca = rustls_pemfile::certs(&mut ca).flatten();
                    // TODO: make system trust store global
                    let mut root_cert_store = RootCertStore::empty();
                    root_cert_store.add_parsable_certificates(
                        rustls_native_certs::load_native_certs() // TODO: Use Mozilla bundle
                            .expect("could not load platform certs"),
                    );
                    root_cert_store.add_parsable_certificates(ca);
                    let root_cert_store = Arc::new(root_cert_store);
                    callbacks.certificate_check(move |cert, hostname| {
                        let cert_verifier = WebPkiServerVerifier::builder(root_cert_store.clone())
                            .build()
                            .map_err(|_| {
                                git2::Error::new(
                                    git2::ErrorCode::Certificate,
                                    git2::ErrorClass::Callback,
                                    "unable to build root CA store",
                                )
                            })?;

                        let end_entity = {
                            if let Some(cert) = cert.as_x509() {
                                CertificateDer::from(cert.data())
                            } else {
                                return Err(git2::Error::new(
                                    git2::ErrorCode::Certificate,
                                    git2::ErrorClass::Callback,
                                    "unable to parse x509 certificate data",
                                ));
                            }
                        };
                        let hostname = ServerName::try_from(hostname).map_err(|_| {
                            git2::Error::new(
                                git2::ErrorCode::Certificate,
                                git2::ErrorClass::Callback,
                                "unable to get server name",
                            )
                        })?;
                        let result = cert_verifier.verify_server_cert(
                            &end_entity,
                            &[],
                            &hostname,
                            &[],
                            UnixTime::now(),
                        );
                        match result {
                            Ok(_) => Ok(CertificateCheckStatus::CertificateOk),
                            Err(err) => {
                                warn!("unable to verify server certificate with custom CA: {err}");
                                Ok(CertificateCheckStatus::CertificatePassthrough)
                            }
                        }
                    });
                } else {
                    callbacks.certificate_check(move |_, _| {
                        Ok(CertificateCheckStatus::CertificatePassthrough)
                    });
                }
            }
        }

        fetch_opt.remote_callbacks(callbacks);
        fetch_opt.depth(1);
        init_opts.origin_url(self.repo_uri());

        let repo = Repository::init_opts(path, &init_opts).map_err(Error::GitrepoAccessError)?;
        {
            let mut remote = repo
                .find_remote("origin")
                .map_err(Error::GitrepoAccessError)?;
            remote
                .fetch(&[ref_name], Some(&mut fetch_opt), None)
                .map_err(Error::GitrepoAccessError)?;
        }

        Ok(repo)
    }
}
