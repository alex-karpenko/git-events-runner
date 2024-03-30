use super::{Context, GitRepoStatus, Reconcilable, SecretRef, SourceState};
use crate::{
    controllers::{API_GROUP, CURRENT_API_VERSION},
    Error, Result,
};
use git2::{
    CertificateCheckStatus, Cred, FetchOptions, RemoteCallbacks, Repository, RepositoryInitOptions,
};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    api::{Patch, PatchParams},
    runtime::{
        controller::Action as ReconcileAction,
        events::{Event, EventType, Recorder},
    },
    Api, Client, CustomResource, ResourceExt,
};
use regex::Regex;
use rustls::{
    client::{danger::ServerCertVerifier, WebPkiServerVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    RootCertStore,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Arc,
};
use strum_macros::{Display, EnumString};
use tokio::time::Duration;
use tracing::{debug, info, warn};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "GitRepo",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    namespaced,
    printcolumn = r#"{"name":"State", "type":"string", "description":"current trigger state", "jsonPath":".status.state"}"#
)]
#[kube(status = "GitRepoStatus")]
#[serde(rename_all = "camelCase")]
pub struct GitRepoSpec {
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

#[derive(EnumString, Debug, Clone, PartialEq, Display)]
#[strum(serialize_all = "lowercase")]
enum RepoUriSchema {
    Http,
    Https,
    Ssh,
    Git,
}

impl Reconcilable for GitRepo {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        let client = ctx.client.clone();
        let recorder = &ctx.diagnostics.read().await.recorder(client.clone(), self);
        let ns = self.namespace().unwrap();
        let repos: Api<GitRepo> = Api::namespaced(client.clone(), &ns);

        // Verify URI format
        let repo_uri_schema = self.parse_repo_uri();
        debug!("repo_uri_schema={:?}", repo_uri_schema);
        if repo_uri_schema.is_err() {
            self.update_resource_state(SourceState::WrongRepoUriFormat, &repos)
                .await?;
            self.publish_source_validation_event(
                recorder,
                EventType::Warning,
                format!("Wrong repository URI format `{}`", self.spec.repo_uri).as_str(),
                "ValidateGitRepositoryUri",
            )
            .await?;
            return Err(Error::WrongSourceUri);
        }

        // Validate auth type and secrets if needed
        let schema = repo_uri_schema.unwrap();
        if let Some(auth_config) = &self.spec.auth_config {
            // Check auth_type and schema compatibility
            let is_schema_compatible = match schema {
                RepoUriSchema::Http => {
                    self.publish_source_validation_event(
                        recorder,
                        EventType::Warning,
                        "Authentication over raw `http` protocol isn`t secure, try to use `https` instead",
                        "ValidateSourceAuthConfig",
                    )
                    .await?;

                    auth_config.auth_type == GitAuthType::Basic
                        || auth_config.auth_type == GitAuthType::Token
                }
                RepoUriSchema::Https => {
                    auth_config.auth_type == GitAuthType::Basic
                        || auth_config.auth_type == GitAuthType::Token
                }
                RepoUriSchema::Ssh => auth_config.auth_type == GitAuthType::Ssh,
                RepoUriSchema::Git => auth_config.auth_type == GitAuthType::Ssh,
            };
            if !is_schema_compatible {
                self.update_resource_state(SourceState::AuthConfigError, &repos)
                    .await?;
                self.publish_source_validation_event(
                    recorder,
                    EventType::Warning,
                    format!(
                        "Auth config is wrong: auth type `{}` is incompatible with schema `{schema}`",
                        auth_config.auth_type
                    )
                    .as_str(),
                    "ValidateSourceAuthConfig",
                )
                .await?;
                return Err(Error::WrongAuthConfig);
            }

            // Auth secrets should be present and contains minimum required fields (depending on auth_type)
            let secret_name = self.spec.auth_config.clone().unwrap().secret_ref.name;
            let keys_to_check = match auth_config.auth_type {
                GitAuthType::Basic => vec![
                    self.spec.auth_config.clone().unwrap().keys.username,
                    self.spec.auth_config.clone().unwrap().keys.password,
                ],
                GitAuthType::Token => vec![self.spec.auth_config.clone().unwrap().keys.token],
                GitAuthType::Ssh => vec![self.spec.auth_config.clone().unwrap().keys.private_key],
            };
            let result = self
                .check_secret_ref(
                    client.clone(),
                    &secret_name,
                    &keys_to_check,
                    Error::WrongAuthConfig,
                )
                .await;
            if result.is_err() {
                self.publish_source_validation_event(
                    recorder,
                    EventType::Warning,
                    format!("Auth config may be wrong: secret `{secret_name}` should exist and contain `{}` key(s)", keys_to_check.join(",")).as_str(),
                    "ValidateSourceAuthConfig",
                )
                .await?;
            }
        } else {
            // Empty AuthConfig
            // SSH schema requires auth config
            if schema == RepoUriSchema::Ssh {
                self.update_resource_state(SourceState::AuthConfigError, &repos)
                    .await?;
                self.publish_source_validation_event(
                    recorder,
                    EventType::Warning,
                    "Auth config is required for SSH protocol",
                    "ValidateSourceAuthConfig",
                )
                .await?;
                return Err(Error::WrongAuthConfig);
            }
        }

        // Validate tls config if it's present and schema is https
        if schema == RepoUriSchema::Https && self.spec.tls_config.is_some() {
            let tls_config = self.spec.tls_config.clone().unwrap();

            if let Some(ca) = tls_config.ca_cert {
                let secret_name = ca.secret_ref.name;
                let keys_to_check = vec![ca.key];
                let result = self
                    .check_secret_ref(
                        client.clone(),
                        &secret_name,
                        &keys_to_check,
                        Error::WrongTlsConfig,
                    )
                    .await;
                if result.is_err() {
                    self.publish_source_validation_event(
                        recorder,
                        EventType::Warning,
                        format!("Auth config may be wrong: secret `{secret_name}` should exist and contain `{}` key(s)", keys_to_check.join(",")).as_str(),
                        "ValidateSourceAuthConfig",
                    )
                    .await?;
                }
            }
        }

        // Always overwrite status object with what we saw
        self.update_resource_state(SourceState::Ready, &repos)
            .await?;
        // If no events were received, check back in 30 minutes
        Ok(ReconcileAction::requeue(Duration::from_secs(30 * 60)))
    }

    async fn cleanup(&self, _ctx: Arc<Context>) -> Result<ReconcileAction> {
        info!(
            "Cleanup GitRepo `{}` in {}",
            self.name_any(),
            self.namespace().unwrap()
        );
        Ok(ReconcileAction::await_change())
    }

    fn finalizer_name(&self) -> String {
        String::from("gitrepos.git-events-runner.rs")
    }

    fn kind(&self) -> &str {
        "GitRepo"
    }
}

impl GitRepo {
    fn parse_repo_uri(&self) -> Result<RepoUriSchema> {
        // 1 - r#"^(?P<schema>git)@(?P<host>[\w.-]+):(?P<owner>[\w.-]+)/(?P<repo>[/\w.-]+)$"#
        // git@host.name:owner/repo.git
        //
        // 2 - r#"^(?P<schema>ssh)://((?P<username>[\w.-]+)@)?(?P<host>[\w.-]+)(:(?P<port>[\d]{1,5}))?(/(?P<repo>[/\w.-]+)?)?$"#
        // ssh://host.name/org/repo/something
        // ssh://host.name:123/org/repo/something
        // ssh://host.name:123/
        // ssh://login@host.name/org/repo/something
        // ssh://login@host.name:123/org/repo/something
        //
        // 3 - r#"^(?P<schema>https?)://(?P<host>[\w.-]+)(:(?P<port>[\d]{1,5}))?(/(?P<repo>[/%&=\?\w.-]+)?)?$"#
        // https://host.name/org/repo/something
        // http://host.name/
        // http://host.name:123/
        // http://host.name:123/org/repo/something

        const GIT_REPO_URI_REGEX: [&str; 3] = [
            r#"^(?P<schema>git)@(?P<host>[\w.-]+):(?P<owner>[\w.-]+)/(?P<repo>[/\w.-]+)$"#,
            r#"^(?P<schema>ssh)://((?P<username>[\w.-]+)@)?(?P<host>[\w.-]+)(:(?P<port>[\d]{1,5}))?(/(?P<repo>[/\w.-]+)?)?$"#,
            r#"^(?P<schema>https?)://(?P<host>[\w.-]+)(:(?P<port>[\d]{1,5}))?(/(?P<repo>[/%&=\?\w.-]+)?)?$"#,
        ];

        for re in &GIT_REPO_URI_REGEX {
            let re = Regex::new(re).expect("Looks like a BUG");
            let result = re.captures(self.spec.repo_uri.as_str());

            if let Some(result) = result {
                let schema = String::from(&result["schema"]);
                let schema = RepoUriSchema::from_str(&schema).expect("Looks like a BUG");
                return Ok(schema);
            }
        }
        Err(Error::WrongSourceUri)
    }

    async fn update_resource_state(&self, state: SourceState, api: &Api<GitRepo>) -> Result<()> {
        let name = self.name_any();
        let new_status = Patch::Apply(json!({
            "apiVersion": format!("{API_GROUP}/{CURRENT_API_VERSION}"),
            "kind": "GitRepo",
            "status": GitRepoStatus {
                state,
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = api
            .patch_status(&name, &ps, &new_status)
            .await
            .map_err(Error::KubeError)?;

        Ok(())
    }

    async fn publish_source_validation_event(
        &self,
        recorder: &Recorder,
        type_: EventType,
        note: &str,
        action: &str,
    ) -> Result<()> {
        recorder
            .publish(Event {
                type_,
                reason: "ValidateSource".into(),
                note: Some(note.into()),
                action: action.into(),
                secondary: None,
            })
            .await
            .map_err(Error::KubeError)?;

        Ok(())
    }

    async fn check_secret_ref(
        &self,
        client: Client,
        secret_name: &str,
        keys: &[String],
        error: Error,
    ) -> Result<()> {
        let ns = self.namespace().unwrap();
        let secret_api: Api<Secret> = Api::namespaced(client, &ns);
        let expected_keys: HashSet<&String> = keys.iter().collect();

        let secret_ref = secret_api
            .get(secret_name)
            .await
            .map_err(Error::KubeError)?;

        debug!("secret_ref={secret_ref:#?}");
        debug!("expected_keys={expected_keys:?}");

        if let Some(data) = secret_ref.data {
            // Check data field
            let secret_keys: HashSet<&String> = data.keys().collect();
            debug!("data_keys={secret_keys:?}");
            if secret_keys.intersection(&expected_keys).count() == expected_keys.len() {
                return Ok(());
            };
        }

        if let Some(data) = secret_ref.string_data {
            // Check string_data field
            let secret_keys: HashSet<&String> = data.keys().collect();
            debug!("string_data_keys={secret_keys:?}");
            if secret_keys.intersection(&expected_keys).count() == expected_keys.len() {
                return Ok(());
            };
        }

        Err(error)
    }

    pub async fn fetch_repo_ref(
        &self,
        client: Client,
        ref_name: &String,
        path: &String,
    ) -> Result<Repository> {
        // Determine which secrets are mandatory and fetch them
        let auth_secrets: HashMap<String, String> =
            if let Some(auth_config) = &self.spec.auth_config {
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
                self.get_secret_strings(client.clone(), &auth_config.secret_ref.name, secret_keys)
                    .await?
            } else {
                HashMap::new()
            };

        let tls_secrets: HashMap<String, String> = if let Some(tls_config) = &self.spec.tls_config {
            let mut secret_keys: HashMap<&String, String> = HashMap::new();
            if let Some(ca_cert) = &tls_config.ca_cert {
                secret_keys.insert(&ca_cert.key, "ca.crt".into());
                self.get_secret_strings(client.clone(), &ca_cert.secret_ref.name, secret_keys)
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
        if let Some(auth_config) = &self.spec.auth_config {
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
                            username_from_url.unwrap(),
                            None,
                            auth_secrets["private_key"].clone().as_str(),
                            None,
                        )
                    });
                }
            }
        }

        if let Some(tls_config) = &self.spec.tls_config {
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
                        rustls_native_certs::load_native_certs()
                            .expect("could not load platform certs"),
                    );
                    root_cert_store.add_parsable_certificates(ca);
                    let root_cert_store = Arc::new(root_cert_store);
                    callbacks.certificate_check(move |cert, hostname| {
                        let cert_verifier =
                            WebPkiServerVerifier::builder(root_cert_store.clone())
                                .build()
                                .unwrap();
                        let end_entity = CertificateDer::from(cert.as_x509().unwrap().data());
                        let hostname = ServerName::try_from(hostname).unwrap();
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
        init_opts.origin_url(&self.spec.repo_uri);

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

    async fn get_secret_strings(
        &self,
        client: Client,
        secret_name: &String,
        secret_keys: HashMap<&String, String>,
    ) -> Result<HashMap<String, String>> {
        let ns = self.namespace().unwrap();
        let secret_api: Api<Secret> = Api::namespaced(client, &ns);
        let secret_ref = secret_api
            .get(secret_name)
            .await
            .map_err(Error::KubeError)?;
        let mut secrets: HashMap<String, String> = HashMap::new();

        for (secret_key, expected_key) in secret_keys.into_iter() {
            let secret_data_b64 = secret_ref.clone().data.ok_or_else(|| {
                Error::GitrepoSecretDecodingError(format!(
                    "no `data` part in the secret `{}`",
                    secret_name
                ))
            })?;
            let secret_data = secret_data_b64
                .get(secret_key)
                .ok_or_else(|| {
                    Error::GitrepoSecretDecodingError(format!(
                        "no `{}` key in the secret `{}`",
                        secret_key, secret_name
                    ))
                })?
                .to_owned();
            let secret_data = String::from_utf8(secret_data.0).map_err(|_e| {
                Error::GitrepoSecretDecodingError(format!(
                    "error converting string `{}` from UTF8 in the secret `{}`",
                    secret_key, secret_name
                ))
            })?;
            secrets.insert(expected_key, secret_data);
        }

        Ok(secrets)
    }
}
