pub(crate) mod action;
pub(crate) mod git_repo;
pub(crate) mod trigger;

use self::trigger::Trigger;
use crate::{Error, Result, TriggerSpec, TriggerStatus};
use futures::{future::join_all, StreamExt};
use k8s_openapi::{
    chrono::{DateTime, Utc},
    NamespaceResourceScope,
};
use kube::{
    api::ListParams,
    runtime::{
        controller::Action as ReconcileAction,
        events::{Recorder, Reporter},
        finalizer::{finalizer, Event as Finalizer},
        watcher::Config,
        Controller,
    },
    Api, Client, Resource, ResourceExt,
};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use sacs::scheduler::TaskScheduler;
use sacs::{scheduler::Scheduler, task::TaskId};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{clone::Clone, collections::HashMap, default::Default, fmt::Debug, sync::Arc};
use tokio::sync::{watch, RwLock};
use tracing::{debug, error, info, warn};

const API_GROUP: &str = "git-events-runner.rs";
const CURRENT_API_VERSION: &str = "v1alpha1";

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    name: String,
}

/// Actual triggers state
#[derive(Default)]
pub struct TriggersState {
    pub(crate) tasks: HashMap<String, TaskId>,
    specs: HashMap<String, TriggerSpec>,
    statuses: HashMap<String, TriggerStatus>,
}

/// State shared between the controllers and the web server
#[derive(Default, Clone)]
pub struct State {
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Web servers readiness
    pub ready: Arc<RwLock<bool>>,
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
    fn recorder<K>(&self, client: Client, res: &K) -> Recorder
    where
        K: Resource<DynamicType = ()>,
    {
        Recorder::new(client, self.reporter.clone(), res.object_ref(&()))
    }
}

// Context for our reconcilers
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Scheduler to run periodic tasks
    pub scheduler: Arc<RwLock<Scheduler>>,
    /// Actual state of all Triggers
    pub triggers: Arc<RwLock<TriggersState>>,
    /// Type of controller
    pub cntrl_type: ControllerType,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Create a Controller Context that can update State
    pub fn to_context(
        &self,
        client: Client,
        scheduler: Arc<RwLock<Scheduler>>,
        triggers: Arc<RwLock<TriggersState>>,
        cntrl_type: ControllerType,
    ) -> Arc<Context> {
        Arc::new(Context {
            client,
            diagnostics: self.diagnostics.clone(),
            scheduler,
            triggers,
            cntrl_type,
        })
    }
}

#[derive(Clone, PartialEq)]
pub enum ControllerType {
    Leader,
    Web,
}
/// Initialize the controllers and shared state (given the crd is installed)
pub async fn run_controllers(
    cntrl_type: ControllerType,
    state: State,
    shutdown_channel: watch::Receiver<bool>,
    outer_scheduler: Option<Arc<RwLock<Scheduler>>>,
    outer_triggers_state: Option<Arc<RwLock<TriggersState>>>,
) {
    info!("Starting all leader controllers");
    let scheduler = outer_scheduler.unwrap_or(Arc::new(RwLock::new(Scheduler::default())));
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");

    {
        let mut controllers = vec![];
        let triggers_state =
            outer_triggers_state.unwrap_or(Arc::new(RwLock::new(TriggersState::default())));
        let context = state.to_context(
            client.clone(),
            scheduler.clone(),
            triggers_state,
            cntrl_type,
        );

        let triggers_api = Api::<Trigger>::all(client.clone());
        check_api_by_list(&triggers_api, "Triggers").await;
        let mut shutdown = shutdown_channel.clone();
        controllers.push(tokio::task::spawn(
            Controller::new(triggers_api, Config::default().any_semantic())
                .graceful_shutdown_on(async move { shutdown.changed().await.unwrap_or(()) })
                .run(
                    reconcile_namespaced::<Trigger>,
                    error_policy::<Trigger>,
                    context.clone(),
                )
                .filter_map(|x| async move { std::result::Result::ok(x) })
                .for_each(|_| futures::future::ready(())),
        ));

        debug!("starting controllers main loop");
        join_all(controllers).await;
        debug!("controllers main loop finished");
    }

    if Arc::strong_count(&scheduler) <= 1 {
        info!("Shutting down Triggers task scheduler");
        let scheduler = Arc::into_inner(scheduler).unwrap().into_inner();
        scheduler
            .shutdown(sacs::scheduler::ShutdownOpts::WaitForFinish)
            .await
            .unwrap_or(())
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
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
}

pub(crate) trait Reconcilable {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction>;
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<ReconcileAction>;
    fn finalizer_name(&self) -> String;
    fn kind(&self) -> &str;
}

fn error_policy<K: Reconcilable>(
    _resource: Arc<K>,
    error: &Error,
    _ctx: Arc<Context>,
) -> ReconcileAction {
    warn!("reconcile failed: {:?}", error);
    ReconcileAction::await_change()
}

async fn reconcile_namespaced<K>(resource: Arc<K>, ctx: Arc<Context>) -> Result<ReconcileAction>
where
    K: Resource + Reconcilable,
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
    finalizer(
        &resource_api,
        resource.finalizer_name().as_str(),
        resource,
        |event| async {
            match event {
                Finalizer::Apply(resource) => resource.reconcile(ctx.clone()).await,
                Finalizer::Cleanup(resource) => resource.cleanup(ctx.clone()).await,
            }
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

pub(crate) fn random_string(len: usize) -> String {
    let rand: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect();
    rand
}
