use clap::Parser;
use tracing::debug;
use tracing_subscriber::{filter::LevelFilter, fmt, EnvFilter};

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct CliConfig {
    /// Enable extreme logging (debug)
    #[arg(short, long)]
    debug: bool,

    /// Enable additional logging (info)
    #[arg(short, long)]
    verbose: bool,

    /// Write logs in JSON format
    #[arg(short, long)]
    json_log: bool,

    /// Port to listen on for webhooks
    #[arg(long, short, value_parser=CliConfig::parse_tcp_port, default_value = "8080")]
    pub webhooks_port: u16,

    /// Port to listen on for utilities web
    #[arg(long, short, value_parser=CliConfig::parse_tcp_port, default_value = "3000")]
    pub utility_port: u16,
}

impl CliConfig {
    /// Constructs CLI config
    pub fn new() -> CliConfig {
        let config: CliConfig = Parser::parse();
        config.setup_logger();

        debug!("CLI config: {:#?}", config);

        config
    }

    /// Creates global logger and set requested log level and format
    fn setup_logger(&self) {
        let level_filter = if self.debug {
            LevelFilter::DEBUG
        } else if self.verbose {
            LevelFilter::INFO
        } else {
            LevelFilter::WARN
        };

        let log_filter = EnvFilter::from_default_env().add_directive(level_filter.into());
        let log_format = fmt::format().with_level(true).with_target(self.debug);

        let subscriber = tracing_subscriber::fmt().with_env_filter(log_filter);
        if self.json_log {
            subscriber
                .event_format(log_format.json().flatten_event(true))
                .init();
        } else {
            subscriber.event_format(log_format.compact()).init();
        };
    }

    /// Parse port number string into u16 and validate range
    fn parse_tcp_port(port: &str) -> Result<u16, String> {
        match port.parse::<u16>() {
            Ok(port) => {
                if port > 0 {
                    Ok(port)
                } else {
                    Err("health check port number should be in range 1..65535".into())
                }
            }
            Err(e) => Err(format!(
                "unable to parse `{port}`: {}, it should be number in range 1..65535",
                e
            )),
        }
    }
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            debug: false,
            verbose: false,
            json_log: false,
            webhooks_port: 8080,
            utility_port: 3000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_good_health_check_port() {
        assert_eq!(CliConfig::parse_tcp_port("123"), Ok(123));
        assert_eq!(CliConfig::parse_tcp_port("65535"), Ok(65535));
    }

    #[test]
    fn parse_wrong_health_check_port() {
        assert_eq!(
            CliConfig::parse_tcp_port("0"),
            Err("health check port number should be in range 1..65535".into())
        );
        assert_eq!(
            CliConfig::parse_tcp_port("65536"),
            Err("unable to parse `65536`: number too large to fit in target type, it should be number in range 1..65535".into())
        );
        assert_eq!(CliConfig::parse_tcp_port("qwerty"), Err("unable to parse `qwerty`: invalid digit found in string, it should be number in range 1..65535".into()));
    }
}
