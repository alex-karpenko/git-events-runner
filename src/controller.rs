use crate::{
    cli::CliConfig,
    resources::{
        trigger::{ScheduleTrigger, ScheduleTriggerSpec, TriggerSchedule},
        CustomApiResource, Reconcilable,
    },
    Error, Result,
};
use chrono::{DateTime, Utc};
use futures::{future::join_all, StreamExt};
use k8s_openapi::NamespaceResourceScope;
use kube::{
    api::ListParams,
    runtime::{
        controller::{self, Action as ReconcileAction},
        events::{Recorder, Reporter},
        finalizer::{finalizer, Event as Finalizer},
        watcher::Config,
        Controller,
    },
    Api, Client, Resource, ResourceExt,
};
use sacs::{
    scheduler::{
        GarbageCollector, RuntimeThreads, Scheduler, SchedulerBuilder, TaskScheduler, WorkerParallelism, WorkerType,
    },
    task::TaskId,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{clone::Clone, collections::HashMap, default::Default, fmt::Debug, sync::Arc, time::Duration};
use tokio::sync::{watch, RwLock};
use tracing::{debug, error, info, warn};

/// State shared between the controllers and the web server
#[derive(Clone)]
pub struct State {
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Web servers readiness
    pub ready: Arc<RwLock<bool>>,
    /// Cli config
    pub cli_config: Arc<CliConfig>,
}

impl State {
    pub fn new(cli_config: Arc<CliConfig>) -> Self {
        Self {
            diagnostics: Default::default(),
            ready: Default::default(),
            cli_config,
        }
    }
}
pub struct Diagnostics {
    pub last_event: DateTime<Utc>,
    pub reporter: Reporter,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "git-events-runner".into(),
        }
    }
}
impl Diagnostics {
    pub(crate) fn recorder<K>(&self, client: Client, res: &K) -> Recorder
    where
        K: Resource<DynamicType = ()>,
    {
        Recorder::new(client, self.reporter.clone(), res.object_ref(&()))
    }
}

// Context for our reconcilers.
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Scheduler to run periodic tasks of ScheduleTrigger (WebhookTrigger has its own scheduler)
    pub scheduler: Arc<RwLock<Scheduler>>,
    /// Actual state of all Triggers
    pub triggers: Arc<RwLock<TriggersState>>,
    /// Cli config
    pub cli_config: Arc<CliConfig>,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Create a Controller Context that can update State
    pub fn to_context(
        &self,
        client: Client,
        scheduler: Arc<RwLock<Scheduler>>,
        triggers: Arc<RwLock<TriggersState>>,
    ) -> Arc<Context> {
        Arc::new(Context {
            client,
            diagnostics: self.diagnostics.clone(),
            scheduler,
            triggers,
            cli_config: self.cli_config.clone(),
        })
    }
}

/// Actual (real world) schedule triggers state
#[derive(Default)]
pub struct TriggersState {
    /// All scheduled tasks
    pub(crate) tasks: HashMap<String, TaskId>,
    /// All specs of scheduled triggers
    // TODO: do we really need this specs? seems we have all resources cached
    pub(crate) schedules: HashMap<String, TriggerSchedule>,
}

/// Initialize the controllers and shared state (given the crd is installed)
pub async fn run_leader_controllers(
    client: Client,
    state: State,
    shutdown_channel: watch::Receiver<bool>,
    schedule_parallelism: usize,
) {
    info!("starting leader controllers");
    let scheduler = SchedulerBuilder::new()
        .garbage_collector(GarbageCollector::Immediate)
        .worker_type(WorkerType::MultiThread(RuntimeThreads::CpuCores))
        .parallelism(WorkerParallelism::Limited(schedule_parallelism))
        .build();
    let scheduler = Arc::new(RwLock::new(scheduler));

    // separate scope to release references in the shared state
    {
        // TODO: refactor to have single task since we have single reconcilable resource so far
        // but this should be don in separate PR to have history in case of possible revert
        let mut controllers = vec![];
        let triggers_state = Arc::new(RwLock::new(TriggersState::default()));
        let context = state.to_context(client.clone(), scheduler.clone(), triggers_state);

        // first controller to handle
        let triggers_api = Api::<ScheduleTrigger>::all(client.clone());
        check_api_by_list(&triggers_api, "Triggers").await;
        let mut shutdown = shutdown_channel.clone();
        controllers.push(tokio::task::spawn(
            Controller::new(triggers_api, Config::default().any_semantic())
                .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
                .graceful_shutdown_on(async move { shutdown.changed().await.unwrap_or(()) })
                .run(
                    reconcile_namespaced::<ScheduleTrigger, ScheduleTriggerSpec>,
                    error_policy::<ScheduleTrigger, ScheduleTriggerSpec>,
                    context.clone(),
                )
                .filter_map(|x| async move { x.ok() })
                .for_each(|_| futures::future::ready(())),
        ));

        debug!("starting controllers main loop");
        join_all(controllers).await;
        debug!("controllers main loop finished");
    }

    info!("shutting down ScheduleTriggers task scheduler");
    // we have to extract scheduler from shared ref (Arc) because shutdown method consumes it
    let scheduler = Arc::into_inner(scheduler)
        .expect("more than one copies of scheduler is present, looks like a BUG!")
        .into_inner();
    scheduler
        .shutdown(sacs::scheduler::ShutdownOpts::WaitForFinish)
        .await
        .unwrap_or(())
}

/// Call `List` on custom resource to verify if the required custom Api is present.
/// ATTENTION: It exits application if CRD isn't present.
async fn check_api_by_list<K>(api: &Api<K>, api_name: &str)
where
    K: Clone + DeserializeOwned + Debug,
{
    if let Err(e) = api.list(&ListParams::default().limit(1)).await {
        error!(api = %api_name, error = %e, "CRD is not queryable, looks like CRD isn't installed/updated?");
        info!("to install run: {} crds | kubectl apply -f -", env!("CARGO_PKG_NAME"));
        std::process::exit(1);
    }
}

/// Reconciliation errors handler
fn error_policy<K, S>(_resource: Arc<K>, error: &Error, _ctx: Arc<Context>) -> ReconcileAction
where
    K: Reconcilable<S>,
{
    warn!(%error, "reconcile failed");
    ReconcileAction::await_change()
}

/// Namespaced resources reconciler
async fn reconcile_namespaced<K, S>(resource: Arc<K>, ctx: Arc<Context>) -> Result<ReconcileAction>
where
    K: Resource + Reconcilable<S> + CustomApiResource,
    K: Debug + Clone + DeserializeOwned + Serialize,
    <K as Resource>::DynamicType: Default,
    K: Resource<Scope = NamespaceResourceScope>,
{
    let ns = resource.namespace().unwrap();
    let resource_api: Api<K> = Api::namespaced(ctx.client.clone(), &ns);

    debug!(kind = %resource.kind(), namespace = %ns, resource = %resource.name_any(), "reconciling");
    if let Some(finalizer_name) = resource.finalizer_name() {
        finalizer(&resource_api, finalizer_name, resource, |event| async {
            match event {
                Finalizer::Apply(resource) => resource.reconcile(ctx.clone()).await,
                Finalizer::Cleanup(resource) => resource.cleanup(ctx.clone()).await,
            }
        })
        .await
        .map_err(|e| Error::FinalizerError(Box::new(e)))
    } else {
        resource.reconcile(ctx.clone()).await
    }
}
