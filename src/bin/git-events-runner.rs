use git_events_runner::{
    cache::CacheStore,
    cli::{Cli, CliConfig},
    config::RuntimeConfig,
    controller::{run_leader_controllers, State},
    leader_lock,
    resources::{
        action::{Action, ClusterAction},
        git_repo::{ClusterGitRepo, GitRepo},
        trigger::{ScheduleTrigger, WebhookTrigger},
    },
    secrets_cache::ExpiringSecretCache,
    signals::SignalHandler,
    web,
};
use kube::{Client, CustomResourceExt};
use sacs::scheduler::{
    GarbageCollector, RuntimeThreads, SchedulerBuilder, WorkerParallelism, WorkerType,
};
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
    tokio::spawn(RuntimeConfig::init_and_watch(
        client.clone(),
        cli_config.config_map_name.clone(),
    ));
    tokio::spawn(CacheStore::watch(client.clone()));
    let secrets_cache = ExpiringSecretCache::new(
        Duration::from_secs(cli_config.secrets_cache_time),
        client.clone(),
    );
    let state = State::new(Arc::new(cli_config.clone()), secrets_cache.clone());
    let identity = Uuid::new_v4().to_string();

    let (mut lock_channel, lock_task) = leader_lock::new(
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

    let utils_web =
        web::build_utils_web(state.clone(), shutdown_rx.clone(), cli_config.utility_port).await;
    let utils_web = tokio::spawn(utils_web);

    let hooks_web = {
        let scheduler = SchedulerBuilder::new()
            .garbage_collector(GarbageCollector::Immediate)
            .worker_type(WorkerType::MultiThread(RuntimeThreads::CpuCores))
            .parallelism(WorkerParallelism::Limited(
                cli_config.webhooks_parallelism as usize,
            ))
            .build();
        let scheduler = Arc::new(RwLock::new(scheduler));
        web::build_hooks_web(
            client.clone(),
            shutdown_rx.clone(),
            scheduler,
            secrets_cache.clone(),
            cli_config.webhooks_port,
            cli_config.source_clone_folder.clone(),
        )
        .await
    };
    let hooks_web = tokio::spawn(hooks_web);

    while !shutdown {
        let (changed_tx, changed_rx) = watch::channel(false);
        let is_leader = lock_channel.borrow_and_update().is_current_for(&identity);
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

        {
            let mut ready = state.ready.write().await;
            *ready = true;
        }

        tokio::select! {
            _ = signal_handler.wait_for_signal() => {
                    if let Err(err) = shutdown_tx.send(true) {
                        error!("Error while sending shutdown event: {err}");
                    }
                    shutdown = true;
                },
            _ = async {
                    while lock_channel.borrow_and_update().is_current_for(&identity) == is_leader {
                        if let Err(err) = lock_channel.changed().await {
                            error!("Error while process leader lock changes: {err}");
                            shutdown = true;
                        }
                    }
                } => {},
        }

        if let Some(controllers) = controllers {
            if let Err(err) = changed_tx.send(true) {
                error!("Unable to send change event: {err}");
            }
            let _ = tokio::join!(controllers);
            info!("Leader lock has been released");
        }
    }

    drop(lock_channel);
    let _ = tokio::join!(lock_task, utils_web, hooks_web);

    Ok(())
}

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
