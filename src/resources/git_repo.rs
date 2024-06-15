use crate::{Error, Result};
use git2::{CertificateCheckStatus, Cred, FetchOptions, RemoteCallbacks, Repository, RepositoryInitOptions};
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client, CustomResource};
use rustls::{
    client::{danger::ServerCertVerifier, WebPkiServerVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    RootCertStore,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use strum_macros::Display;
use tracing::{debug, instrument, warn};

const URI_VALIDATION_REGEX: &str = r#"^git@[\w.-]+:[\w.-]+/[/\w.-]+$|^ssh://([\w.-]+@)?[\w.-]+(:[\d]{1,5})?(/([/\w.-]+)?)?$|^https?://[\w.-]+(:[\d]{1,5})?(/([/%&=\?\w.-]+)?)?$"#;

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "GitRepo",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    doc = "GitRepo custom resource definition",
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
    version = "v1alpha1",
    doc = "ClusterGitRepo custom resource definition"
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

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) namespace: Option<String>,
}

/// Shared private behavior of all kinds of git repo
trait GitRepoInternals {
    fn auth_config(&self) -> &Option<GitAuthConfig>;
    fn tls_config(&self) -> &Option<TlsConfig>;
    fn repo_uri(&self) -> &String;
    fn is_namespaced(&self) -> bool;
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

    fn is_namespaced(&self) -> bool {
        true
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

    fn is_namespaced(&self) -> bool {
        false
    }
}

impl GitRepoGetter for GitRepo {}
impl GitRepoGetter for ClusterGitRepo {}

/// Getter trait to implement shared behavior: it's able to get content (clone) of repo's particular reference
#[allow(private_bounds, async_fn_in_trait)]
pub trait GitRepoGetter: GitRepoInternals {
    #[instrument("fetch repo reference", skip_all, fields(reference=ref_name,path))]
    async fn fetch_repo_ref(
        &self,
        client: Client,
        ref_name: &String,
        path: &String,
        secrets_ns: &String,
    ) -> Result<Repository> {
        // Determine which secrets are mandatory and fetch them
        // TODO: refactor to use secrets cache and remove client from parameters
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
            let ns = if self.is_namespaced() {
                secrets_ns
            } else {
                auth_config.secret_ref.namespace.as_ref().unwrap_or(secrets_ns)
            };
            get_secret_strings(client.clone(), &auth_config.secret_ref.name, secret_keys, ns).await?
        } else {
            HashMap::new()
        };

        let tls_secrets: HashMap<String, String> = if let Some(tls_config) = self.tls_config() {
            let mut secret_keys: HashMap<&String, String> = HashMap::new();
            if let Some(ca_cert) = &tls_config.ca_cert {
                secret_keys.insert(&ca_cert.key, "ca.crt".into());

                let ns = if self.is_namespaced() {
                    secrets_ns
                } else {
                    ca_cert.secret_ref.namespace.as_ref().unwrap_or(secrets_ns)
                };

                get_secret_strings(client.clone(), &ca_cert.secret_ref.name, secret_keys, ns).await?
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // Create a callback object with necessary secrets
        let mut callbacks = RemoteCallbacks::new();
        let mut init_opts = RepositoryInitOptions::new();
        let mut fetch_opt = FetchOptions::default();
        if let Some(auth_config) = self.auth_config() {
            match auth_config.auth_type {
                GitAuthType::Basic => {
                    callbacks.credentials(move |_url, _username_from_url, _allowed_types| {
                        Cred::userpass_plaintext(auth_secrets["username"].as_str(), auth_secrets["password"].as_str())
                    });
                }
                GitAuthType::Token => {
                    fetch_opt.custom_headers(&[format!("Authorization: {}", auth_secrets["token"]).as_str()]);
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
            debug!(no_verify_ssl = %tls_config.no_verify_ssl, "certificate check callback");
            if tls_config.no_verify_ssl {
                // ignore ca verification - lets consider it as valid
                callbacks.certificate_check(move |_cert, _hostname| Ok(CertificateCheckStatus::CertificateOk));
            } else if tls_config.ca_cert.is_some() {
                // if we have our own CA specified:
                // create root CA store from system one and add our CA to it
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
                    let cert_verifier =
                        WebPkiServerVerifier::builder(root_cert_store.clone())
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

                    // verify server's c ert against just create trust store
                    let result = cert_verifier.verify_server_cert(&end_entity, &[], &hostname, &[], UnixTime::now());
                    match result {
                        // if everything verified - return ok
                        Ok(_) => Ok(CertificateCheckStatus::CertificateOk),
                        Err(err) => {
                            warn!(error = %err, "verifying server certificate with custom CA");
                            // if not - pass responsibility to usual verification method
                            Ok(CertificateCheckStatus::CertificatePassthrough)
                        }
                    }
                });
            } else {
                // no custom CA - let's verify cert in a usual way
                callbacks.certificate_check(move |_, _| Ok(CertificateCheckStatus::CertificatePassthrough));
            }
        }

        fetch_opt.remote_callbacks(callbacks);
        fetch_opt.depth(1); // don't clone whole repo, just single ref we need
        init_opts.origin_url(self.repo_uri());

        let repo = Repository::init_opts(path, &init_opts).map_err(Error::GitrepoAccessError)?;
        {
            let mut remote = repo.find_remote("origin").map_err(Error::GitrepoAccessError)?;
            remote
                .fetch(&[ref_name], Some(&mut fetch_opt), None)
                .map_err(Error::GitrepoAccessError)?;
        }

        Ok(repo)
    }
}

/// TODO: refactor it to use secrets cache
/// Retrieve all keys from the specified secret
///
/// keys is map of "key in the secret" -> "key to put in the result map".
/// it's useful to retrieve come configured keys and expose some pre-defined (expected) keys
/// i.e. in the actual secret we have key "pass", but we want to address it as "password".
/// use this approach to unify secrets keys for authorization in remote repos.
///
/// actually this is quite awkward approach and it's subject to complete reworking
///
async fn get_secret_strings<'a>(
    client: Client,
    secret_name: &'a String,
    secret_keys: HashMap<&String, String>,
    ns: &'a str,
) -> Result<HashMap<String, String>> {
    let secret_api: Api<Secret> = Api::namespaced(client, ns);
    let secret_ref = secret_api.get(secret_name).await?;
    let mut secrets: HashMap<String, String> = HashMap::new();

    let secret_data_b64 = secret_ref
        .clone()
        .data
        .ok_or_else(|| Error::SecretDecodingError(format!("no `data` part in the secret `{}`", secret_name)))?;

    for (secret_key, expected_key) in secret_keys.into_iter() {
        let secret_data = secret_data_b64
            .get(secret_key)
            .ok_or_else(|| {
                Error::SecretDecodingError(format!("no `{}` key in the secret `{}`", secret_key, secret_name))
            })?
            .to_owned();
        let secret_data = String::from_utf8(secret_data.0).map_err(|_e| {
            Error::SecretDecodingError(format!(
                "error converting string `{}` from UTF8 in the secret `{}`",
                secret_key, secret_name
            ))
        })?;
        secrets.insert(expected_key, secret_data);
    }

    Ok(secrets)
}
