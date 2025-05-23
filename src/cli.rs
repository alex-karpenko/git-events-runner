//! Cli parameters for git-events-runner binary
use anyhow::Context;
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};
use std::sync::OnceLock;
use tracing::debug;

use crate::{web::RequestsRateLimitParams, Error};

const DEFAULT_SOURCE_CLONE_FOLDER: &str = "/tmp/git-events-runner";
const DEFAULT_CONFIG_MAP_NAME: &str = "git-events-runner-config";
const DEFAULT_LEADER_LOCK_LEASE_NAME: &str = "git-events-runner-leader-lock";
const DEFAULT_METRICS_PREFIX: &str = "git_events_runner";

static CLI_CONFIG: OnceLock<CliConfig> = OnceLock::new();

/// Root CLI commands
#[derive(Parser, Debug)]
#[command(author, version, about)]
#[allow(clippy::large_enum_variant)]
pub enum Cli {
    /// Print CRD definitions to stdout
    Crds,
    /// Print default dynamic config YAML to stdout
    Config(CliConfigDumpOptions),
    /// Run K8s controller
    Run(CliConfig),
}

/// Parameters for the `run` subcommand
#[derive(Parser, Debug, Clone)]
pub struct CliConfig {
    /// Port to listen on for webhooks
    #[arg(long, short, value_parser=clap::value_parser!(u16).range(1..), default_value = "8080")]
    pub webhooks_port: u16,

    /// Port to listen on for utilities web
    #[arg(long, short, value_parser=clap::value_parser!(u16).range(1..), default_value = "3000")]
    pub utility_port: u16,

    /// Maximum number of webhook triggers running in parallel
    #[arg(long, value_parser=clap::value_parser!(u16).range(1..256), default_value = "16")]
    pub webhooks_parallelism: u16,

    /// Maximum number of schedule triggers running in parallel
    #[arg(long, value_parser=clap::value_parser!(u16).range(1..256), default_value = "16")]
    pub schedule_parallelism: u16,

    /// Seconds to cache secrets for
    #[arg(long, value_parser=clap::value_parser!(u64).range(1..), default_value = "60")]
    pub secrets_cache_time: u64,

    /// Path (within container) to clone repo to
    #[arg(long, default_value = DEFAULT_SOURCE_CLONE_FOLDER)]
    pub source_clone_folder: String,

    /// Name of the ConfigMap with dynamic controller config
    #[arg(long, default_value = DEFAULT_CONFIG_MAP_NAME)]
    pub config_map_name: String,

    /// Name of the Lease for leader locking
    #[arg(long, default_value = DEFAULT_LEADER_LOCK_LEASE_NAME)]
    pub leader_lease_name: String,

    /// Leader lease duration, seconds
    #[arg(long, value_parser=clap::value_parser!(u64).range(1..301), default_value = "30")]
    pub leader_lease_duration: u64,

    /// Leader lease grace interval, seconds
    #[arg(long, value_parser=clap::value_parser!(u64).range(1..301), default_value = "5")]
    pub leader_lease_grace: u64,

    /// Name of the ConfigMap with dynamic controller config
    #[arg(long, default_value = DEFAULT_METRICS_PREFIX)]
    pub metrics_prefix: String,

    /// Global webhooks requests rate limit (burst limit/seconds)
    #[arg(long, value_parser=Cli::rrl_parser)]
    pub hooks_rrl_global: Option<RequestsRateLimitParams>,

    /// Requests rate limit (burst limit/seconds) per webhook trigger
    #[arg(long, value_parser=Cli::rrl_parser)]
    pub hooks_rrl_trigger: Option<RequestsRateLimitParams>,

    /// Requests rate limit (burst limit/seconds) per webhook source
    #[arg(long, value_parser=Cli::rrl_parser)]
    pub hooks_rrl_source: Option<RequestsRateLimitParams>,

    /// Path to the TLS certificate file
    #[arg(long, requires = "tls_key_path", conflicts_with_all=["tls_secret_name", "tls_secret_namespace"])]
    pub tls_cert_path: Option<String>,

    /// Path to the TLS key file
    #[arg(long, requires = "tls_cert_path", conflicts_with_all=["tls_secret_name", "tls_secret_namespace"])]
    pub tls_key_path: Option<String>,

    /// Name of the Secret with TLS certificate and key
    #[arg(long, conflicts_with_all = ["tls_cert_path", "tls_key_path"])]
    pub tls_secret_name: Option<String>,

    /// Namespace of the TLS secret
    #[arg(long, requires = "tls_secret_name", conflicts_with_all = ["tls_cert_path", "tls_key_path"])]
    pub tls_secret_namespace: Option<String>,

    /// Output logs in JSON format
    #[arg(long, short = 'j')]
    pub json_logs: bool,
}

/// Parameters for the `config` subcommand
#[derive(Parser, Debug, Clone)]
pub struct CliConfigDumpOptions {
    /// Include some templates for Helm chart
    #[arg(short = 't', long)]
    pub helm_template: bool,
}

impl Cli {
    /// Constructs CLI config
    #[allow(clippy::new_without_default)]
    pub fn new() -> Cli {
        let cli: Cli = Parser::parse();
        debug!(config = ?cli, "creating cli config");

        // If we run controller - set shared CLiConfig instance
        if let Cli::Run(config) = cli {
            CLI_CONFIG.set(config.clone()).unwrap();
            Cli::Run(config)
        } else {
            cli
        }
    }

    /// Parse RRL params using original try_from(&str)
    fn rrl_parser(s: &str) -> Result<RequestsRateLimitParams, String> {
        RequestsRateLimitParams::try_from(s).map_err(|e| e.to_string())
    }
}

impl CliConfig {
    /// Returns really parsed CLI parameters or test mock with defaults
    pub fn get() -> &'static Self {
        #[cfg(not(test))]
        {
            CLI_CONFIG.get().unwrap()
        }

        #[cfg(test)]
        {
            CLI_CONFIG.get_or_init(|| CliConfig::parse_from::<_, &String>(&[]))
        }
    }

    /// Returns ready to use RustlsConfig instance if TLS config options were provided,
    /// or None if no config
    pub async fn build_tls_config(&self, client: Client) -> anyhow::Result<Option<RustlsConfig>> {
        match (
            self.tls_cert_path.as_ref(),
            self.tls_key_path.as_ref(),
            self.tls_secret_name.as_ref(),
        ) {
            (Some(cert), Some(key), _) => {
                let config = RustlsConfig::from_pem_file(cert, key)
                    .await
                    .context(format!("Unable to apply TLS config with cert={cert} and key={key}"))?;
                Ok(Some(config))
            }
            (_, _, Some(secret_name)) => {
                let secret_api: Api<Secret> = if let Some(namespace) = self.tls_secret_namespace.as_ref() {
                    Api::namespaced(client, namespace)
                } else {
                    Api::default_namespaced(client)
                };
                let secret = secret_api
                    .get(secret_name)
                    .await
                    .context(format!("Unable to get TLS secret {secret_name}"))?;
                let secret_data = secret.data.ok_or_else(|| {
                    Error::SecretDecodingError(format!("no `data` part in the secret `{secret_name}`"))
                })?;
                let cert = secret_data
                    .get("tls.crt")
                    .ok_or_else(|| {
                        Error::SecretDecodingError(format!("no `tls.crt` key in the secret `{secret_name}`"))
                    })?
                    .to_owned();
                let cert = String::from_utf8(cert.0)
                    .context("TLS cert is not a valid UTF-8 string")?
                    .into_bytes();

                let key = secret_data
                    .get("tls.key")
                    .ok_or_else(|| {
                        Error::SecretDecodingError(format!("no `tls.key` key in the secret `{secret_name}`"))
                    })?
                    .to_owned();
                let key = String::from_utf8(key.0)
                    .context("TLS key is not a valid UTF-8 string")?
                    .into_bytes();

                let config = RustlsConfig::from_pem(cert, key).await?;
                Ok(Some(config))
            }
            (_, _, _) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests;

    use insta::assert_debug_snapshot;
    use k8s_openapi::{api::core::v1::Namespace, ByteString};
    use kube::{
        api::{DeleteParams, ObjectMeta, PostParams},
        Resource as _,
    };
    use std::{collections::BTreeMap, env};
    use tokio::{fs::File, io::AsyncReadExt, sync::OnceCell};

    const NAMESPACE: &str = "secret-tls-test";
    const TEST_CERTIFICATES_SECRET_NAME: &str = "test-tls-certificates";
    const SERVER_CERT_BUNDLE: &str = "/test-server.pem";
    const SERVER_END_CERT: &str = "/end.crt";
    const SERVER_PRIVATE_KEY: &str = "/test-server.key";

    static INITIALIZED: OnceCell<()> = OnceCell::const_new();

    async fn init(client: Client) {
        INITIALIZED
            .get_or_init(|| async {
                tests::init_crypto_provider().await;
                create_namespace(client).await;
            })
            .await;
    }

    /// Unattended namespace creation
    async fn create_namespace(client: Client) {
        let api = Api::<Namespace>::all(client);
        let pp = PostParams::default();

        let mut data = Namespace::default();
        data.meta_mut().name = Some(String::from(NAMESPACE));
        api.create(&pp, &data).await.unwrap_or_default();
    }

    #[test]
    fn default_cli_consistency() {
        let cli = CliConfig::parse_from::<_, &str>([]);
        assert_debug_snapshot!(cli);
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn tls_config_nothing() {
        tests::init_crypto_provider().await;
        let client = tests::get_test_kube_client().await.unwrap();

        let cli = CliConfig::parse_from::<_, &str>([]);
        let config = cli.build_tls_config(client).await.unwrap();
        assert!(config.is_none());
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn tls_config_cert_key() {
        let client = tests::get_test_kube_client().await.unwrap();
        init(client.clone()).await;

        let out_dir = env::var("OUT_DIR").unwrap();
        let cert_bundle_path = format!("{out_dir}/tls/{SERVER_CERT_BUNDLE}");
        let cert_path = format!("{out_dir}/tls/{SERVER_END_CERT}");
        let key_path = format!("{out_dir}/tls/{SERVER_PRIVATE_KEY}");

        let cli = CliConfig::parse_from::<_, &str>([
            "run",
            "--tls-cert-path",
            cert_bundle_path.as_str(),
            "--tls-key-path",
            key_path.as_str(),
        ]);
        let config = cli.build_tls_config(client.clone()).await.unwrap();
        assert!(config.is_some());

        let cli = CliConfig::parse_from::<_, &str>([
            "run",
            "--tls-cert-path",
            cert_path.as_str(),
            "--tls-key-path",
            key_path.as_str(),
        ]);
        let config = cli.build_tls_config(client).await.unwrap();
        assert!(config.is_some());
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn tls_config_secret() {
        let client = tests::get_test_kube_client().await.unwrap();
        init(client.clone()).await;

        let out_dir = env::var("OUT_DIR").unwrap();
        let cert_bundle_path = format!("{out_dir}/tls/{SERVER_CERT_BUNDLE}");
        let cert_path = format!("{out_dir}/tls/{SERVER_END_CERT}");
        let key_path = format!("{out_dir}/tls/{SERVER_PRIVATE_KEY}");

        let cli = CliConfig::parse_from::<_, &str>([
            "run",
            "--tls-secret-name",
            TEST_CERTIFICATES_SECRET_NAME,
            "--tls-secret-namespace",
            NAMESPACE,
        ]);
        let secret_api: Api<Secret> = Api::namespaced(client.clone(), NAMESPACE);

        // Delete possible leftovers from the previous run
        let _ = secret_api
            .delete(TEST_CERTIFICATES_SECRET_NAME, &DeleteParams::default())
            .await;

        // Test with bundle
        let mut cert = vec![];
        let mut key = vec![];
        File::open(cert_bundle_path)
            .await
            .unwrap()
            .read_to_end(&mut cert)
            .await
            .unwrap();
        File::open(key_path).await.unwrap().read_to_end(&mut key).await.unwrap();

        let cert: ByteString = ByteString(cert);
        let key: ByteString = ByteString(key);

        let secret = Secret {
            type_: Some("kubernetes.io/tls".to_string()),
            metadata: ObjectMeta {
                name: Some(TEST_CERTIFICATES_SECRET_NAME.to_string()),
                namespace: Some(NAMESPACE.to_string()),
                ..ObjectMeta::default()
            },
            data: Some(BTreeMap::from([
                ("tls.crt".to_string(), cert),
                ("tls.key".to_string(), key.clone()),
            ])),
            ..Secret::default()
        };

        secret_api.create(&PostParams::default(), &secret).await.unwrap();

        let config = cli.build_tls_config(client.clone()).await.unwrap();
        assert!(config.is_some());

        secret_api
            .delete(TEST_CERTIFICATES_SECRET_NAME, &DeleteParams::default())
            .await
            .unwrap();

        // Test end certificate
        let mut cert = vec![];
        File::open(cert_path)
            .await
            .unwrap()
            .read_to_end(&mut cert)
            .await
            .unwrap();

        let cert: ByteString = ByteString(cert);

        let secret = Secret {
            type_: Some("kubernetes.io/tls".to_string()),
            metadata: ObjectMeta {
                name: Some(TEST_CERTIFICATES_SECRET_NAME.to_string()),
                namespace: Some(NAMESPACE.to_string()),
                ..ObjectMeta::default()
            },
            data: Some(BTreeMap::from([
                ("tls.crt".to_string(), cert),
                ("tls.key".to_string(), key),
            ])),
            ..Secret::default()
        };

        secret_api.create(&PostParams::default(), &secret).await.unwrap();

        let config = cli.build_tls_config(client).await.unwrap();
        assert!(config.is_some());

        secret_api
            .delete(TEST_CERTIFICATES_SECRET_NAME, &DeleteParams::default())
            .await
            .unwrap();
    }
}
