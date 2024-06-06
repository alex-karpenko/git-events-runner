use crate::{
    cache::ApiCache,
    controller::Context,
    resources::{
        action::{Action, ActionExecutor, ClusterAction},
        git_repo::{ClusterGitRepo, GitRepo, GitRepoGetter},
        random_string, CustomApiResource, Reconcilable, API_GROUP, CURRENT_API_VERSION,
    },
    Error, Result,
};
use chrono::{DateTime, Utc};
use git2::{Oid, Repository};
use k8s_openapi::{chrono::SecondsFormat, NamespaceResourceScope};
use kube::{
    api::{Patch, PatchParams},
    core::object::HasStatus,
    runtime::{
        controller::Action as ReconcileAction,
        events::{Event, EventType, Recorder},
    },
    Api, Client, CustomResource, Resource, ResourceExt,
};
use sacs::{
    scheduler::{CancelOpts, TaskScheduler},
    task::{CronOpts, CronSchedule, Task, TaskSchedule},
};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io,
    sync::Arc,
    time::{Duration, SystemTime},
};
use strum_macros::{Display, EnumString};
use tokio::{
    fs::{create_dir_all, remove_dir_all, try_exists, File},
    io::AsyncReadExt,
};
use tracing::{debug, error, info, warn};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[kube(
    kind = "ScheduleTrigger",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    doc = "ScheduleTrigger custom resource definition",
    namespaced,
    printcolumn = r#"{"name":"State", "type":"string", "description":"current trigger state", "jsonPath":".status.state"}"#,
    printcolumn = r#"{"name":"Last Run", "type":"date", "format":"date-time", "description":"time of last trigger run", "jsonPath":".status.lastRun"}"#
)]
#[kube(status = "TriggerStatus")]
#[serde(rename_all = "camelCase")]
pub struct ScheduleTriggerSpec {
    sources: TriggerSources,
    schedule: TriggerSchedule,
    action: TriggerAction,
}

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[kube(
    kind = "WebhookTrigger",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    doc = "WebhookTrigger custom resource definition",
    namespaced,
    printcolumn = r#"{"name":"State", "type":"string", "description":"current trigger state", "jsonPath":".status.state"}"#,
    printcolumn = r#"{"name":"Last Run", "type":"date", "format":"date-time", "description":"time of last trigger run", "jsonPath":".status.lastRun"}"#
)]
#[kube(status = "TriggerStatus")]
#[serde(rename_all = "camelCase")]
pub struct WebhookTriggerSpec {
    pub(crate) sources: TriggerSources,
    pub(crate) webhook: TriggerWebhook,
    action: TriggerAction,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TriggerStatus {
    state: TriggerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    checked_sources: HashMap<String, CheckedSourceState>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckedSourceState {
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed: Option<String>,
}

impl CheckedSourceState {
    fn is_equal(&self, other: Option<&CheckedSourceState>, with_file: bool) -> bool {
        if let Some(other) = other {
            if !with_file {
                self.commit_hash == other.commit_hash
            } else {
                self.file_hash == other.file_hash
            }
        } else {
            self.commit_hash.is_none() && self.file_hash.is_none()
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
pub enum TriggerState {
    #[default]
    Idle,
    Running,
    WrongConfig,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerSources {
    #[serde(default)]
    kind: TriggerSourceKind,
    pub(crate) names: Vec<String>,
    #[serde(default)]
    watch_on: TriggerWatchOn,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]
pub enum TriggerSourceKind {
    #[default]
    GitRepo,
    ClusterGitRepo,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerWatchOn {
    on_change_only: bool,
    #[serde(default)]
    reference: TriggerGitRepoReference,
    #[serde(rename = "file")]
    #[serde(skip_serializing_if = "Option::is_none")]
    file_: Option<String>,
}

impl Default for TriggerWatchOn {
    fn default() -> Self {
        Self {
            on_change_only: true,
            reference: Default::default(),
            file_: None,
        }
    }
}

impl TriggerWatchOn {
    fn reference_name(&self) -> String {
        match &self.reference {
            TriggerGitRepoReference::Branch(r) => r,
            TriggerGitRepoReference::Tag(r) => r,
            TriggerGitRepoReference::Commit(r) => r,
        }
        .to_string()
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TriggerGitRepoReference {
    Branch(String),
    Tag(String),
    Commit(String),
}

impl Default for TriggerGitRepoReference {
    fn default() -> Self {
        Self::Branch(String::from("main"))
    }
}

impl TriggerGitRepoReference {
    pub fn to_refname(&self, prefix: Option<&str>) -> (String, &String) {
        let prefix = prefix.unwrap_or("");
        match self {
            TriggerGitRepoReference::Branch(branch) => (format!("{prefix}branch"), branch),
            TriggerGitRepoReference::Tag(tag) => (format!("{prefix}tag"), tag),
            TriggerGitRepoReference::Commit(commit) => (format!("{prefix}commit"), commit),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TriggerSchedule {
    Interval(String),
    Cron(String),
}

impl TriggerSchedule {
    fn is_valid(&self) -> bool {
        match self {
            TriggerSchedule::Interval(interval) => interval.parse::<humantime::Duration>().is_ok(),
            TriggerSchedule::Cron(cron) => CronSchedule::try_from(cron).is_ok(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerWebhook {
    pub(crate) multi_source: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) auth_config: Option<TriggerWebhookAuthConfig>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerWebhookAuthConfig {
    pub(crate) secret_ref: SecretRef,
    pub(crate) key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) header: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub(crate) name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TriggerAction {
    kind: TriggerActionKind,
    name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]
pub enum TriggerActionKind {
    #[default]
    Action,
    ClusterAction,
}

/// To use in create_trigger_task - it reflects which sources in the trigger should be processed:
/// all of the defined ones or single one only.
/// It's used by WebhookTrigger mostly.
#[derive(Clone)]
pub enum TriggerTaskSources {
    All,
    Single(String),
}

impl From<String> for TriggerTaskSources {
    fn from(value: String) -> Self {
        Self::Single(value)
    }
}

impl From<&String> for TriggerTaskSources {
    fn from(value: &String) -> Self {
        Self::Single(value.to_owned())
    }
}

impl CustomApiResource for ScheduleTrigger {
    fn kind(&self) -> &str {
        "ScheduleTrigger"
    }
}

impl CustomApiResource for WebhookTrigger {
    fn kind(&self) -> &str {
        "WebhookTrigger"
    }
}

impl Trigger<ScheduleTriggerSpec> for ScheduleTrigger {
    fn sources(&self) -> &TriggerSources {
        &self.spec.sources
    }

    fn action(&self) -> &TriggerAction {
        &self.spec.action
    }

    fn trigger_status(&self) -> &Option<TriggerStatus> {
        &self.status
    }
}

impl Trigger<WebhookTriggerSpec> for WebhookTrigger {
    fn sources(&self) -> &TriggerSources {
        &self.spec.sources
    }

    fn action(&self) -> &TriggerAction {
        &self.spec.action
    }

    fn trigger_status(&self) -> &Option<TriggerStatus> {
        &self.status
    }
}

impl Reconcilable<ScheduleTriggerSpec> for ScheduleTrigger {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        let trigger_hash_key = self.trigger_hash_key();

        // Reconcile if schedule has been changed
        if Some(&self.spec.schedule) != ctx.triggers.read().await.schedules.get(&trigger_hash_key) {
            let mut triggers = ctx.triggers.write().await;
            debug!("Update ScheduleTrigger `{trigger_hash_key}`");

            // Cancel existing task if it's present
            if triggers.tasks.contains_key(&trigger_hash_key) {
                let task_id = triggers.tasks.remove(&trigger_hash_key).unwrap();
                let scheduler = ctx.scheduler.write().await;
                let res = scheduler.cancel(task_id, CancelOpts::Ignore).await;
                if res.is_err() {
                    error!("Can't cancel task: {res:?}");
                }
            }

            // Get time of last trigger's run, if it was run
            let last_run: Option<SystemTime> = if let Some(status) = self.status() {
                status.last_run.as_ref().map(|last_run| {
                    DateTime::parse_from_rfc3339(last_run)
                        .expect("Incorrect time format in LastRun field, looks like a BUG.")
                        .into()
                })
            } else {
                None
            };

            // Get new schedule for new task
            match self.get_task_schedule(last_run) {
                Ok(schedule) => {
                    // Add new task to scheduler instead of the cancelled one
                    let scheduler = ctx.scheduler.write().await;
                    let task = self.create_trigger_task(
                        ctx.client.clone(),
                        schedule,
                        TriggerTaskSources::All,
                        ctx.cli_config.source_clone_folder.clone(),
                    );
                    let task_id = scheduler.add(task).await?;
                    let tasks = &mut triggers.tasks;
                    tasks.insert(self.trigger_hash_key(), task_id);
                }
                Err(_) => {
                    // Invalid schedule
                    let recorder = &ctx.diagnostics.read().await.recorder(ctx.client.clone(), self);
                    self.publish_trigger_validation_event(
                        recorder,
                        EventType::Warning,
                        "Unable to parse schedule expression, scheduler is disabled",
                        "ValidateTriggerConfig",
                    )
                    .await?;
                }
            }

            // Update trigger schedule in the map
            triggers
                .schedules
                .insert(self.trigger_hash_key(), self.spec.schedule.clone());
        }

        Ok(ReconcileAction::await_change())
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        info!(
            "Cleanup {} `{}` in {}",
            self.kind(),
            self.name_any(),
            self.namespace()
                .expect("unable to get resource namespace, looks like a BUG!")
        );
        let trigger_hash_key = self.trigger_hash_key();
        // Remove from schedules map
        if ctx.triggers.read().await.schedules.contains_key(&trigger_hash_key) {
            let mut triggers = ctx.triggers.write().await;
            triggers.schedules.remove(&trigger_hash_key);
        }
        // Drop task and remove from tasks map
        if ctx.triggers.read().await.tasks.contains_key(&trigger_hash_key) {
            let mut triggers = ctx.triggers.write().await;
            let task_id = triggers.tasks.remove(&trigger_hash_key).unwrap();
            let scheduler = ctx.scheduler.write().await;
            let res = scheduler.cancel(task_id, CancelOpts::Kill).await;
            if res.is_err() {
                error!("Can't cancel task: {res:?}");
            }
        }

        Ok(ReconcileAction::await_change())
    }

    fn finalizer_name(&self) -> Option<&'static str> {
        Some("scheduletriggers.git-events-runner.rs")
    }
}

impl ScheduleTrigger {
    fn get_task_schedule(&self, last_run: Option<SystemTime>) -> Result<TaskSchedule, ()> {
        if self.spec.schedule.is_valid() {
            let schedule = match &self.spec.schedule {
                TriggerSchedule::Interval(interval) => {
                    let interval: Duration = interval.parse::<humantime::Duration>().map_err(|_| ())?.into();
                    if let Some(last_run) = last_run {
                        let since_last_run = SystemTime::now()
                            .duration_since(last_run)
                            .expect("wrong time of trigger last run, looks like a BUG");
                        let delay = interval.checked_sub(since_last_run).unwrap_or(Duration::from_secs(0));
                        TaskSchedule::IntervalDelayed(interval, delay)
                    } else {
                        TaskSchedule::Interval(interval)
                    }
                }
                TriggerSchedule::Cron(cron) => {
                    let cron = CronSchedule::try_from(cron).unwrap();
                    TaskSchedule::Cron(
                        cron,
                        CronOpts {
                            at_start: last_run.is_none(),
                            concurrent: false,
                        },
                    )
                }
            };
            Ok(schedule)
        } else {
            Err(())
        }
    }

    async fn publish_trigger_validation_event(
        &self,
        recorder: &Recorder,
        type_: EventType,
        note: &str,
        action: &str,
    ) -> Result<()> {
        recorder
            .publish(Event {
                type_,
                reason: "ValidateTrigger".into(),
                note: Some(note.into()),
                action: action.into(),
                secondary: None,
            })
            .await?;

        Ok(())
    }
}

/// Shared behavior for any triggers
pub(crate) trait Trigger<S>
where
    Self: 'static + Send + Sync + ApiCache,
    Self: Resource + ResourceExt + CustomApiResource,
    Self: std::fmt::Debug + Clone + DeserializeOwned + Serialize,
    <Self as Resource>::DynamicType: Default,
    Self: Resource<Scope = NamespaceResourceScope>,
    <Self as kube::Resource>::DynamicType: std::hash::Hash + std::cmp::Eq + Clone,
    S: Send + Sync + 'static,
{
    fn sources(&self) -> &TriggerSources;
    fn action(&self) -> &TriggerAction;
    fn trigger_status(&self) -> &Option<TriggerStatus>;

    /// key to use in various hash maps
    fn trigger_hash_key(&self) -> String {
        format!(
            "{}/{}",
            self.namespace()
                .expect("unable to get resource namespace, looks like a BUG!"), // all triggers are namespaced
            self.name_any()
        )
    }

    /// Update changes parts of the trigger's status filed
    /// If state, checked_sources or last_run isn't None - update it by parameter value
    /// In any of them is None - get existing value from the cache and use it
    ///
    /// If checked_sources is provided in parameters - merge it with existing map
    ///
    fn update_trigger_status(
        &self,
        state: Option<TriggerState>,
        checked_sources: Option<HashMap<String, CheckedSourceState>>,
        last_run: Option<DateTime<Utc>>,
        api: &Api<Self>,
    ) -> impl Future<Output = Result<()>> + Send {
        async move {
            let name = self.name_any();
            let trigger = Self::get(&name, Some(&self.namespace().unwrap()))?;

            let new_status = if let Some(status) = trigger.trigger_status() {
                // Status is already present in the trigger
                // Use new checked_sources map as addition to existing
                let mut checked_sources = checked_sources.unwrap_or_default();
                checked_sources.extend(status.checked_sources.clone());

                // Filter checked_sources to exclude non-existing sources
                let checked_sources: HashMap<String, CheckedSourceState> = checked_sources
                    .into_iter()
                    .filter(|(k, _v)| trigger.sources().names.contains(k))
                    .collect();

                TriggerStatus {
                    state: state.unwrap_or(status.state.clone()),
                    last_run: if let Some(last_run) = last_run {
                        Some(last_run.to_rfc3339_opts(SecondsFormat::Secs, true))
                    } else {
                        status.last_run.clone()
                    },
                    checked_sources,
                }
            } else {
                // No status so far, create new
                TriggerStatus {
                    state: state.unwrap_or_default(),
                    last_run: last_run.map(|last_run| last_run.to_rfc3339_opts(SecondsFormat::Secs, true)),
                    checked_sources: checked_sources.unwrap_or_default(),
                }
            };

            let status = Patch::Apply(json!({
                "apiVersion": format!("{API_GROUP}/{CURRENT_API_VERSION}"),
                "kind": self.kind(),
                "status": {
                    "state": new_status.state,
                    "lastRun": new_status.last_run,
                    "checkedSources": new_status.checked_sources,
                }
            }));
            let pp = PatchParams::apply("cntrlr").force();
            let _o = api.patch_status(&name, &pp, &status).await?;

            Ok(())
        }
    }

    fn create_trigger_task(
        &self,
        client: Client,
        schedule: TaskSchedule,
        task_sources: TriggerTaskSources,
        source_clone_folder: String,
    ) -> Task {
        let trigger_name = self.name_any();
        let trigger_ns = self
            .namespace()
            .expect("unable to get resource namespace, looks like a BUG!");

        Task::new(schedule, move |id| {
            // All these values should be moved ot the task
            let trigger_name = trigger_name.clone();
            let trigger_ns = trigger_ns.clone();
            let client = client.clone();
            let task_sources = task_sources.clone();
            let source_clone_folder = source_clone_folder.clone();

            // Actual trigger job
            Box::pin(async move {
                debug!("Start trigger job: trigger={trigger_ns}/{trigger_name}, job id={id}");
                let triggers_api: Api<Self> = Api::namespaced(client.clone(), &trigger_ns);
                // Get current trigger from cache
                let trigger = Self::get(&trigger_name, Some(&trigger_ns));

                if let Ok(trigger) = trigger {
                    // base folder to clone sources
                    // add random suffix because some webhook trigger may have the same name
                    let base_temp_dir =
                        format!("{source_clone_folder}/{trigger_ns}/{trigger_name}/{}", random_string(8));

                    // Filter checked_sources to exclude non-existing sources
                    // this is possible between last trigger update via API and following task run
                    // so just exclude non-existing sources from the current status
                    let checked_sources: HashMap<String, CheckedSourceState> = trigger
                        .trigger_status()
                        .clone()
                        .unwrap_or_default()
                        .checked_sources
                        .into_iter()
                        .filter(|(k, _v)| trigger.sources().names.contains(k))
                        .collect();

                    // mark trigger as running
                    if let Err(err) = trigger
                        .update_trigger_status(Some(TriggerState::Running), None, None, &triggers_api)
                        .await
                    {
                        error!("Unable to update trigger `{}` state: {err:?}", trigger_name);
                    }

                    // Create set of sources to check
                    let sources_to_check = match task_sources {
                        TriggerTaskSources::All => trigger.sources().names.iter().cloned().collect::<HashSet<String>>(),
                        TriggerTaskSources::Single(source) => HashSet::from([source]),
                    };

                    // Main check loop over all sources
                    for source in sources_to_check {
                        let mut new_source_state = CheckedSourceState {
                            commit_hash: None,
                            file_hash: None,
                            changed: None,
                        };
                        debug!(
                            "Processing {} source {trigger_ns}/{trigger_name}/{source}",
                            trigger.sources().kind
                        );

                        // - Create temp dest folder
                        let repo_path = format!("{base_temp_dir}/{source}");
                        let _ = remove_dir_all(&repo_path).await; // to prevent error if some leftovers exist
                        if let Err(err) = create_dir_all(&repo_path).await {
                            error!("Unable to create temporary folder `{repo_path}`: {err:?}");
                        } else {
                            // TODO: refactor this part to obtain "impl trait" object which implements fetch_repo_ref() instead of kind
                            // and just use it to clone repo
                            let repo = match trigger.sources().kind {
                                // - Get GitRepo object
                                TriggerSourceKind::GitRepo => {
                                    let gitrepo = GitRepo::get(&source, Some(&trigger_ns));
                                    // - Call repo.fetch_repo_ref(...) to get repository content using particular reference
                                    if let Ok(gitrepo) = gitrepo {
                                        gitrepo
                                            .fetch_repo_ref(
                                                client.clone(),
                                                &trigger.sources().watch_on.reference_name(),
                                                &repo_path,
                                                &trigger_ns,
                                            )
                                            .await
                                    } else {
                                        error!("Unable to get GitRepo {trigger_ns}/{source}: {:?}", gitrepo.err());
                                        continue;
                                    }
                                }
                                TriggerSourceKind::ClusterGitRepo => {
                                    let gitrepo = ClusterGitRepo::get(&source, None);
                                    // - Call repo.fetch_repo_ref(...) to get repository content using particular reference
                                    if let Ok(gitrepo) = gitrepo {
                                        gitrepo
                                            .fetch_repo_ref(
                                                client.clone(),
                                                &trigger.sources().watch_on.reference_name(),
                                                &repo_path,
                                                &trigger_ns,
                                            )
                                            .await
                                    } else {
                                        error!(
                                            "Unable to get ClusterGitRepo {trigger_ns}/{source}: {:?}",
                                            gitrepo.err()
                                        );
                                        continue;
                                    }
                                }
                            };

                            // - Call get_latest_commit_hash(...) to get latest commit
                            if let Ok(repo) = repo {
                                let latest_commit = get_latest_commit(&repo, &trigger.sources().watch_on.reference);
                                if let Ok(latest_commit) = latest_commit {
                                    debug!("Latest commit: {:?}", latest_commit);
                                    new_source_state.commit_hash = Some(latest_commit.to_string());

                                    // - Get file hash if it's required and present
                                    if let Some(path) = &trigger.sources().watch_on.file_ {
                                        let full_path = format!("{repo_path}/{path}");
                                        let is_exists = try_exists(&full_path).await;
                                        if is_exists.is_ok() && is_exists.unwrap() {
                                            let file_hash = get_file_hash(&full_path).await;
                                            if let Ok(file_hash) = file_hash {
                                                debug!("File hash: {file_hash}");
                                                new_source_state.file_hash = Some(file_hash);
                                            } else {
                                                error!("Unable to calc file `{path}` hash: {:?}", file_hash.err());
                                            }
                                        } else {
                                            warn!("File `{path}` doesn't exits");
                                        }
                                    }
                                } else {
                                    error!(
                                        "Unable to get latest commit from GitRepo `{source}`: {:?} ",
                                        latest_commit.err()
                                    );
                                    continue;
                                }
                            } else {
                                error!("Unable to fetch GitRepo `{}` from remote: {:?}", source, repo.err());
                                continue;
                            }

                            // - Get self.status... latest processed hash for current source
                            // - If it differs from latest commit or onChangesOnly==false - run action
                            let current_source_state = checked_sources.get(&source);
                            if !new_source_state
                                .is_equal(current_source_state, trigger.sources().watch_on.file_.is_some())
                                || !trigger.sources().watch_on.on_change_only
                            {
                                // TODO: refactor this part to get "impl executable action" which implements execute(), instead of this match by action's kind
                                let action_exec_result = match trigger.action().kind {
                                    TriggerActionKind::Action => {
                                        let action = Action::get(&trigger.action().name, Some(&trigger_ns));
                                        match action {
                                            Ok(action) => {
                                                action
                                                    .execute(
                                                        &trigger.sources().kind,
                                                        &source,
                                                        &new_source_state.commit_hash.clone().unwrap(),
                                                        &trigger.sources().watch_on.reference,
                                                        &trigger_ns,
                                                    )
                                                    .await
                                            }
                                            Err(err) => Err(err),
                                        }
                                    }
                                    TriggerActionKind::ClusterAction => {
                                        let action = ClusterAction::get(&trigger.action().name, None);
                                        match action {
                                            Ok(action) => {
                                                action
                                                    .execute(
                                                        &trigger.sources().kind,
                                                        &source,
                                                        &new_source_state.commit_hash.clone().unwrap(),
                                                        &trigger.sources().watch_on.reference,
                                                        &trigger_ns,
                                                    )
                                                    .await
                                            }
                                            Err(err) => Err(err),
                                        }
                                    }
                                };

                                // - Update self.spec.... by latest processed commit
                                match action_exec_result {
                                    Ok(_) => {
                                        new_source_state.changed =
                                            Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
                                        let new_source_state: HashMap<String, CheckedSourceState> =
                                            HashMap::from([(source, new_source_state)]);
                                        if let Err(err) = trigger
                                            .update_trigger_status(
                                                Some(TriggerState::Running),
                                                Some(new_source_state),
                                                Some(Utc::now()),
                                                &triggers_api,
                                            )
                                            .await
                                        {
                                            error!("Unable to update trigger `{}` state: {err:?}", trigger_name);
                                        }
                                    }
                                    Err(err) => error!(
                                        "Unable to run {} {}/{}: {}",
                                        trigger.action().kind,
                                        trigger_ns,
                                        trigger.action().name,
                                        err
                                    ),
                                }
                            }
                            // - Remove temp dir content
                            if let Err(err) = remove_dir_all(&repo_path).await {
                                error!("Unable to remove temporary folder `{repo_path}`: {err:?}");
                            }
                        }
                    }
                    // change trigger's state to idle and set last_run time in the status
                    if let Err(err) = trigger
                        .update_trigger_status(Some(TriggerState::Idle), None, Some(Utc::now()), &triggers_api)
                        .await
                    {
                        error!("Unable to update trigger `{}` state: {err:?}", trigger_name);
                    }
                } else {
                    error!("Unable to get Trigger {trigger_ns}/{trigger_name}: {:?}", trigger.err());
                }

                debug!("Finish trigger job: trigger={trigger_ns}/{trigger_name}, job id={id}");
            })
        })
    }
}

/// Returns latest commit of specified reference, depending on reference type
pub fn get_latest_commit(repo: &Repository, reference: &TriggerGitRepoReference) -> Result<Oid> {
    let oid = match reference {
        TriggerGitRepoReference::Branch(r) => repo
            .resolve_reference_from_short_name(&format!("origin/{r}"))
            .map_err(Error::GitrepoAccessError)?
            .target()
            .unwrap(),
        TriggerGitRepoReference::Tag(r) => repo
            .resolve_reference_from_short_name(r)
            .map_err(Error::GitrepoAccessError)?
            .target()
            .unwrap(),
        TriggerGitRepoReference::Commit(r) => Oid::from_str(r).map_err(Error::GitrepoAccessError)?,
    };

    let ref_obj = repo.find_object(oid, None).map_err(Error::GitrepoAccessError)?;
    repo.checkout_tree(&ref_obj, None).map_err(Error::GitrepoAccessError)?;

    Ok(oid)
}

/// Calculates SHA256 hash of the file
async fn get_file_hash(path: &String) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buf: Vec<u8> = Vec::new();

    File::open(&path)
        .await
        .map_err(Error::TriggerFileAccessError)?
        .read_to_end(&mut buf)
        .await
        .map_err(Error::TriggerFileAccessError)?;

    let mut buf = buf.as_slice();
    io::copy(&mut buf, &mut hasher).map_err(Error::TriggerFileAccessError)?;
    let hash = hasher.finalize();

    Ok(format!("{hash:x}"))
}
