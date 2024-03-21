pub(crate) mod action;
pub(crate) mod git_repo;
pub(crate) mod trigger;

use self::{action::Action, git_repo::GitRepo, trigger::Trigger};
use crate::{TriggerSpec, TriggerStatus};
use futures::{future::join_all, StreamExt};
use k8s_openapi::chrono::{DateTime, Utc};
use kube::{
    api::ListParams,
    runtime::{
        events::{Recorder, Reporter},
        watcher::Config,
        Controller,
    },
    Api, Client,
};
use sacs::{scheduler::Scheduler, task::TaskId};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tracing::{error, info};

const API_GROUP: &str = "git-events-runner.rs";
const CURRENT_API_VERSION: &str = "v1alpha1";

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TlsVerifyConfig {
    Ignore,
    #[default]
    Ca,
    Full,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct GitRepoStatus {
    pub state: SourceState,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum SourceState {
    #[default]
    Pending,
    Ready,
    TlsConfigError,
    AuthConfigError,
    WrongRepoUriFormat,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    name: String,
}

/// Actual triggers state
#[derive(Default)]
pub struct TriggersState {
    tasks: HashMap<String, TaskId>,
    specs: HashMap<String, TriggerSpec>,
    statuses: HashMap<String, TriggerStatus>,
}

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
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
        K: kube::Resource<DynamicType = ()>,
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
}

/// State shared between the controllers and the web server
#[derive(Clone, Default)]
pub struct State {
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// SACS Scheduler to run periodic tasks
    pub scheduler: Arc<RwLock<Scheduler>>,
    /// Actual state of all Triggers
    pub triggers: Arc<RwLock<TriggersState>>,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Create a Controller Context that can update State
    pub fn to_context(&self, client: Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            diagnostics: self.diagnostics.clone(),
            scheduler: self.scheduler.clone(),
            triggers: self.triggers.clone(),
        })
    }
}

/// Initialize the controllers and shared state (given the crd is installed)
pub async fn run(state: State) {
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
    let mut controllers = vec![];

    let context = state.to_context(client.clone());

    let git_repos = Api::<GitRepo>::all(client.clone());
    check_api_by_list(&git_repos, "GitRepos").await;
    controllers.push(tokio::task::spawn(
        Controller::new(git_repos, Config::default().any_semantic())
            .shutdown_on_signal()
            .run(git_repo::reconcile, git_repo::error_policy, context.clone())
            .filter_map(|x| async move { std::result::Result::ok(x) })
            .for_each(|_| futures::future::ready(())),
    ));

    let triggers = Api::<Trigger>::all(client.clone());
    check_api_by_list(&triggers, "Triggers").await;
    controllers.push(tokio::task::spawn(
        Controller::new(triggers, Config::default().any_semantic())
            .shutdown_on_signal()
            .run(trigger::reconcile, trigger::error_policy, context.clone())
            .filter_map(|x| async move { std::result::Result::ok(x) })
            .for_each(|_| futures::future::ready(())),
    ));

    let actions = Api::<Action>::all(client.clone());
    check_api_by_list(&actions, "Actions").await;
    controllers.push(tokio::task::spawn(
        Controller::new(actions, Config::default().any_semantic())
            .shutdown_on_signal()
            .run(action::reconcile, action::error_policy, context.clone())
            .filter_map(|x| async move { std::result::Result::ok(x) })
            .for_each(|_| futures::future::ready(())),
    ));

    join_all(controllers).await;
}

async fn check_api_by_list<K>(api: &Api<K>, api_name: &str)
where
    K: Clone + DeserializeOwned + std::fmt::Debug,
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
