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

    /// Path to TLS certificate file
    #[arg(long, requires = "tls_key", conflicts_with_all=["tls_secret_name", "tls_secret_namespace"])]
    pub tls_cert: Option<String>,

    /// Path to TLS key file
    #[arg(long, requires = "tls_cert", conflicts_with_all=["tls_secret_name", "tls_secret_namespace"])]
    pub tls_key: Option<String>,

    /// Secret name with TLS certificate and key
    #[arg(long, conflicts_with_all = ["tls_cert", "tls_key"])]
    pub tls_secret_name: Option<String>,

    /// Namespace of the  TLS secret
    #[arg(long, requires = "tls_secret_name", conflicts_with_all = ["tls_cert", "tls_key"])]
    pub tls_secret_namespace: Option<String>,
    /* /// Write logs in JSON format
    #[arg(short, long)]
    json_log: bool, */
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
            self.tls_cert.as_ref(),
            self.tls_key.as_ref(),
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
                    Error::SecretDecodingError(format!("no `data` part in the secret `{}`", secret_name))
                })?;
                let cert = secret_data
                    .get("tls.crt")
                    .ok_or_else(|| {
                        Error::SecretDecodingError(format!("no `tls.crt` key in the secret `{}`", secret_name))
                    })?
                    .to_owned();
                let cert = String::from_utf8(cert.0)
                    .context("TLS cert is not a valid UTF-8 string")?
                    .into_bytes();

                let key = secret_data
                    .get("tls.key")
                    .ok_or_else(|| {
                        Error::SecretDecodingError(format!("no `tls.key` key in the secret `{}`", secret_name))
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
    use insta::assert_debug_snapshot;

    use super::*;

    #[test]
    fn default_cli_consistency() {
        let cli = CliConfig::parse_from::<_, &str>([]);
        assert_debug_snapshot!(cli);
    }
}
