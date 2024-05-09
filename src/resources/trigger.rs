use crate::{
    cache::ApiCache,
    controller::{Context, CustomApiResource, Reconcilable, API_GROUP, CURRENT_API_VERSION},
    resources::{
        action::{Action, ActionExecutor, ClusterAction},
        git_repo::{ClusterGitRepo, GitRepo, GitRepoGetter},
        random_string,
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

#[derive(
    Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq,
)]
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

#[derive(
    Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq,
)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]
pub enum TriggerActionKind {
    #[default]
    Action,
    ClusterAction,
}

impl Trigger<ScheduleTriggerSpec> for ScheduleTrigger {
    fn sources(&self) -> &TriggerSources {
        &self.spec.sources
    }

    fn action(&self) -> &TriggerAction {
        &self.spec.action
    }
}

pub(crate) trait HasTriggerStatus {
    fn trigger_status(&self) -> &Option<TriggerStatus>;
}

impl HasTriggerStatus for ScheduleTrigger {
    fn trigger_status(&self) -> &Option<TriggerStatus> {
        &self.status
    }
}

impl HasTriggerStatus for WebhookTrigger {
    fn trigger_status(&self) -> &Option<TriggerStatus> {
        &self.status
    }
}

impl Reconcilable<ScheduleTriggerSpec> for ScheduleTrigger {
    async fn reconcile(&self, ctx: Arc<Context<ScheduleTriggerSpec>>) -> Result<ReconcileAction> {
        let client = ctx.client.clone();
        let recorder = &ctx.diagnostics.read().await.recorder(client.clone(), self);
        let trigger_key = self.trigger_hash_key();

        // Reconcile if something has been changed
        if Some(&self.spec) != ctx.triggers.read().await.specs.get(&trigger_key) {
            // What is changed?
            let is_changed = {
                let old_trigger = ctx.triggers.read().await;
                let old_trigger = old_trigger.specs.get(&trigger_key);
                old_trigger.is_none()
                    || old_trigger.unwrap().sources != self.spec.sources
                    || old_trigger.unwrap().action != self.spec.action
                    || old_trigger.unwrap().schedule != self.spec.schedule
            };

            // Trigger has been changed
            let mut triggers = ctx.triggers.write().await;
            if is_changed {
                debug!("Update ScheduleTrigger `{trigger_key}`");

                // drop existing task if it was
                if triggers.tasks.contains_key(&trigger_key) {
                    let task_id = triggers.tasks.remove(&trigger_key).unwrap();
                    let scheduler = ctx.scheduler.write().await;
                    let res = scheduler.cancel(task_id, CancelOpts::Ignore).await;
                    if res.is_err() {
                        error!("Can't cancel task: {res:?}");
                    }
                }

                // Get time of trigger last run, if it was run
                let last_run: Option<SystemTime> = if let Some(status) = self.status() {
                    status.last_run.as_ref().map(|last_run| {
                        DateTime::parse_from_rfc3339(last_run)
                            .expect("Incorrect time format in LastRun field, looks like a BUG.")
                            .into()
                    })
                } else {
                    None
                };

                // Add new task
                if let Some(schedule) = self.get_task_schedule(last_run) {
                    let scheduler = ctx.scheduler.write().await;
                    let task = self.create_trigger_task(
                        client.clone(),
                        schedule,
                        None,
                        ctx.cli_config.source_clone_folder.clone(),
                        ctx.identity.clone(),
                    );
                    let task_id = scheduler.add(task).await?;
                    let tasks = &mut triggers.tasks;
                    tasks.insert(self.trigger_hash_key(), task_id);
                } else {
                    // Invalid schedule
                    self.publish_trigger_validation_event(
                        recorder,
                        EventType::Warning,
                        "Unable to parse schedule expression, scheduler is disabled",
                        "ValidateTriggerConfig",
                    )
                    .await?;
                }
            }

            // Update trigger spec in the map
            triggers
                .specs
                .insert(self.trigger_hash_key(), self.spec.clone());
        }

        Ok(ReconcileAction::await_change())
    }

    async fn cleanup(&self, ctx: Arc<Context<ScheduleTriggerSpec>>) -> Result<ReconcileAction> {
        info!(
            "Cleanup {} `{}` in {}",
            self.kind(),
            self.name_any(),
            self.namespace()
                .expect("unable to get resource namespace, looks like a BUG!")
        );
        let trigger_key = self.trigger_hash_key();
        // Remove from triggers map
        if ctx.triggers.read().await.specs.contains_key(&trigger_key) {
            let mut triggers = ctx.triggers.write().await;
            triggers.specs.remove(&trigger_key);
        }
        // Drop task and remove from tasks map
        if ctx.triggers.read().await.tasks.contains_key(&trigger_key) {
            let mut triggers = ctx.triggers.write().await;
            let task_id = triggers.tasks.remove(&trigger_key).unwrap();
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

impl Trigger<WebhookTriggerSpec> for WebhookTrigger {
    fn sources(&self) -> &TriggerSources {
        &self.spec.sources
    }

    fn action(&self) -> &TriggerAction {
        &self.spec.action
    }
}

impl ScheduleTrigger {
    fn get_task_schedule(&self, last_run: Option<SystemTime>) -> Option<TaskSchedule> {
        if self.spec.schedule.is_valid() {
            let schedule = match &self.spec.schedule {
                TriggerSchedule::Interval(interval) => {
                    let interval: Duration = interval
                        .parse::<humantime::Duration>()
                        .expect("unable to parse time interval, looks like a BUG")
                        .into();
                    if let Some(last_run) = last_run {
                        let since_last_run = SystemTime::now()
                            .duration_since(last_run)
                            .expect("wrong time of trigger last run, looks like a BUG");
                        let delay = interval
                            .checked_sub(since_last_run)
                            .unwrap_or(Duration::from_secs(0));
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
            Some(schedule)
        } else {
            None
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

pub(crate) trait Trigger<S>
where
    Self: 'static + Send + Sync + ApiCache,
    Self: Resource + ResourceExt + CustomApiResource + HasTriggerStatus,
    Self: std::fmt::Debug + Clone + DeserializeOwned + Serialize,
    <Self as Resource>::DynamicType: Default,
    Self: Resource<Scope = NamespaceResourceScope>,
    <Self as kube::Resource>::DynamicType: std::hash::Hash + std::cmp::Eq + Clone,
    S: Send + Sync + 'static,
{
    fn sources(&self) -> &TriggerSources;
    fn action(&self) -> &TriggerAction;

    fn trigger_hash_key(&self) -> String {
        format!(
            "{}/{}",
            self.namespace()
                .expect("unable to get resource namespace, looks like a BUG!"),
            self.name_any()
        )
    }

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
                TriggerStatus {
                    state: state.unwrap_or_default(),
                    last_run: last_run
                        .map(|last_run| last_run.to_rfc3339_opts(SecondsFormat::Secs, true)),
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
        source: Option<String>,
        source_clone_folder: String,
        identity: String,
    ) -> Task {
        let trigger_name = self.name_any();
        let trigger_ns = self
            .namespace()
            .expect("unable to get resource namespace, looks like a BUG!");
        Task::new(schedule, move |id| {
            let trigger_name = trigger_name.clone();
            let trigger_ns = trigger_ns.clone();
            let client = client.clone();
            let source = source.clone();
            let source_clone_folder = source_clone_folder.clone();
            let identity = identity.clone();
            Box::pin(async move {
                debug!("Start trigger job: trigger={trigger_ns}/{trigger_name}, job id={id}");
                let triggers_api: Api<Self> = Api::namespaced(client.clone(), &trigger_ns);
                // Get current trigger from cache
                let trigger = Self::get(&trigger_name, Some(&trigger_ns));

                if let Ok(trigger) = trigger {
                    let base_temp_dir = format!(
                        "{source_clone_folder}/{trigger_ns}/{trigger_name}/{}",
                        random_string(8)
                    );

                    // Filter checked_sources to exclude non-existing sources
                    let checked_sources: HashMap<String, CheckedSourceState> = trigger
                        .trigger_status()
                        .clone()
                        .unwrap_or_default()
                        .checked_sources
                        .into_iter()
                        .filter(|(k, _v)| trigger.sources().names.contains(k))
                        .collect();

                    if let Err(err) = trigger
                        .update_trigger_status(
                            Some(TriggerState::Running),
                            None,
                            None,
                            &triggers_api,
                        )
                        .await
                    {
                        error!("Unable to update trigger `{}` state: {err:?}", trigger_name);
                    }

                    // Create set of sources
                    let sources_to_check = if let Some(source) = source {
                        HashSet::from([source])
                    } else {
                        trigger
                            .sources()
                            .names
                            .iter()
                            .cloned()
                            .collect::<HashSet<String>>()
                    };
                    // For each source
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
                        let _ = remove_dir_all(&repo_path).await; // to prevent error if folder exists by mistake
                        if let Err(err) = create_dir_all(&repo_path).await {
                            error!("Unable to create temporary folder `{repo_path}`: {err:?}");
                        } else {
                            let repo = match trigger.sources().kind {
                                // - Get GitRepo object
                                TriggerSourceKind::GitRepo => {
                                    let gitrepo = GitRepo::get(&source, Some(&trigger_ns));
                                    // - Call repo.fetch_repo_ref(...) to get repository
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
                                            "Unable to get GitRepo {trigger_ns}/{source}: {:?}",
                                            gitrepo.err()
                                        );
                                        continue;
                                    }
                                }
                                TriggerSourceKind::ClusterGitRepo => {
                                    let gitrepo = ClusterGitRepo::get(&source, None);
                                    // - Call repo.fetch_repo_ref(...) to get repository
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
                                            "Unable to get GitRepo {trigger_ns}/{source}: {:?}",
                                            gitrepo.err()
                                        );
                                        continue;
                                    }
                                }
                            };

                            // - Call get_latest_commit_hash(...) to get latest commit
                            if let Ok(repo) = repo {
                                let latest_commit =
                                    get_latest_commit(&repo, &trigger.sources().watch_on.reference);
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
                                                error!(
                                                    "Unable to calc file `{path}` hash: {:?}",
                                                    file_hash.err()
                                                );
                                            }
                                        } else {
                                            warn!("File `{path}` doesn't exits");
                                        }
                                    }
                                } else {
                                    error!("Unable to get latest commit from GitRepo `{source}`: {:?} ", latest_commit.err());
                                    continue;
                                }
                            } else {
                                error!(
                                    "Unable to fetch GitRepo `{}` from remote: {:?}",
                                    source,
                                    repo.err()
                                );
                                continue;
                            }

                            // - Get self.status... latest processed hash for current source
                            // - If it differs from latest commit or onChangesOnly==false - run action
                            let current_source_state = checked_sources.get(&source);
                            if !new_source_state.is_equal(
                                current_source_state,
                                trigger.sources().watch_on.file_.is_some(),
                            ) || !trigger.sources().watch_on.on_change_only
                            {
                                let action_exec_result = match trigger.action().kind {
                                    TriggerActionKind::Action => {
                                        let action =
                                            Action::get(&trigger.action().name, Some(&trigger_ns));
                                        match action {
                                            Ok(action) => {
                                                // TODO: Should we retry in case of error?
                                                action
                                                    .execute(
                                                        &trigger.sources().kind,
                                                        &source,
                                                        &new_source_state
                                                            .commit_hash
                                                            .clone()
                                                            .unwrap(),
                                                        &trigger.sources().watch_on.reference,
                                                        client.clone(),
                                                        &trigger_ns,
                                                        identity.clone(),
                                                    )
                                                    .await
                                            }
                                            Err(err) => Err(err),
                                        }
                                    }
                                    TriggerActionKind::ClusterAction => {
                                        let action =
                                            ClusterAction::get(&trigger.action().name, None);
                                        match action {
                                            Ok(action) => {
                                                // TODO: Should we retry in case of error?
                                                action
                                                    .execute(
                                                        &trigger.sources().kind,
                                                        &source,
                                                        &new_source_state
                                                            .commit_hash
                                                            .clone()
                                                            .unwrap(),
                                                        &trigger.sources().watch_on.reference,
                                                        client.clone(),
                                                        &trigger_ns,
                                                        identity.clone(),
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
                                        new_source_state.changed = Some(
                                            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                                        );
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
                                            error!(
                                                "Unable to update trigger `{}` state: {err:?}",
                                                trigger_name
                                            );
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
                    if let Err(err) = trigger
                        .update_trigger_status(
                            Some(TriggerState::Idle),
                            None,
                            Some(Utc::now()),
                            &triggers_api,
                        )
                        .await
                    {
                        error!("Unable to update trigger `{}` state: {err:?}", trigger_name);
                    }
                } else {
                    error!(
                        "Unable to get Trigger {trigger_ns}/{trigger_name}: {:?}",
                        trigger.err()
                    );
                }

                debug!("Finish trigger job: trigger={trigger_ns}/{trigger_name}, job id={id}");
            })
        })
    }
}

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
        TriggerGitRepoReference::Commit(r) => {
            Oid::from_str(r).map_err(Error::GitrepoAccessError)?
        }
    };

    let ref_obj = repo
        .find_object(oid, None)
        .map_err(Error::GitrepoAccessError)?;
    repo.checkout_tree(&ref_obj, None)
        .map_err(Error::GitrepoAccessError)?;

    Ok(oid)
}

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
