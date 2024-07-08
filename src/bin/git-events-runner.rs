use git_events_runner::{
    cache::{ApiCacheStore, SecretsCache},
    cli::{Cli, CliConfig, CliConfigDumpOptions},
    config::RuntimeConfig,
    controller::{run_leader_controllers, State},
    jobs::JobsQueue,
    resources::{
        action::{Action, ClusterAction},
        git_repo::{ClusterGitRepo, GitRepo},
        trigger::{ScheduleTrigger, WebhookTrigger},
    },
    signals::SignalHandler,
    web,
};
use kube::{Client, CustomResourceExt};
use kube_lease_manager::LeaseManagerBuilder;
use opentelemetry_otlp::WithExportConfig;
use sacs::scheduler::{GarbageCollector, RuntimeThreads, SchedulerBuilder, WorkerParallelism, WorkerType};
use std::{sync::Arc, time::Duration};
use tokio::sync::{watch, RwLock};
use tracing::{error, info, instrument};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
use uuid::Uuid;

const OPENTELEMETRY_ENDPOINT_URL_ENV_NAME: &str = "OPENTELEMETRY_ENDPOINT_URL";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing();

    match Cli::new() {
        Cli::Crds => generate_crds(),
        Cli::Config(options) => generate_config_yaml(options),
        Cli::Run(cli_config) => run(cli_config).await,
    }
}

#[instrument("controller", skip_all)]
async fn run(cli_config: CliConfig) -> anyhow::Result<()> {
    let client = Client::try_default().await?;
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
        web::build_hooks_web(
            client.clone(),
            shutdown_rx.clone(),
            scheduler,
            cli_config.webhooks_port,
            cli_config.source_clone_folder.clone(),
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

    Ok(())
}

/// Just print put all CRD definitions
fn generate_crds() -> anyhow::Result<()> {
    let crds = vec![
        serde_yaml::to_string(&GitRepo::crd()).unwrap(),
        serde_yaml::to_string(&ClusterGitRepo::crd()).unwrap(),
        serde_yaml::to_string(&ScheduleTrigger::crd()).unwrap(),
        serde_yaml::to_string(&WebhookTrigger::crd()).unwrap(),
        serde_yaml::to_string(&Action::crd()).unwrap(),
        serde_yaml::to_string(&ClusterAction::crd()).unwrap(),
    ];

    for crd in crds {
        print!("---\n{crd}");
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
fn setup_tracing() {
    let env_filter = EnvFilter::try_from_default_env()
        .or(EnvFilter::try_new("info"))
        .unwrap();

    let logger = tracing_subscriber::fmt::layer().compact().with_target(true);
    let collector = Registry::default().with(env_filter).with(logger);

    if let Some(tracer) = get_tracer() {
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
        let collector = collector.with(telemetry);
        tracing::subscriber::set_global_default(collector).unwrap();
    } else {
        tracing::subscriber::set_global_default(collector).unwrap();
    }
}

/// Get OTLP tracer if endpoint env var is defined.
fn get_tracer() -> Option<opentelemetry_sdk::trace::Tracer> {
    if let Ok(otlp_endpoint) = std::env::var(OPENTELEMETRY_ENDPOINT_URL_ENV_NAME) {
        let otlp_exporter = opentelemetry_otlp::new_exporter().tonic().with_endpoint(otlp_endpoint);
        let trace_config = opentelemetry_sdk::trace::config().with_resource(opentelemetry_sdk::Resource::new(vec![
            opentelemetry::KeyValue::new("service.name", env!("CARGO_PKG_NAME")),
        ]));

        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(otlp_exporter)
            .with_trace_config(trace_config)
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .unwrap();

        Some(tracer)
    } else {
        None
    }
}
