use clap::Parser;
use std::sync::OnceLock;
use tracing::debug;

const DEFAULT_SOURCE_CLONE_FOLDER: &str = "/tmp/git-events-runner";
const DEFAULT_CONFIG_MAP_NAME: &str = "git-events-runner-config";
const DEFAULT_LEADER_LOCK_LEASE_NAME: &str = "git-events-runner-leader-lock";
const DEFAULT_METRICS_PREFIX: &str = "git_events_runner";

static CLI_CONFIG: OnceLock<CliConfig> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub enum Cli {
    /// Print CRD definitions to stdout
    Crds,
    /// Print default dynamic config YAML to stdout
    Config(CliConfigDumpOptions),
    /// Run K8s controller
    Run(CliConfig),
}

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
    // /// Write logs in JSON format
    // #[arg(short, long)]
    // json_log: bool,
}

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
}

impl CliConfig {
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
