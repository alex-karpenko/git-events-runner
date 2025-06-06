//! GitRepo CRDs
use super::CustomApiResource;
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

/// GitRepo CRD spec section
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
    /// Full URI of the repo
    #[schemars(regex = "URI_VALIDATION_REGEX")]
    pub repo_uri: String,
    /// TLS config section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_config: Option<TlsConfig>,
    /// Auth config section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<GitAuthConfig>,
}

/// ClusterGitRepo CRD spec section
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
    /// Full URI of the repo
    #[schemars(regex = "URI_VALIDATION_REGEX")]
    pub repo_uri: String,
    /// TLS config section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_config: Option<TlsConfig>,
    /// Auth config section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<GitAuthConfig>,
}

/// Repo TLS config section
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    #[serde(default)]
    no_verify_ssl: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ca_cert: Option<TlsCaConfig>,
}

/// Repo custom TLS CA config
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

/// Repo authentication config
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitAuthConfig {
    #[serde(rename = "type")]
    auth_type: GitAuthType,
    secret_ref: SecretRef,
    #[serde(default)]
    keys: GitAuthSecretKeys,
}

/// Repos' type of the authentication
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Display)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum GitAuthType {
    /// Basic auth using Base64 encoded login:password pair as a token
    /// in the header: `Authorization: Basic <token>`
    Basic,
    /// Arbitrary token in Authorization header, like
    /// `Authorization: <token>`
    Token,
    /// Use SSH private key to auth
    Ssh,
}

/// Keys in the secret data section for different auth parameters
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

/// Reference to the secret
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

impl CustomApiResource for GitRepo {
    fn crd_kind() -> &'static str {
        "GitRepo"
    }
}

impl CustomApiResource for ClusterGitRepo {
    fn crd_kind() -> &'static str {
        "ClusterGitRepo"
    }
}

impl GitRepoGetter for GitRepo {}
impl GitRepoGetter for ClusterGitRepo {}

/// Represents type of the URI schema
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(missing_docs)]
pub enum GitRepoUriSchema {
    Http,
    Https,
    Ssh,
    Git,
}

/// Getter trait to implement shared behavior: it's able to get content (clone) of repo's particular reference
#[allow(private_bounds, async_fn_in_trait)]
pub trait GitRepoGetter: GitRepoInternals {
    /// Retrieve content of the particular reference of the repo into the specified path
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
                            username_from_url.unwrap_or("git"),
                            None,
                            auth_secrets["private_key"].clone().as_str(),
                            None,
                        )
                    });
                }
            }
        }

        match self.uri_schema() {
            GitRepoUriSchema::Http => {}
            GitRepoUriSchema::Ssh | GitRepoUriSchema::Git => {
                callbacks.certificate_check(move |_cert, _hostname| Ok(CertificateCheckStatus::CertificateOk));
            }
            GitRepoUriSchema::Https => {
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

                            // verify server's c ert against just create trust store
                            let result =
                                cert_verifier.verify_server_cert(&end_entity, &[], &hostname, &[], UnixTime::now());
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
            }
        }

        fetch_opt.remote_callbacks(callbacks);
        let depth = match self.uri_schema() {
            GitRepoUriSchema::Http | GitRepoUriSchema::Https => 1,
            GitRepoUriSchema::Ssh | GitRepoUriSchema::Git => 0,
        };
        fetch_opt.depth(depth);

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

    /// Parse repo URI and return its schema type
    fn uri_schema(&self) -> GitRepoUriSchema {
        if self.repo_uri().starts_with("http://") {
            GitRepoUriSchema::Http
        } else if self.repo_uri().starts_with("https://") {
            GitRepoUriSchema::Https
        } else if self.repo_uri().starts_with("ssh://") {
            GitRepoUriSchema::Ssh
        } else if self.repo_uri().starts_with("git@") {
            GitRepoUriSchema::Git
        } else {
            panic!("Unknown URI schema");
        }
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
        .ok_or_else(|| Error::SecretDecodingError(format!("no `data` part in the secret `{secret_name}`")))?;

    for (secret_key, expected_key) in secret_keys.into_iter() {
        let secret_data = secret_data_b64
            .get(secret_key)
            .ok_or_else(|| Error::SecretDecodingError(format!("no `{secret_key}` key in the secret `{secret_name}`")))?
            .to_owned();
        let secret_data = String::from_utf8(secret_data.0).map_err(|_e| {
            Error::SecretDecodingError(format!(
                "error converting string `{secret_key}` from UTF8 in the secret `{secret_name}`"
            ))
        })?;
        secrets.insert(expected_key, secret_data);
    }

    Ok(secrets)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
mod test {
    use super::*;
    use crate::tests::{self, get_test_git_ca};
    use base64::{prelude::BASE64_STANDARD, Engine as _};
    use k8s_openapi::{api::core::v1::Secret, ByteString};
    use kube::api::{Api, DeleteParams, ObjectMeta, PostParams};
    use rstest::*;
    use rstest_reuse::{apply, template};
    use std::collections::{BTreeMap, HashMap};

    async fn ensure_secret(client: Client, name: &str, data: BTreeMap<String, String>, ns: &str) -> anyhow::Result<()> {
        let secret_api: Api<Secret> = Api::namespaced(client, ns);
        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                ..Default::default()
            },
            data: Some(data.into_iter().map(|(k, v)| (k, ByteString(v.into()))).collect()),
            ..Default::default()
        };

        let _ = secret_api.delete(name, &DeleteParams::default()).await;
        secret_api.create(&PostParams::default(), &secret).await?;

        Ok(())
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn get_secret_strings_with_data() {
        const TEST_SECRET_NAME: &str = "test-secret-get-secret-strings-with-data";
        const TEST_NAMESPACE: &str = "default";

        let client = tests::get_test_kube_client().await.unwrap();
        let secret_api: Api<Secret> = Api::namespaced(client.clone(), TEST_NAMESPACE);

        let secret_keys = [
            (&String::from("u"), String::from("username")),
            (&String::from("p"), String::from("password")),
        ];
        let secret_keys: HashMap<&String, String> = HashMap::from(secret_keys);

        let secret_data = BTreeMap::from([
            (String::from("u"), String::from("user")),
            (String::from("p"), String::from("pass")),
        ]);

        ensure_secret(client.clone(), TEST_SECRET_NAME, secret_data, TEST_NAMESPACE)
            .await
            .unwrap();

        let secrets = get_secret_strings(client.clone(), &TEST_SECRET_NAME.into(), secret_keys, TEST_NAMESPACE)
            .await
            .unwrap();

        assert_eq!(secrets["username"], "user");
        assert_eq!(secrets["password"], "pass");

        secret_api
            .delete(TEST_SECRET_NAME, &DeleteParams::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn get_secret_strings_wrong_keys() {
        const TEST_SECRET_NAME: &str = "test-secret-get-secret-strings-wrong-keys";
        const TEST_NAMESPACE: &str = "default";

        let client = tests::get_test_kube_client().await.unwrap();
        let secret_api: Api<Secret> = Api::namespaced(client.clone(), TEST_NAMESPACE);

        let secret_data = BTreeMap::from([
            (String::from("u"), String::from("user")),
            (String::from("p"), String::from("pass")),
        ]);

        ensure_secret(client.clone(), TEST_SECRET_NAME, secret_data, TEST_NAMESPACE)
            .await
            .unwrap();

        let secret_keys = [(&String::from("qqq"), String::from("www"))];
        let secret_keys: HashMap<&String, String> = HashMap::from(secret_keys);
        let secrets = get_secret_strings(client, &TEST_SECRET_NAME.into(), secret_keys, TEST_NAMESPACE).await;

        assert!(secrets.is_err());
        let err = secrets.unwrap_err();
        assert!(err.to_string().contains("no `qqq` key in the secret"));
        assert!(matches!(err, Error::SecretDecodingError(_)));

        secret_api
            .delete(TEST_SECRET_NAME, &DeleteParams::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn get_secret_strings_no_data() {
        const TEST_SECRET_NAME: &str = "test-secret-get-secret-strings-no-data";
        const TEST_NAMESPACE: &str = "default";

        let client = tests::get_test_kube_client().await.unwrap();
        let secret_api: Api<Secret> = Api::namespaced(client.clone(), TEST_NAMESPACE);

        let secret_keys = [
            (&String::from("u"), String::from("username")),
            (&String::from("p"), String::from("password")),
        ];
        let secret_keys: HashMap<&String, String> = HashMap::from(secret_keys);

        ensure_secret(client.clone(), TEST_SECRET_NAME, BTreeMap::new(), TEST_NAMESPACE)
            .await
            .unwrap();

        let secrets = get_secret_strings(client.clone(), &TEST_SECRET_NAME.into(), secret_keys, TEST_NAMESPACE).await;

        assert!(secrets.is_err());
        let err = secrets.unwrap_err();
        assert!(err.to_string().contains("no `data` part in the secret"));
        assert!(matches!(err, Error::SecretDecodingError(_)));

        secret_api
            .delete(TEST_SECRET_NAME, &DeleteParams::default())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn get_secret_strings_no_secret() {
        const TEST_SECRET_NAME: &str = "test-secret-get-secret-strings-no-secret";
        const TEST_NAMESPACE: &str = "default";

        let client = tests::get_test_kube_client().await.unwrap();

        let secret_keys = [
            (&String::from("u"), String::from("username")),
            (&String::from("p"), String::from("password")),
        ];
        let secret_keys: HashMap<&String, String> = HashMap::from(secret_keys);

        let secrets = get_secret_strings(client, &TEST_SECRET_NAME.into(), secret_keys, TEST_NAMESPACE).await;

        assert!(secrets.is_err());
        let err = secrets.unwrap_err();
        assert!(err.to_string().contains("not found"));
        assert!(matches!(err, Error::KubeError(_)));
    }

    // Kind: GitRepo, ClusterGitRepo
    //
    // Visibility: public, private
    // Schema: https, ssh
    // TLS: no verify, verify, verify with CA
    // Auth: none, basic, ssh, token (just use "Basic b64e(username:password"))
    // Branch/tag: main, v1, unknown
    // Secret ns: default, non-exiting

    enum TestRepoVisibility {
        Public,
        Private,
    }

    enum TestRepoUriSchema {
        Https,
        Git,
        Ssh,
    }

    enum TestTlsConfig {
        None,
        Ignore,
        Verify,
        VerifyWithCa,
    }

    enum TestAuthConfig {
        None,
        Basic,
        Token,
        Ssh,
    }

    #[derive(Debug, Clone)]
    struct TestGitRepoBuilder {
        name: String,
        repo_uri: String,
        tls_config: Option<TlsConfig>,
        auth_config: Option<GitAuthConfig>,
    }

    impl TestGitRepoBuilder {
        async fn build(
            client: Client,
            name: impl Into<String>,
            type_: TestRepoVisibility,
            schema: TestRepoUriSchema,
            tls: TestTlsConfig,
            auth: TestAuthConfig,
            ns: Option<String>,
        ) -> Self {
            const TEST_PUBLIC_REPO_PATH: &str = "gitea-admin/test-1.git";
            const TEST_PRIVATE_REPO_PATH: &str = "gitea-admin/test-2.git";
            const TEST_SECRET_PREFIX: &str = "git-repo-test-secret";
            const TEST_GIT_USERNAME: &str = "gitea-admin";
            const TEST_GIT_PASSWORD: &str = "gitea-admin";

            let name = name.into();
            let hostname = tests::get_test_git_hostname().await.unwrap();
            let secrets_ns = ns.clone().unwrap_or("default".into());
            let tls_secret_name = format!("{TEST_SECRET_PREFIX}-{name}-tls");
            let auth_secret_name = format!("{TEST_SECRET_PREFIX}-{name}-auth");
            let ssh_key = include_str!("../../tests/ssh/test-key-ed25519").to_string();
            let ca = get_test_git_ca().await.unwrap();

            let repo_path = match type_ {
                TestRepoVisibility::Public => TEST_PUBLIC_REPO_PATH,
                TestRepoVisibility::Private => TEST_PRIVATE_REPO_PATH,
            };

            let repo_uri = match schema {
                TestRepoUriSchema::Https => format!("https://{hostname}/{repo_path}"),
                TestRepoUriSchema::Ssh => format!("ssh://git@{hostname}/{repo_path}"),
                TestRepoUriSchema::Git => format!("git@{hostname}:{repo_path}"),
            };

            let tls_config = match tls {
                TestTlsConfig::None => None,
                TestTlsConfig::Ignore => Some(TlsConfig {
                    no_verify_ssl: true,
                    ..Default::default()
                }),
                TestTlsConfig::Verify => Some(TlsConfig {
                    no_verify_ssl: false,
                    ..Default::default()
                }),
                TestTlsConfig::VerifyWithCa => {
                    Self::put_strings_to_secret(client.clone(), &[&ca], &["ca.crt"], &tls_secret_name, &secrets_ns)
                        .await
                        .unwrap();
                    Some(TlsConfig {
                        no_verify_ssl: false,
                        ca_cert: Some(TlsCaConfig {
                            key: "ca.crt".into(),
                            secret_ref: SecretRef {
                                name: tls_secret_name,
                                namespace: ns.clone(),
                            },
                        }),
                    })
                }
            };

            let auth_config = match auth {
                TestAuthConfig::None => None,
                TestAuthConfig::Basic => {
                    let data = BTreeMap::from([
                        ("username".into(), TEST_GIT_USERNAME.into()),
                        ("password".into(), TEST_GIT_PASSWORD.into()),
                    ]);
                    ensure_secret(client.clone(), &auth_secret_name, data, &secrets_ns)
                        .await
                        .unwrap();
                    Some(GitAuthConfig {
                        auth_type: GitAuthType::Basic,
                        secret_ref: SecretRef {
                            name: auth_secret_name,
                            namespace: ns,
                        },
                        keys: GitAuthSecretKeys::default(),
                    })
                }
                TestAuthConfig::Token => {
                    let basic_token = BASE64_STANDARD.encode(format!("{TEST_GIT_USERNAME}:{TEST_GIT_PASSWORD}"));
                    let basic_token = format!("Basic {basic_token}");
                    let data = BTreeMap::from([("token".into(), basic_token)]);
                    ensure_secret(client.clone(), &auth_secret_name, data, &secrets_ns)
                        .await
                        .unwrap();

                    Some(GitAuthConfig {
                        auth_type: GitAuthType::Token,
                        secret_ref: SecretRef {
                            name: auth_secret_name,
                            namespace: ns,
                        },
                        keys: GitAuthSecretKeys::default(),
                    })
                }
                TestAuthConfig::Ssh => {
                    Self::put_strings_to_secret(
                        client.clone(),
                        &[&ssh_key],
                        &["ssh-privatekey"],
                        &auth_secret_name,
                        &secrets_ns,
                    )
                    .await
                    .unwrap();
                    Some(GitAuthConfig {
                        auth_type: GitAuthType::Ssh,
                        secret_ref: SecretRef {
                            name: auth_secret_name,
                            namespace: ns,
                        },
                        keys: GitAuthSecretKeys::default(),
                    })
                }
            };

            Self {
                name,
                repo_uri,
                tls_config,
                auth_config,
            }
        }

        async fn put_strings_to_secret(
            client: Client,
            data: &[&String],
            keys: &[&str],
            secret_name: &str,
            ns: &str,
        ) -> anyhow::Result<()> {
            let data = keys
                .iter()
                .map(|s| String::from(*s))
                .zip(data.iter().map(|s| String::from(*s)))
                .collect::<BTreeMap<_, _>>();

            ensure_secret(client, secret_name, data, ns).await
        }
    }

    impl From<TestGitRepoBuilder> for GitRepo {
        fn from(value: TestGitRepoBuilder) -> Self {
            let spec = GitRepoSpec {
                repo_uri: value.repo_uri,
                auth_config: value.auth_config,
                tls_config: value.tls_config,
            };

            GitRepo::new(value.name.as_str(), spec)
        }
    }

    impl From<TestGitRepoBuilder> for ClusterGitRepo {
        fn from(value: TestGitRepoBuilder) -> Self {
            let spec = ClusterGitRepoSpec {
                repo_uri: value.repo_uri,
                auth_config: value.auth_config,
                tls_config: value.tls_config,
            };

            ClusterGitRepo::new(value.name.as_str(), spec)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    enum Expected {
        Ok,
        Err,
    }

    #[template]
    #[rstest]
    #[case( // 01
        "public-https-no-verify-main",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Https,
        TestTlsConfig::Ignore,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 02
        "public-https-verify-main",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Https,
        TestTlsConfig::Verify,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Err
    )]
    #[case( // 03
        "public-https-verify-ca-main",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Https,
        TestTlsConfig::VerifyWithCa,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 04
        "public-ssh-main-anon",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Ssh,
        TestTlsConfig::None,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Err
    )]
    #[case( // 05
        "public-git-main-anon",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Git,
        TestTlsConfig::None,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Err
    )]
    #[case( // 06
        "public-ssh-main-ssh",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Ssh,
        TestTlsConfig::None,
        TestAuthConfig::Ssh,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 07
        "public-git-main-ssh",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Git,
        TestTlsConfig::None,
        TestAuthConfig::Ssh,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 08
        "private-https-verify-ca-main-anon",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Https,
        TestTlsConfig::VerifyWithCa,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Err
    )]
    #[case( // 09
        "private-https-verify-ca-main-basic",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Https,
        TestTlsConfig::VerifyWithCa,
        TestAuthConfig::Basic,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 10
        "private-https-verify-ca-main-token",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Https,
        TestTlsConfig::VerifyWithCa,
        TestAuthConfig::Token,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 11
        "private-ssh-main-ssh",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Ssh,
        TestTlsConfig::None,
        TestAuthConfig::Ssh,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 12
        "private-git-main-ssh",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Git,
        TestTlsConfig::None,
        TestAuthConfig::Ssh,
        "main",
        "default",
        Expected::Ok
    )]
    #[case( // 13
        "private-ssh-main-ssh-anon",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Ssh,
        TestTlsConfig::None,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Err
    )]
    #[case( // 14
        "private-git-main-ssh-anon",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Git,
        TestTlsConfig::None,
        TestAuthConfig::None,
        "main",
        "default",
        Expected::Err
    )]
    #[case( // 15
        "private-git-v1-tag-ssh",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Git,
        TestTlsConfig::None,
        TestAuthConfig::Ssh,
        "v1",
        "default",
        Expected::Ok
    )]
    #[case( // 16
        "private-https-v1-tag-basic",
        TestRepoVisibility::Private,
        TestRepoUriSchema::Https,
        TestTlsConfig::Ignore,
        TestAuthConfig::Basic,
        "v1",
        "default",
        Expected::Ok
    )]
    #[case( // 17
        "public-git-v1-tag-ssh",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Git,
        TestTlsConfig::None,
        TestAuthConfig::Ssh,
        "v1",
        "default",
        Expected::Ok
    )]
    #[case( // 18
        "public-https-v1-tag-anon",
        TestRepoVisibility::Public,
        TestRepoUriSchema::Https,
        TestTlsConfig::Ignore,
        TestAuthConfig::None,
        "v1",
        "default",
        Expected::Ok
    )]
    #[tokio::test]
    #[ignore = "needs docker"]
    async fn test_git_repo_fetch_ref_template(
        #[case] name: impl Into<String>,
        #[case] visibility: TestRepoVisibility,
        #[case] schema: TestRepoUriSchema,
        #[case] tls: TestTlsConfig,
        #[case] auth: TestAuthConfig,
        #[case] ref_name: impl Into<String>,
        #[case] ns: impl Into<String>,
        #[case] expected: Expected,
    ) {
    }

    #[apply(test_git_repo_fetch_ref_template)]
    async fn test_git_repo_fetch_ref(
        #[case] name: impl Into<String>,
        #[case] visibility: TestRepoVisibility,
        #[case] schema: TestRepoUriSchema,
        #[case] tls: TestTlsConfig,
        #[case] auth: TestAuthConfig,
        #[case] ref_name: impl Into<String>,
        #[case] ns: impl Into<String>,
        #[case] expected: Expected,
    ) {
        const SECRETS_NAMESPACE: &str = "default";

        let client = tests::get_test_kube_client().await.unwrap();
        let ref_name = ref_name.into();
        let ns = ns.into();
        let name: String = name.into();

        let repo_builder = TestGitRepoBuilder::build(
            client.clone(),
            format!("git-repo-{name}"),
            visibility,
            schema,
            tls,
            auth,
            Some(SECRETS_NAMESPACE.into()),
        )
        .await;

        let repo: GitRepo = repo_builder.clone().into();
        let path = tempfile::tempdir().unwrap();
        let path = String::from(path.path().to_str().unwrap());

        let repo = repo.fetch_repo_ref(client.clone(), &ref_name, &path, &ns).await;

        if repo.is_err() && expected != Expected::Err {
            eprintln!(
                "namespaced test: {name}, repo_builder={repo_builder:?}, err={}",
                repo.as_ref().err().unwrap()
            );
        }

        if repo.is_ok() && expected != Expected::Ok {
            eprintln!("namespaced test: {name}, repo_builder={repo_builder:?}, expecter Err, but got Ok",);
        }

        match expected {
            Expected::Ok => {
                assert!(repo.is_ok(), "Failed test name: {name}");
            }
            Expected::Err => {
                assert!(repo.is_err(), "Failed test name: {name}");
            }
        }
    }

    #[apply(test_git_repo_fetch_ref_template)]
    async fn test_cluster_git_repo_fetch_ref(
        #[case] name: impl Into<String>,
        #[case] visibility: TestRepoVisibility,
        #[case] schema: TestRepoUriSchema,
        #[case] tls: TestTlsConfig,
        #[case] auth: TestAuthConfig,
        #[case] ref_name: impl Into<String>,
        #[case] ns: impl Into<String>,
        #[case] expected: Expected,
    ) {
        const SECRETS_NAMESPACE: &str = "default";

        let client = tests::get_test_kube_client().await.unwrap();
        let ref_name = ref_name.into();
        let ns = ns.into();
        let name: String = name.into();

        let repo_builder = TestGitRepoBuilder::build(
            client.clone(),
            format!("cluster-git-repo-{name}"),
            visibility,
            schema,
            tls,
            auth,
            Some(SECRETS_NAMESPACE.into()),
        )
        .await;

        let repo: ClusterGitRepo = repo_builder.clone().into();
        let path = tempfile::tempdir().unwrap();
        let path = String::from(path.path().to_str().unwrap());

        let repo = repo.fetch_repo_ref(client.clone(), &ref_name, &path, &ns).await;

        if repo.is_err() && expected != Expected::Err {
            eprintln!(
                "cluster test: {name}, repo_builder={repo_builder:?}, err={}",
                repo.as_ref().err().unwrap()
            );
        }

        if repo.is_ok() && expected != Expected::Ok {
            eprintln!("cluster test: {name}, repo_builder={repo_builder:?}, expecter Err, but got Ok",);
        }

        match expected {
            Expected::Ok => {
                assert!(repo.is_ok(), "Failed test name: {name}");
            }
            Expected::Err => {
                assert!(repo.is_err(), "Failed test name: {name}");
            }
        }
    }
}
