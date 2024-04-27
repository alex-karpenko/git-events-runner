use crate::{
    config::CliConfig,
    resources::trigger::{
        ScheduleTrigger, ScheduleTriggerSpec, TriggerStatus, WebhookTrigger, WebhookTriggerSpec,
    },
    secrets_cache::ExpiringSecretCache,
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
        GarbageCollector, RuntimeThreads, Scheduler, SchedulerBuilder, TaskScheduler,
        WorkerParallelism, WorkerType,
    },
    task::TaskId,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    clone::Clone, collections::HashMap, default::Default, fmt::Debug, sync::Arc, time::Duration,
};
use tokio::sync::{watch, RwLock};
use tracing::{debug, error, info, warn};

pub const API_GROUP: &str = "git-events-runner.rs";
pub const CURRENT_API_VERSION: &str = "v1alpha1";

/// Actual triggers state
pub struct TriggersState<S> {
    pub(crate) tasks: HashMap<String, TaskId>,
    pub(crate) specs: HashMap<String, S>,
    pub(crate) statuses: HashMap<String, TriggerStatus>,
}

impl<S> Default for TriggersState<S> {
    fn default() -> Self {
        Self {
            tasks: Default::default(),
            specs: HashMap::<String, S>::new(),
            statuses: Default::default(),
        }
    }
}

/// State shared between the controllers and the web server
#[derive(Clone)]
pub struct State {
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Web servers readiness
    pub ready: Arc<RwLock<bool>>,
    /// Shared secrets cache/retriever
    pub secrets_cache: Arc<ExpiringSecretCache>,
    /// Cli config
    pub cli_config: Arc<CliConfig>,
}

impl State {
    pub fn new(cli_config: Arc<CliConfig>, secrets_cache: Arc<ExpiringSecretCache>) -> Self {
        Self {
            diagnostics: Default::default(),
            ready: Default::default(),
            secrets_cache,
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

// Context for our reconcilers
#[derive(Clone)]
pub struct Context<S> {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Scheduler to run periodic tasks
    pub scheduler: Arc<RwLock<Scheduler>>,
    /// Actual state of all Triggers
    pub triggers: Arc<RwLock<TriggersState<S>>>,
    /// Shared secrets cache/retriever
    pub secrets_cache: Arc<ExpiringSecretCache>,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Create a Controller Context that can update State
    pub fn to_context<S>(
        &self,
        client: Client,
        scheduler: Arc<RwLock<Scheduler>>,
        triggers: Arc<RwLock<TriggersState<S>>>,
    ) -> Arc<Context<S>> {
        Arc::new(Context {
            client,
            diagnostics: self.diagnostics.clone(),
            secrets_cache: self.secrets_cache.clone(),
            scheduler,
            triggers,
        })
    }
}

/// Initialize the controllers and shared state (given the crd is installed)
pub async fn run_leader_controllers(
    client: Client,
    state: State,
    shutdown_channel: watch::Receiver<bool>,
    schedule_parallelism: usize,
) {
    info!("Starting Leader controllers");
    let scheduler = SchedulerBuilder::new()
        .garbage_collector(GarbageCollector::Immediate)
        .worker_type(WorkerType::MultiThread(RuntimeThreads::CpuCores))
        .parallelism(WorkerParallelism::Limited(schedule_parallelism))
        .build();
    let scheduler = Arc::new(RwLock::new(scheduler));

    {
        let mut controllers = vec![];
        let triggers_state = Arc::new(RwLock::new(TriggersState::<ScheduleTriggerSpec>::default()));
        let context = state.to_context(client.clone(), scheduler.clone(), triggers_state);

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
                .filter_map(|x| async move { std::result::Result::ok(x) })
                .for_each(|_| futures::future::ready(())),
        ));

        debug!("starting controllers main loop");
        join_all(controllers).await;
        debug!("controllers main loop finished");
    }

    info!("Shutting down ScheduleTriggers task scheduler");
    let scheduler = Arc::into_inner(scheduler)
        .expect("more than one copies of scheduler is present, looks like a BUG!")
        .into_inner();
    scheduler
        .shutdown(sacs::scheduler::ShutdownOpts::WaitForFinish)
        .await
        .unwrap_or(())
}

pub async fn run_web_controllers(
    client: Client,
    state: State,
    shutdown_channel: watch::Receiver<bool>,
    scheduler: Arc<RwLock<Scheduler>>,
    triggers_state: Arc<RwLock<TriggersState<WebhookTriggerSpec>>>,
) {
    info!("Starting Web controllers");

    {
        let mut controllers = vec![];
        let context = state.to_context(client.clone(), scheduler, triggers_state);

        let triggers_api = Api::<WebhookTrigger>::all(client.clone());
        check_api_by_list(&triggers_api, "Triggers").await;
        let mut shutdown = shutdown_channel.clone();
        controllers.push(tokio::task::spawn(
            Controller::new(triggers_api, Config::default().any_semantic())
                .with_config(controller::Config::default().debounce(Duration::from_millis(500)))
                .graceful_shutdown_on(async move { shutdown.changed().await.unwrap_or(()) })
                .run(
                    reconcile_namespaced::<WebhookTrigger, WebhookTriggerSpec>,
                    error_policy::<WebhookTrigger, WebhookTriggerSpec>,
                    context.clone(),
                )
                .filter_map(|x| async move { std::result::Result::ok(x) })
                .for_each(|_| futures::future::ready(())),
        ));

        debug!("starting controllers main loop");
        join_all(controllers).await;
        debug!("controllers main loop finished");
    }
}

async fn check_api_by_list<K>(api: &Api<K>, api_name: &str)
where
    K: Clone + DeserializeOwned + Debug,
{
    if let Err(e) = api.list(&ListParams::default().limit(1)).await {
        error!(
            "CRD `{}` is not queryable; {e:?}. Is the CRD installed/updated?",
            api_name
        );
        info!("Installation: git-events-runner crds | kubectl apply -f -");
        std::process::exit(1);
    }
}

#[allow(async_fn_in_trait)]
pub trait Reconcilable<S> {
    async fn reconcile(&self, ctx: Arc<Context<S>>) -> Result<ReconcileAction>;
    async fn cleanup(&self, ctx: Arc<Context<S>>) -> Result<ReconcileAction>;
    fn kind(&self) -> &str;
    fn finalizer_name(&self) -> Option<&'static str> {
        None
    }
}

fn error_policy<K, S>(_resource: Arc<K>, error: &Error, _ctx: Arc<Context<S>>) -> ReconcileAction
where
    K: Reconcilable<S>,
{
    warn!("reconcile failed: {:?}", error);
    ReconcileAction::await_change()
}

async fn reconcile_namespaced<K, S>(
    resource: Arc<K>,
    ctx: Arc<Context<S>>,
) -> Result<ReconcileAction>
where
    K: Resource + Reconcilable<S>,
    K: Debug + Clone + DeserializeOwned + Serialize,
    <K as Resource>::DynamicType: Default,
    K: Resource<Scope = NamespaceResourceScope>,
{
    let ns = resource.namespace().unwrap();
    let resource_api: Api<K> = Api::namespaced(ctx.client.clone(), &ns);

    info!(
        "Reconciling {} `{}/{}`",
        resource.kind(),
        resource.name_any(),
        ns
    );
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
