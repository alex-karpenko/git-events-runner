use git_events_runner::{
    cache::ApiCacheStore,
    cache::SecretsCache,
    cli::{Cli, CliConfig},
    config::RuntimeConfig,
    controller::{run_leader_controllers, State},
    jobs, leader,
    resources::{
        action::{Action, ClusterAction},
        git_repo::{ClusterGitRepo, GitRepo},
        trigger::{ScheduleTrigger, WebhookTrigger},
    },
    signals::SignalHandler,
    web,
};
use kube::{Client, CustomResourceExt};
use sacs::scheduler::{GarbageCollector, RuntimeThreads, SchedulerBuilder, WorkerParallelism, WorkerType};
use std::{sync::Arc, time::Duration};
use tokio::sync::{watch, RwLock};
use tracing::{error, info};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::new();

    match cli {
        Cli::Crds => generate_crds(),
        Cli::Run(cli_config) => run(cli_config).await,
    }
}

async fn run(cli_config: CliConfig) -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let identity = Uuid::new_v4().to_string();

    // detached runtime configuration task
    tokio::spawn(RuntimeConfig::init_and_watch(
        client.clone(),
        cli_config.config_map_name.clone(),
    ));
    tokio::spawn(ApiCacheStore::watch(client.clone())); // detached task for custom resources cache
    tokio::spawn(jobs::watch(client.clone(), identity.clone())); // detached task to watch on all our k8s jobs

    let state = State::new(Arc::new(cli_config.clone()), identity.clone());
    SecretsCache::init_cache(Duration::from_secs(cli_config.secrets_cache_time), client.clone());

    // create leader lease to run ScheduleTrigger controller in single instance only
    let (mut leader_lease_channel, leader_lease_task) = leader::leader_lease_handler(
        &identity,
        Some(client.default_namespace().to_string()),
        &cli_config.leader_lease_name,
        cli_config.leader_lease_duration,
        cli_config.leader_lease_grace,
    )
    .await?;

    let mut signal_handler = SignalHandler::new().expect("unable to create signal handler");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut shutdown = false;

    // create and run (as detached task) web server for readiness/liveness probes and metrics
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
            identity.clone(),
        )
        .await
    };
    let hooks_web = tokio::spawn(hooks_web);

    // loop watches for leader changes and shutdown "signal"
    // if we become a leader - start controller to serve ScheduleTrigger,
    // and stop it if we lost leadership.
    while !shutdown {
        let (changed_tx, changed_rx) = watch::channel(false);
        let is_leader = leader_lease_channel.borrow_and_update().is_current_for(&identity);
        let controllers = if is_leader {
            info!("Leader lock has been acquired, identity={identity}");
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
                        error!("Error while sending shutdown event: {err}");
                    }
                    shutdown = true;
                },
            _ = async {
                    while leader_lease_channel.borrow_and_update().is_current_for(&identity) == is_leader {
                        if let Err(err) = leader_lease_channel.changed().await {
                            error!("Error while process leader lock changes: {err}");
                            shutdown = true;
                        }
                    }
                } => {},
        }

        // if we have any controller running now - sent shutdown message to it
        if let Some(controllers) = controllers {
            if let Err(err) = changed_tx.send(true) {
                error!("Unable to send change event: {err}");
            }
            // and wait for finish of controller
            let _ = tokio::join!(controllers);
            info!("Leader lock has been released");
        }
    }

    // when we got shutdown message or signal and exit from the previous loop
    // free leader lease
    drop(leader_lease_channel);
    // and wait for finish of all tasks
    let _ = tokio::join!(leader_lease_task, utils_web, hooks_web);

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
