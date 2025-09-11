//! Main controller app.
#![deny(unsafe_code, warnings, missing_docs)]
use kube::Client;
use kube_lease_manager::LeaseManagerBuilder;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::SdkTracerProvider;
use rustls::crypto::aws_lc_rs;
use sacs::scheduler::{GarbageCollector, RuntimeThreads, SchedulerBuilder, WorkerParallelism, WorkerType};
use std::{sync::Arc, time::Duration};
use tokio::sync::{RwLock, watch};
use tracing::{error, info, instrument};
use tracing_subscriber::{EnvFilter, Layer, Registry, layer::SubscriberExt};
use uuid::Uuid;

use git_events_runner::{
    cache::{ApiCacheStore, SecretsCache},
    cli::{Cli, CliConfig, CliConfigDumpOptions},
    config::RuntimeConfig,
    controller::{State, run_leader_controllers},
    jobs::JobsQueue,
    resources,
    signals::SignalHandler,
    web::{self, RequestRateLimitsConfig},
};

const OPENTELEMETRY_ENDPOINT_URL_ENV_NAME: &str = "OPENTELEMETRY_ENDPOINT_URL";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    match Cli::new() {
        Cli::Crds => generate_crds(),
        Cli::Config(options) => generate_config_yaml(options),
        Cli::Run(cli_config) => {
            let client = Client::try_default().await?;
            run(cli_config, client).await
        }
    }
}

#[instrument("controller", skip_all)]
async fn run(cli_config: CliConfig, client: Client) -> anyhow::Result<()> {
    let trace_provider = setup_tracing(cli_config.json_logs);
    let tls_config = cli_config.build_tls_config(client.clone()).await?;
    let identity = Uuid::new_v4().to_string();

    // detached runtime configuration task
    tokio::spawn(RuntimeConfig::init_and_watch(
        client.clone(),
        cli_config.config_map_name.clone(),
    ));
    tokio::spawn(ApiCacheStore::watch(client.clone())); // detached task for custom resources cache

    let state = State::new(Arc::new(cli_config.source_clone_folder.clone()));
    SecretsCache::init_cache(Duration::from_secs(cli_config.secrets_cache_time), client.clone());

    // create leader lease to run ScheduleTrigger controller in single instance only
    let lease_manager = LeaseManagerBuilder::new(client.clone(), cli_config.leader_lease_name)
        .with_identity(identity.clone())
        .with_namespace(client.default_namespace().to_string())
        .with_duration(cli_config.leader_lease_duration)
        .with_grace(cli_config.leader_lease_grace)
        .build()
        .await?;
    let (mut leader_lease_channel, leader_lease_task) = lease_manager.watch().await;

    let mut signal_handler = SignalHandler::new().expect("unable to create signal handler");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut shutdown = false;

    // create a detached task to watch on all our k8s jobs
    let jobs_queue = tokio::spawn(JobsQueue::init_and_watch(
        client.clone(),
        identity.clone(),
        shutdown_rx.clone(),
    ));

    // create and run (as a detached task) web server for readiness/liveness probes and metrics
    let utils_web = web::build_utils_web(state.clone(), shutdown_rx.clone(), cli_config.utility_port).await;
    let utils_web = tokio::spawn(utils_web);

    // create web server for serve requests to WebhookTrigger resources
    let hooks_web = {
        // it uses its own scheduler
        let scheduler = SchedulerBuilder::new()
            .garbage_collector(GarbageCollector::Immediate)
            .worker_type(WorkerType::MultiThread(RuntimeThreads::CpuCores))
            .parallelism(WorkerParallelism::Limited(cli_config.webhooks_parallelism as usize))
            .build();
        let scheduler = Arc::new(RwLock::new(scheduler));
        let request_rate_limits = RequestRateLimitsConfig {
            global: cli_config.hooks_rrl_global,
            trigger: cli_config.hooks_rrl_trigger,
            source: cli_config.hooks_rrl_source,
        };

        web::build_hooks_web(
            client.clone(),
            shutdown_rx.clone(),
            scheduler,
            cli_config.webhooks_port,
            cli_config.source_clone_folder.clone(),
            request_rate_limits,
            tls_config,
        )
        .await
    };
    let hooks_web = tokio::spawn(hooks_web);

    // loop watches for leader changes and shutdown "signal"
    // if we become a leader - start controller to serve ScheduleTrigger,
    // and stop it if we lost leadership.
    while !shutdown {
        let (changed_tx, changed_rx) = watch::channel(false);
        let is_leader = *leader_lease_channel.borrow_and_update();
        let controllers = if is_leader {
            info!(%identity, "leader lock has been acquired");
            Some(tokio::spawn(run_leader_controllers(
                client.clone(),
                state.clone(),
                changed_rx.clone(),
                cli_config.schedule_parallelism as usize,
            )))
        } else {
            None
        };

        // set global readiness state to true
        {
            let mut ready = state.ready.write().await;
            *ready = true;
        }

        // wait fo one of:
        // - signal - start shutdown process
        // - leader changed - start or stop controller
        tokio::select! {
            _ = signal_handler.wait_for_signal() => {
                    if let Err(err) = shutdown_tx.send(true) {
                        error!(error = %err, "sending shutdown event");
                    }
                    shutdown = true;
                },
            _ = async {
                    while *leader_lease_channel.borrow_and_update() == is_leader {
                        if let Err(err) = leader_lease_channel.changed().await {
                            error!(error = %err, "processing leader lock change");
                            shutdown = true;
                        }
                    }
                } => {},
        }

        // if we have any controller running now, send a shutdown message to it
        if let Some(controllers) = controllers {
            if let Err(err) = changed_tx.send(true) {
                error!(error = %err, "sending change event");
            }
            // and wait for finish of controller
            let _ = tokio::join!(controllers);
            info!("leader lock has been released");
        }
    }

    // when we got shut down message or signal and exit from the previous loop,
    // free leader lease
    drop(leader_lease_channel);
    // and wait for finishing of all tasks
    let _ = tokio::join!(leader_lease_task, utils_web, hooks_web, jobs_queue);
    if let Some(trace_provider) = trace_provider {
        trace_provider.shutdown()?;
    }

    Ok(())
}

/// Just print all CRD definitions
fn generate_crds() -> anyhow::Result<()> {
    for crd in resources::get_all_crds() {
        print!("---\n{}", serde_yaml_ng::to_string(&crd).unwrap());
    }

    Ok(())
}

/// Print default dynamic config to stdout as YAML
fn generate_config_yaml(options: CliConfigDumpOptions) -> anyhow::Result<()> {
    let opts = if options.helm_template {
        git_events_runner::config::YamlConfigOpts::HelmTemplate
    } else {
        git_events_runner::config::YamlConfigOpts::Raw
    };

    let yaml_str = RuntimeConfig::default_as_yaml_string(opts)?;
    print!("{yaml_str}");

    Ok(())
}

/// Creates global logger, tracer and set requested log level and format
fn setup_tracing(json_logs: bool) -> Option<SdkTracerProvider> {
    let env_filter = EnvFilter::try_from_default_env()
        .or(EnvFilter::try_new("info"))
        .unwrap();

    let collector = Registry::default().with(get_logger_layer(json_logs)).with(env_filter);

    let trace_provider = get_trace_provider();
    if let Some(tracer) = trace_provider.clone() {
        let tracer = tracer.tracer(env!("CARGO_PKG_NAME"));
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        let collector = collector.with(telemetry);
        tracing::subscriber::set_global_default(collector).unwrap();
    } else {
        tracing::subscriber::set_global_default(collector).unwrap();
    }

    trace_provider
}

fn get_logger_layer(json_logs: bool) -> impl Layer<Registry> {
    let fmt_layer = tracing_subscriber::fmt::layer();

    if json_logs {
        fmt_layer
            .json()
            .with_target(true)
            .flatten_event(true)
            .with_span_list(false)
            .with_current_span(false)
            .boxed()
    } else {
        fmt_layer.compact().with_target(true).boxed()
    }
}

/// Get OTLP tracer if endpoint env var is defined.
fn get_trace_provider() -> Option<SdkTracerProvider> {
    if let Ok(otlp_endpoint) = std::env::var(OPENTELEMETRY_ENDPOINT_URL_ENV_NAME) {
        let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(otlp_endpoint)
            .build()
            .unwrap();

        let tracer = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(otlp_exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_attribute(opentelemetry::KeyValue::new("service.name", env!("CARGO_PKG_NAME")))
                    .build(),
            )
            .build();

        Some(tracer)
    } else {
        None
    }
}
