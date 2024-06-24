use crate::{
    cli::CLI_CONFIG,
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
use lazy_static::lazy_static;
use prometheus::{histogram_opts, opts, register, HistogramVec, IntCounterVec};
use sacs::{
    scheduler::{
        GarbageCollector, RuntimeThreads, Scheduler, SchedulerBuilder, TaskScheduler, WorkerParallelism, WorkerType,
    },
    task::TaskId,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{clone::Clone, collections::HashMap, default::Default, fmt::Debug, sync::Arc, time::Duration};
use tokio::{
    sync::{watch, RwLock},
    time::Instant,
};
use tracing::{debug, error, info, warn};

lazy_static! {
    static ref METRICS: Metrics = Metrics::default().register();
}

struct Metrics {
    reconcile_duration: HistogramVec,
    reconcile_count: IntCounterVec,
    failed_count: IntCounterVec,
}

impl Default for Metrics {
    fn default() -> Self {
        let cli_config = CLI_CONFIG.get().unwrap();

        let reconcile_count = IntCounterVec::new(
            opts!(
                format!("{}_reconcile_count", cli_config.metrics_prefix),
                "The number reconciliations",
            ),
            &["namespace", "resource_kind", "resource_name"],
        )
        .unwrap();

        let failed_count = IntCounterVec::new(
            opts!(
                format!("{}_failed_reconcile_count", cli_config.metrics_prefix),
                "The number failed reconciliations",
            ),
            &["namespace", "resource_kind", "resource_name"],
        )
        .unwrap();

        let reconcile_duration = HistogramVec::new(
            histogram_opts!(
                format!("{}_reconcile_duration_seconds", cli_config.metrics_prefix),
                "The duration of reconciliations"
            )
            .buckets(vec![0.001, 0.01, 0.1, 0.5, 1., 2.]),
            &["namespace", "resource_kind", "resource_name"],
        )
        .unwrap();

        Self {
            reconcile_duration,
            reconcile_count,
            failed_count,
        }
    }
}

impl Metrics {
    fn register(self) -> Self {
        register(Box::new(self.reconcile_count.clone())).unwrap();
        register(Box::new(self.reconcile_duration.clone())).unwrap();
        register(Box::new(self.failed_count.clone())).unwrap();

        self
    }

    fn count_and_measure(&self, namespace: &str, resource_kind: &str, resource_name: &str, latency: Duration) {
        let labels: [&str; 3] = [namespace, resource_kind, resource_name];

        if let Ok(metric) = self.reconcile_count.get_metric_with_label_values(&labels) {
            metric.inc();
        }

        if let Ok(metric) = self.reconcile_duration.get_metric_with_label_values(&labels) {
            metric.observe(latency.as_secs_f64());
        }
    }

    fn count_failed(&self, namespace: &str, resource_kind: &str, resource_name: &str) {
        let labels: [&str; 3] = [namespace, resource_kind, resource_name];

        if let Ok(metric) = self.failed_count.get_metric_with_label_values(&labels) {
            metric.inc();
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
    /// Cli config
    pub source_clone_folder: Arc<String>,
}

impl State {
    pub fn new(source_clone_folder: Arc<String>) -> Self {
        Self {
            diagnostics: Default::default(),
            ready: Default::default(),
            source_clone_folder,
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
    pub source_clone_folder: Arc<String>,
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
            source_clone_folder: self.source_clone_folder.clone(),
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
        if let Err(_e) = check_api_by_list(&triggers_api).await {
            std::process::exit(1);
        }

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
async fn check_api_by_list<K>(api: &Api<K>) -> Result<()>
where
    K: Clone + DeserializeOwned + Debug + CustomApiResource,
{
    if let Err(e) = api.list(&ListParams::default().limit(1)).await {
        error!(api = %K::crd_kind(), error = %e, "CRD is not queryable, looks like CRD isn't installed/updated?");
        info!("to install run: {} crds | kubectl apply -f -", env!("CARGO_PKG_NAME"));
        Err(Error::KubeError(e))
    } else {
        Ok(())
    }
}

/// Reconciliation errors handler
fn error_policy<K, S>(resource: Arc<K>, error: &Error, _ctx: Arc<Context>) -> ReconcileAction
where
    K: Reconcilable<S>,
    K: CustomApiResource + kube::Resource,
{
    warn!(%error, "reconcile failed");
    METRICS.count_failed(
        &resource.namespace().unwrap_or_default(),
        K::crd_kind(),
        &resource.name_any(),
    );
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
    let start = Instant::now();
    ctx.diagnostics.write().await.last_event = Utc::now();

    let ns = resource.namespace().unwrap();
    let resource_name = resource.name_any();
    let resource_api: Api<K> = Api::namespaced(ctx.client.clone(), &ns);

    debug!(kind = %K::crd_kind(), namespace = %ns, resource = %resource_name, "reconciling");
    let result = if let Some(finalizer_name) = resource.finalizer_name() {
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
    };

    METRICS.count_and_measure(&ns, K::crd_kind(), &resource_name, start.elapsed());
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{
        action::{Action, ClusterAction},
        git_repo::{ClusterGitRepo, GitRepo},
        trigger::{self, WebhookTrigger},
    };
    use k8s_openapi::api::core::v1::Namespace;
    use kube::{
        api::{Api, DeleteParams, PostParams},
        Client, CustomResource,
    };
    use schemars::JsonSchema;
    use serde::Deserialize;
    use tokio::sync::OnceCell;

    static TRACING_INITIALIZED: OnceCell<()> = OnceCell::const_new();
    static NAMESPACE_INITIALIZED: OnceCell<()> = OnceCell::const_new();

    const NAMESPACE: &str = "reconcile-trigger-should-update-status";

    #[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
    #[cfg_attr(test, derive(Default))]
    #[kube(kind = "FakeCustomResource", group = "git-events-runner.rs", version = "v1alpha1")]
    #[serde(rename_all = "camelCase")]
    pub struct FakeCustomResourceSpec {
        some_field: String,
    }

    impl CustomApiResource for FakeCustomResource {
        fn crd_kind() -> &'static str {
            "FakeCustomResource"
        }
    }

    async fn init() {
        TRACING_INITIALIZED.get_or_init(|| async { init_tracing().await }).await;
        NAMESPACE_INITIALIZED
            .get_or_init(|| async { create_namespace().await })
            .await;
    }

    async fn init_tracing() {
        tracing_subscriber::fmt::init();
    }

    /// Unattended namespace creation
    async fn create_namespace() {
        let client = Client::try_default().await.unwrap();
        let api = Api::<Namespace>::all(client);
        let pp = PostParams::default();

        let mut data = Namespace::default();
        data.meta_mut().name = Some(String::from(NAMESPACE));

        api.create(&pp, &data).await.unwrap_or_default();
    }

    /// Create the simplest test context: default client, scheduler and state
    async fn get_test_context() -> Arc<Context> {
        let client = Client::try_default().await.unwrap();
        let state = State::new(Arc::new(String::from("/tmp/test_git_events_runner_context")));
        let scheduler = Arc::new(RwLock::new(Scheduler::default()));
        let triggers = Arc::new(RwLock::new(TriggersState::default()));

        state.to_context(client, scheduler, triggers)
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn reconcile_schedule_trigger_should_set_idle_status() {
        init().await;

        let ctx = get_test_context().await;
        let trigger_i_name = "good-interval-trigger";
        let trigger_c_name = "good-cron-trigger";

        let api = Api::<ScheduleTrigger>::namespaced(ctx.client.clone(), NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create good trigger with interval schedule
        let trigger_i = ScheduleTrigger::test_trigger_with_interval(trigger_i_name, NAMESPACE, "30s");
        let trigger_i = api.create(&pp, &trigger_i).await.unwrap();
        let _ = trigger_i.reconcile(ctx.clone()).await.unwrap();
        // Assert status
        let output = api.get_status(trigger_i_name).await.unwrap();
        assert!(output.status.is_some());
        assert_eq!(output.status.unwrap().state, trigger::TriggerState::Idle);
        assert_eq!(ctx.triggers.read().await.schedules.len(), 1);
        assert_eq!(ctx.triggers.read().await.tasks.len(), 1);

        // Create good trigger with cron schedule
        let trigger_c = ScheduleTrigger::test_trigger_with_cron(trigger_c_name, NAMESPACE, "0 0 * * *");
        let trigger_c = api.create(&pp, &trigger_c).await.unwrap();
        let _ = trigger_c.reconcile(ctx.clone()).await.unwrap();
        // Assert status
        let output = api.get_status(trigger_c_name).await.unwrap();
        assert!(output.status.is_some());
        assert_eq!(output.status.unwrap().state, trigger::TriggerState::Idle);
        assert_eq!(ctx.triggers.read().await.schedules.len(), 2);
        assert_eq!(ctx.triggers.read().await.tasks.len(), 2);

        // Clean up interval trigger
        let _result = api.delete(trigger_i_name, &dp).await.unwrap();
        let _ = trigger_i.cleanup(ctx.clone()).await.unwrap();
        // Assert for changed state
        assert_eq!(ctx.triggers.read().await.schedules.len(), 1);
        assert_eq!(ctx.triggers.read().await.tasks.len(), 1);

        // Clean up cron trigger
        let _result = api.delete(trigger_c_name, &dp).await.unwrap();
        let _ = trigger_c.cleanup(ctx.clone()).await.unwrap();
        // Assert for empty state
        assert_eq!(ctx.triggers.read().await.schedules.len(), 0);
        assert_eq!(ctx.triggers.read().await.tasks.len(), 0);
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn all_crds_should_be_installed() {
        let ctx = get_test_context().await;

        assert!(check_api_by_list(&Api::<ScheduleTrigger>::all(ctx.client.clone()))
            .await
            .is_ok());
        assert!(check_api_by_list(&Api::<WebhookTrigger>::all(ctx.client.clone()))
            .await
            .is_ok());
        assert!(check_api_by_list(&Api::<Action>::all(ctx.client.clone())).await.is_ok());
        assert!(check_api_by_list(&Api::<ClusterAction>::all(ctx.client.clone()))
            .await
            .is_ok());
        assert!(check_api_by_list(&Api::<GitRepo>::all(ctx.client.clone()))
            .await
            .is_ok());
        assert!(check_api_by_list(&Api::<ClusterGitRepo>::all(ctx.client.clone()))
            .await
            .is_ok());
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn fake_crd_error() {
        let ctx = get_test_context().await;

        assert!(check_api_by_list(&Api::<FakeCustomResource>::all(ctx.client.clone()))
            .await
            .is_err());
    }
}
