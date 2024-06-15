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
use globwalk::{FileType, GlobWalkerBuilder};
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
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};
use strum_macros::{Display, EnumString};
use tokio::{
    fs::{create_dir_all, remove_dir_all, File},
    io::AsyncReadExt,
};
use tracing::{debug, debug_span, error, info, instrument};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
}

impl Default for TriggerWatchOn {
    fn default() -> Self {
        Self {
            on_change_only: true,
            reference: Default::default(),
            files: None,
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

impl std::fmt::Display for TriggerGitRepoReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (ref_type, ref_name) = self.to_refname(None);
        write!(f, "{ref_type}/{ref_name}")
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
/// all the defined ones or single one only.
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
    #[instrument(level = "debug", skip_all,
        fields(
            kind=self.kind(),
            namespace=self.namespace().unwrap(),
            trigger=self.name_any()
        )
    )]
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        let trigger_hash_key = self.trigger_hash_key();

        // Reconcile if schedule has been changed
        if Some(&self.spec.schedule) != ctx.triggers.read().await.schedules.get(&trigger_hash_key) {
            let mut triggers = ctx.triggers.write().await;
            debug!(trigger = %trigger_hash_key, "updating ScheduleTrigger");

            // Cancel the existing task if it's present.
            if triggers.tasks.contains_key(&trigger_hash_key) {
                let task_id = triggers.tasks.remove(&trigger_hash_key).unwrap();
                let scheduler = ctx.scheduler.write().await;
                let res = scheduler.cancel(task_id, CancelOpts::Ignore).await;
                if res.is_err() {
                    error!(error = ?res.err(), "canceling task");
                }
            }

            // Get a time of last triggers' run if it was run ever.
            let last_run: Option<SystemTime> = if let Some(status) = self.status() {
                status.last_run.as_ref().map(|last_run| {
                    DateTime::parse_from_rfc3339(last_run)
                        .expect("Incorrect time format in LastRun field, looks like a BUG.")
                        .into()
                })
            } else {
                None
            };

            // Get a new schedule for the new task.
            match self.get_task_schedule(last_run) {
                Ok(schedule) => {
                    // Add a new task to the scheduler instead of the canceled one.
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

    #[instrument(level = "debug", skip_all,
        fields(
            kind=self.kind(),
            namespace=self.namespace().unwrap(),
            trigger=self.name_any()
        )
    )]
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        info!(
            kind = %self.kind(),
            namespace = %self.namespace().expect("unable to get resource namespace, looks like a BUG!"),
            trigger = %self.name_any(),
            "cleaning up"
        );
        let trigger_hash_key = self.trigger_hash_key();
        // Remove from the schedule map.
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
                error!(error = ?res.err(), "canceling task");
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
    <Self as Resource>::DynamicType: std::hash::Hash + Eq + Clone,
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
    /// If checked_sources is provided in parameters, then merge it with the existing map.
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
                // Status is already present in the trigger.
                // Use the provided checked_sources map as an addition to the existing ones.
                let mut new_checked_sources = status.checked_sources.clone();
                new_checked_sources.extend(checked_sources.unwrap_or_default());

                // Filter checked_sources to exclude non-existing sources
                let new_checked_sources: HashMap<String, CheckedSourceState> = new_checked_sources
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
                    checked_sources: new_checked_sources,
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
            let pp = PatchParams::apply("controller").force();
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
            // All these values should be moved to the task.
            let trigger_name = trigger_name.clone();
            let trigger_ns = trigger_ns.clone();
            let client = client.clone();
            let task_sources = task_sources.clone();
            let source_clone_folder = source_clone_folder.clone();

            // Actual trigger job
            Box::pin(async move {
                let span = debug_span!("trigger task", id = id.to_string());
                let _span = span.enter();

                debug!(namespace = %trigger_ns, trigger = %trigger_name, job_id = %id, "starting trigger job");
                let triggers_api: Api<Self> = Api::namespaced(client.clone(), &trigger_ns);
                // Get current trigger from cache
                let trigger = Self::get(&trigger_name, Some(&trigger_ns));

                if let Ok(trigger) = trigger {
                    // This is a base folder to clone sources to.
                    // Add a random suffix because some webhook trigger may have the same name
                    let base_temp_dir =
                        format!("{source_clone_folder}/{trigger_ns}/{trigger_name}/{}", random_string(8));

                    // Filter checked_sources to exclude non-existing sources,
                    // this is possible state in between of the last trigger update via API,
                    // and the following task run so just exclude non-existing sources from the current status.
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
                        error!(trigger = % trigger_name, error = %err, "updating trigger state");
                    }

                    // Create a set of the sources to check.
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
                        debug!(namespace = %trigger_ns, trigger = %trigger_name, kind = %trigger.sources().kind, source = %source, "processing source");

                        // - Create temp dest folder
                        let repo_path = format!("{base_temp_dir}/{source}");
                        let _ = remove_dir_all(&repo_path).await; // to prevent error if some leftovers exist
                        if let Err(err) = create_dir_all(&repo_path).await {
                            error!(path = %repo_path, error = %err, "creating temporary folder");
                        } else {
                            // TODO: refactor this part to obtain "impl trait" object which implements fetch_repo_ref() instead of kind
                            // and just use it to clone repo
                            let repo = match trigger.sources().kind {
                                // - Get GitRepo object
                                TriggerSourceKind::GitRepo => {
                                    let gitrepo = GitRepo::get(&source, Some(&trigger_ns));
                                    // - Call repo.fetch_repo_ref(...)
                                    // to get repository content
                                    // using a particular reference
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
                                        error!(namespace = %trigger_ns, source = %source, error = ?gitrepo.err(), "unable to get GitRepo");
                                        continue;
                                    }
                                }
                                TriggerSourceKind::ClusterGitRepo => {
                                    let gitrepo = ClusterGitRepo::get(&source, None);
                                    // Call repo.fetch_repo_ref(...)
                                    // to get repository content
                                    // using a particular reference
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
                                        error!(namespace = %trigger_ns, source = %source, error = ?gitrepo.err(), "unable to get ClusterGitRepo");
                                        continue;
                                    }
                                }
                            };

                            // - Call get_latest_commit_hash(...) to get latest commit
                            if let Ok(repo) = repo {
                                let latest_commit = get_latest_commit(&repo, &trigger.sources().watch_on.reference);
                                if let Ok(latest_commit) = latest_commit {
                                    debug!(hash = %latest_commit, "latest commit");
                                    new_source_state.commit_hash = Some(latest_commit.to_string());

                                    // - Get file hash if it's required and present
                                    if let Some(files) = &trigger.sources().watch_on.files {
                                        let file_hash = get_file_glob_hash(&repo_path, files).await;
                                        if let Ok(file_hash) = file_hash {
                                            let hex_hash = hex::encode(file_hash);
                                            debug!(hash = hex_hash, "checking files content");
                                            new_source_state.file_hash = Some(hex_hash);
                                        } else {
                                            error!(error = ?file_hash.err(), "calculating files hash");
                                        }
                                    }
                                } else {
                                    error!(source = %source, error = ?latest_commit.err(), "getting latest commit from GitRepo");
                                    continue;
                                }
                            } else {
                                error!(source = %source, error = ?repo.err(), "fetching GitRepo from remote");
                                continue;
                            }

                            // Get self.status... latest processed hash for the current source.
                            // If it differs from the latest commit or onChangesOnly==false - run action.
                            let current_source_state = checked_sources.get(&source);
                            if !new_source_state
                                .is_equal(current_source_state, trigger.sources().watch_on.files.is_some())
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

                                // - Update self.spec... by the latest processed commit
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
                                            error!(trigger = %trigger_name, error = %err, "updating trigger state");
                                        }
                                    }
                                    Err(err) => error!(
                                        kind = %trigger.action().kind, namespace = %trigger_ns, action = %trigger.action().name, error = %err, "running action"),
                                }
                            } else {
                                if let Some(current_source_state) = checked_sources.get(&source) {
                                    new_source_state.changed.clone_from(&current_source_state.changed);
                                }
                                let new_source_state: HashMap<String, CheckedSourceState> =
                                    HashMap::from([(source, new_source_state)]);
                                if let Err(err) = trigger
                                    .update_trigger_status(None, Some(new_source_state), None, &triggers_api)
                                    .await
                                {
                                    error!(trigger = %trigger_name, error = %err, "updating trigger state");
                                }
                            }
                            // - Remove temp dir content
                            if let Err(err) = remove_dir_all(&repo_path).await {
                                error!(path = %repo_path, error = %err, "removing temporary folder");
                            }
                        }
                    }
                    // change trigger's state to idle and set last_run time in the status
                    if let Err(err) = trigger
                        .update_trigger_status(Some(TriggerState::Idle), None, Some(Utc::now()), &triggers_api)
                        .await
                    {
                        error!(trigger = %trigger_name, error = %err, "updating trigger state");
                    }
                } else {
                    error!(namespace = %trigger_ns, trigger = %trigger_name, error = ?trigger.err(), "getting Trigger");
                }

                debug!(namespace = %trigger_ns, trigger = %trigger_name, job_id = %id, "finishing trigger job");
            })
        })
    }
}

/// Returns the latest commit of a specified reference, depending on the reference type.
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
async fn get_file_hash(path: &Path) -> Result<Vec<u8>> {
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
    let hash = hasher.finalize().to_vec();

    Ok(hash)
}

/// Calculates SHA256 hash of files selected by glob
async fn get_file_glob_hash(folder: &String, glob: &[String]) -> Result<Vec<u8>> {
    let mut hasher = Sha256::new();
    let glob = GlobWalkerBuilder::from_patterns(folder, glob)
        .file_type(FileType::FILE)
        .sort_by(|p1, p2| p1.path().cmp(p2.path()))
        .build()?;

    for file_name in glob {
        let file_name = file_name.map_err(|e| Error::TriggerDirWalkError(e.to_string()))?;
        let file_path = file_name.path();

        let hash = get_file_hash(file_path).await?;
        hasher.update(hash);
    }

    let hash = hasher.finalize().to_vec();
    Ok(hash)
}
