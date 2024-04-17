pub(crate) mod action;
pub(crate) mod git_repo;
pub(crate) mod trigger;

use self::trigger::ScheduleTrigger;
use crate::{
    Error, Result, ScheduleTriggerSpec, TriggerStatus, WebhookTrigger, WebhookTriggerSpec,
};
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
pub struct TriggersState<S> {
    tasks: HashMap<String, TaskId>,
    specs: HashMap<String, S>,
    statuses: HashMap<String, TriggerStatus>,
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
pub struct Context<S> {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Scheduler to run periodic tasks
    pub scheduler: Arc<RwLock<Scheduler>>,
    /// Actual state of all Triggers
    pub triggers: Arc<RwLock<TriggersState<S>>>,
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
            scheduler,
            triggers,
        })
    }
}

/// Initialize the controllers and shared state (given the crd is installed)
pub async fn run_leader_controllers(state: State, shutdown_channel: watch::Receiver<bool>) {
    info!("Starting Leader controllers");
    let scheduler = Arc::new(RwLock::new(Scheduler::default()));
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");

    {
        let mut controllers = vec![];
        let triggers_state = Arc::new(RwLock::new(TriggersState::<ScheduleTriggerSpec>::default()));
        let context = state.to_context(client.clone(), scheduler.clone(), triggers_state);

        let triggers_api = Api::<ScheduleTrigger>::all(client.clone());
        check_api_by_list(&triggers_api, "Triggers").await;
        let mut shutdown = shutdown_channel.clone();
        controllers.push(tokio::task::spawn(
            Controller::new(triggers_api, Config::default().any_semantic())
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
    let scheduler = Arc::into_inner(scheduler).unwrap().into_inner();
    scheduler
        .shutdown(sacs::scheduler::ShutdownOpts::WaitForFinish)
        .await
        .unwrap_or(())
}

pub async fn run_web_controllers(
    state: State,
    shutdown_channel: watch::Receiver<bool>,
    scheduler: Arc<RwLock<Scheduler>>,
    triggers_state: Arc<RwLock<TriggersState<WebhookTriggerSpec>>>,
) {
    info!("Starting Web controllers");
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");

    {
        let mut controllers = vec![];
        let context = state.to_context(client.clone(), scheduler, triggers_state);

        let triggers_api = Api::<WebhookTrigger>::all(client.clone());
        check_api_by_list(&triggers_api, "Triggers").await;
        let mut shutdown = shutdown_channel.clone();
        controllers.push(tokio::task::spawn(
            Controller::new(triggers_api, Config::default().any_semantic())
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
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
}

#[allow(async_fn_in_trait)]
pub trait Reconcilable<S> {
    async fn reconcile(&self, ctx: Arc<Context<S>>) -> Result<ReconcileAction>;
    async fn cleanup(&self, ctx: Arc<Context<S>>) -> Result<ReconcileAction>;
    fn finalizer_name(&self) -> String;
    fn kind(&self) -> &str;
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
