use clap::Parser;
use tracing::debug;
use tracing_subscriber::{filter::LevelFilter, fmt, EnvFilter};

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub enum Cli {
    /// Print CRD definitions to stdout
    Crds,
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

    /// Seconds to cache secrets
    #[arg(long, default_value = "30")]
    pub secrets_cache_time: u64,

    /// Enable extreme logging (debug)
    #[arg(short, long)]
    debug: bool,

    /// Enable additional logging (info)
    #[arg(short, long)]
    verbose: bool,
    // /// Write logs in JSON format
    // #[arg(short, long)]
    // json_log: bool,
}

impl Cli {
    /// Constructs CLI config
    pub fn new() -> Cli {
        let cli: Cli = Parser::parse();
        cli.setup_logger();

        debug!("CLI config: {:#?}", cli);

        cli
    }

    /// Creates global logger and set requested log level and format
    fn setup_logger(&self) {
        if let Self::Run(config) = self {
            let level_filter = if config.debug {
                LevelFilter::DEBUG
            } else if config.verbose {
                LevelFilter::INFO
            } else {
                LevelFilter::WARN
            };

            let log_filter = EnvFilter::from_default_env().add_directive(level_filter.into());
            let log_format = fmt::format().with_level(true).with_target(config.debug);

            let subscriber = tracing_subscriber::fmt().with_env_filter(log_filter);
            // if config.json_log {
            //     subscriber
            //         .event_format(log_format.json().flatten_event(true))
            //         .init();
            // } else {
            subscriber.event_format(log_format.compact()).init();
            // };
        }
    }
}

impl Default for Cli {
    fn default() -> Self {
        Self::Run(CliConfig {
            webhooks_port: 8080,
            utility_port: 3000,
            webhooks_parallelism: 16,
            schedule_parallelism: 16,
            secrets_cache_time: 60,
            debug: false,
            verbose: false,
        })
    }
}
