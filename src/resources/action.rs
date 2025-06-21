//! Action CRDs
use super::{
    random_string,
    trigger::{TriggerGitRepoReference, TriggerSourceKind},
    CustomApiResource,
};
use crate::{config::RuntimeConfig, jobs::JobsQueue, Result};
use chrono::{DateTime, Local};
use k8s_openapi::{
    api::{
        batch::v1::{Job, JobSpec},
        core::v1::{
            Affinity, Container, EmptyDirVolumeSource, EnvVar, PodSpec, PodTemplateSpec, Toleration, Volume,
            VolumeMount,
        },
    },
    apimachinery::pkg::apis::meta::v1::OwnerReference,
};
use kube::{api::ObjectMeta, CustomResource, Resource, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};
use strum::{Display, EnumString};
use tracing::{info, instrument};

/// Job/pod label for action kind
pub const ACTION_JOB_ACTION_KIND_LABEL: &str = "git-events-runner.rs/action-kind";
/// Job/pod label for action name
pub const ACTION_JOB_ACTION_NAME_LABEL: &str = "git-events-runner.rs/action-name";
/// Job/pod label for source kind
pub const ACTION_JOB_SOURCE_KIND_LABEL: &str = "git-events-runner.rs/source-kind";
/// Job/pod label for source ame
pub const ACTION_JOB_SOURCE_NAME_LABEL: &str = "git-events-runner.rs/source-name";
/// Job/pod label for trigger kind
pub const ACTION_JOB_TRIGGER_KIND_LABEL: &str = "git-events-runner.rs/trigger-kind";
/// Job/pod label for trigger name
pub const ACTION_JOB_TRIGGER_NAME_LABEL: &str = "git-events-runner.rs/trigger-name";

/// Action CRD spec section
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Action",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    doc = "Action custom resource definition",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    source_override: Option<ActionSourceOverride>,
    #[serde(default)]
    action_job: ActionJob,
}

/// ClusterAction CRD spec section
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "ClusterAction",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    doc = "ClusterAction custom resource definition"
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterActionSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    source_override: Option<ClusterActionSourceOverride>,
    #[serde(default)]
    action_job: ActionJob,
}

/// Action source override section
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionSourceOverride {
    kind: TriggerSourceKind,
    name: String,
    reference: TriggerGitRepoReference,
}

/// ClusterAction source override section
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterActionSourceOverride {
    kind: TriggerClusterSourceKind,
    name: String,
    reference: TriggerGitRepoReference,
}

/// Possible kinds of trigger for ClusterAction
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema, EnumString, Display, PartialEq)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]
pub enum TriggerClusterSourceKind {
    /// ClusterGitRepo is allowed only
    #[default]
    ClusterGitRepo,
}

/// Action Job config section
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ActionJob {
    #[serde(skip_serializing_if = "Option::is_none")]
    workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloner_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tolerations: Option<Vec<Toleration>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    affinity: Option<Affinity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_selector: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds_after_finished: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_deadline_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_waiting_timeout_seconds: Option<u64>,
    enable_cloner_debug: bool,
    preserve_git_folder: bool,
}

impl CustomApiResource for Action {
    fn crd_kind() -> &'static str {
        "Action"
    }
}

impl CustomApiResource for ClusterAction {
    fn crd_kind() -> &'static str {
        "ClusterAction"
    }
}

impl ActionExecutor for Action {}
impl ActionExecutor for ClusterAction {}

#[allow(private_bounds)]
pub(crate) trait ActionExecutor: ActionInternals {
    /// Creates K8s Job spec and run an actual job from it
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip_all,
        fields(
            source_kind = %source_kind,
            source_name = %source_name,
            source_commit = %source_commit,
            reference = %trigger_ref
        ))]
    async fn execute(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
        ns: &str,
        trigger_kind: &str,
        trigger_name: &str,
    ) -> Result<()> {
        let job = self.create_job_spec(
            source_kind,
            source_name,
            source_commit,
            trigger_ref,
            ns,
            trigger_kind,
            trigger_name,
        )?;
        info!(namespace = %ns, job = %job.name_any(), "enqueue job");

        let timeout = self
            .action_job_spec()
            .job_waiting_timeout_seconds
            .map(Duration::from_secs);

        JobsQueue::enqueue(job, ns, timeout).await
    }
}

trait ActionInternals: Sized + Resource + CustomApiResource {
    fn action_job_spec(&self) -> ActionJob;
    fn source_override_spec(&self) -> Option<ActionSourceOverride>;
    fn get_owner_reference(&self) -> OwnerReference;

    /// format job name
    fn job_name(&self) -> String {
        let timestamp = DateTime::<Local>::from(SystemTime::now())
            .format("%Y%m%d-%H%M%S")
            .to_string();

        format!("{}-{}-{}", self.name_any(), timestamp, random_string(2)).to_lowercase()
    }

    /// Create K8s Job specification
    #[allow(clippy::too_many_arguments)]
    fn create_job_spec(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
        ns: &str,
        trigger_kind: &str,
        trigger_name: &str,
    ) -> Result<Job> {
        let config = RuntimeConfig::get();
        let action_job = self.action_job_spec();
        let source_override = self.source_override_spec();

        // Create pod metadata if additional labels/annotations is defined
        let pod_template_metadata = if action_job.labels.is_some() || action_job.annotations.is_some() {
            let metadata = ObjectMeta {
                labels: action_job.labels.clone(),
                annotations: action_job.annotations.clone(),
                ..Default::default()
            };

            Some(metadata)
        } else {
            None
        };

        // Fill out custom jobs' labels
        let mut labels: BTreeMap<String, String> = BTreeMap::new();
        labels.insert(ACTION_JOB_ACTION_KIND_LABEL.into(), Self::crd_kind().to_string());
        labels.insert(ACTION_JOB_ACTION_NAME_LABEL.into(), self.name_any());
        labels.insert(ACTION_JOB_SOURCE_KIND_LABEL.into(), source_kind.to_string());
        labels.insert(ACTION_JOB_SOURCE_NAME_LABEL.into(), source_name.into());
        labels.insert(ACTION_JOB_TRIGGER_KIND_LABEL.into(), trigger_kind.into());
        labels.insert(ACTION_JOB_TRIGGER_NAME_LABEL.into(), trigger_name.into());

        // And add additional labels if defined
        if let Some(additional_labels) = action_job.labels {
            labels.extend(additional_labels);
        }

        let args = if let Some(source_override) = source_override {
            self.get_gitrepo_cloner_args(&source_override.kind, &source_override.name, &source_override.reference)
        } else {
            self.get_gitrepo_cloner_args(
                source_kind,
                source_name,
                &TriggerGitRepoReference::Commit(source_commit.into()),
            )
        };

        let service_account = if let Some(sa) = action_job.service_account {
            Some(sa)
        } else {
            RuntimeConfig::get().action.default_service_account.clone()
        };

        let ttl_seconds_after_finished = action_job
            .ttl_seconds_after_finished
            .unwrap_or(RuntimeConfig::get().action.ttl_seconds_after_finished);
        let active_deadline_seconds = action_job
            .active_deadline_seconds
            .unwrap_or(RuntimeConfig::get().action.active_deadline_seconds);

        let job = Job {
            metadata: ObjectMeta {
                name: Some(self.job_name()),
                labels: Some(labels),
                annotations: action_job.annotations,
                owner_references: Some(vec![self.get_owner_reference()]),
                ..Default::default()
            },
            spec: Some(JobSpec {
                backoff_limit: Some(0),
                parallelism: Some(1),
                ttl_seconds_after_finished: Some(ttl_seconds_after_finished),
                active_deadline_seconds: Some(active_deadline_seconds),
                template: PodTemplateSpec {
                    metadata: pod_template_metadata,
                    spec: Some(PodSpec {
                        service_account_name: service_account,
                        node_selector: action_job.node_selector,
                        affinity: action_job.affinity,
                        tolerations: action_job.tolerations,
                        init_containers: Some(vec![Container {
                            name: config.action.containers.cloner.name.clone(),
                            image: Some(
                                action_job
                                    .cloner_image
                                    .unwrap_or(config.action.containers.cloner.image.clone()),
                            ),
                            args: Some(args),
                            volume_mounts: Some(vec![VolumeMount {
                                name: config.action.workdir.volume_name.clone(),
                                mount_path: action_job
                                    .workdir
                                    .clone()
                                    .unwrap_or(config.action.workdir.mount_path.clone()),
                                ..Default::default()
                            }]),
                            ..Default::default()
                        }]),
                        containers: vec![Container {
                            name: config.action.containers.worker.name.clone(),
                            image: Some(
                                action_job
                                    .action_image
                                    .unwrap_or(config.action.containers.worker.image.clone()),
                            ),
                            working_dir: Some(
                                action_job
                                    .workdir
                                    .clone()
                                    .unwrap_or(config.action.workdir.mount_path.clone()),
                            ),
                            command: action_job.command.clone(),
                            args: action_job.args,
                            env: Some(self.get_action_job_envs(
                                source_kind,
                                source_name,
                                source_commit,
                                trigger_ref,
                                ns,
                                trigger_kind,
                                trigger_name,
                            )),
                            volume_mounts: Some(vec![VolumeMount {
                                name: config.action.workdir.volume_name.clone(),
                                mount_path: action_job.workdir.unwrap_or(config.action.workdir.mount_path.clone()),
                                ..Default::default()
                            }]),
                            ..Default::default()
                        }],
                        restart_policy: Some("Never".into()),
                        volumes: Some(vec![Volume {
                            name: config.action.workdir.volume_name.clone(),
                            empty_dir: Some(EmptyDirVolumeSource::default()),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }),
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        Ok(job)
    }

    /// Prepare a list of special environment variables to pass to worker container,
    /// use `format_action_job_env_var` helper to unify approach
    #[allow(clippy::too_many_arguments)]
    fn get_action_job_envs(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
        trigger_ref: &TriggerGitRepoReference,
        ns: &str,
        trigger_kind: &str,
        trigger_name: &str,
    ) -> Vec<EnvVar> {
        let action_job = self.action_job_spec();
        let source_override = self.source_override_spec();

        let (trigger_ref_type, trigger_ref_name) = trigger_ref.to_refname(None);
        let mut envs = vec![
            format_action_job_env_var(
                "WORKDIR",
                &action_job
                    .workdir
                    .unwrap_or(RuntimeConfig::get().action.workdir.mount_path.clone()),
            ),
            format_action_job_env_var("TRIGGER_SOURCE_KIND", &source_kind.to_string()),
            format_action_job_env_var("TRIGGER_SOURCE_NAME", source_name),
            format_action_job_env_var("TRIGGER_SOURCE_COMMIT", source_commit),
            format_action_job_env_var("TRIGGER_SOURCE_REF_TYPE", &trigger_ref_type),
            format_action_job_env_var("TRIGGER_SOURCE_REF_NAME", trigger_ref_name),
            format_action_job_env_var("TRIGGER_KIND", trigger_kind),
            format_action_job_env_var("TRIGGER_NAME", trigger_name),
        ];

        if source_kind == &TriggerSourceKind::GitRepo {
            envs.push(format_action_job_env_var("TRIGGER_SOURCE_NAMESPACE", ns));
        }

        if let Some(source_override) = source_override {
            let (ref_type, ref_name) = source_override.reference.to_refname(None);

            envs.push(format_action_job_env_var(
                "ACTION_SOURCE_KIND",
                &source_override.kind.to_string(),
            ));
            envs.push(format_action_job_env_var("ACTION_SOURCE_NAME", &source_override.name));
            envs.push(format_action_job_env_var("ACTION_SOURCE_REF_TYPE", ref_type.as_str()));
            envs.push(format_action_job_env_var("ACTION_SOURCE_REF_NAME", ref_name));
            if source_override.kind == TriggerSourceKind::GitRepo {
                envs.push(format_action_job_env_var("ACTION_SOURCE_NAMESPACE", ns));
            }
        }

        envs
    }

    /// prepares all needed arguments for cloner container entrypoint
    fn get_gitrepo_cloner_args(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_ref: &TriggerGitRepoReference,
    ) -> Vec<String> {
        let action_job = self.action_job_spec();

        let mut args = vec![
            "--kind".into(),
            source_kind.to_string(),
            "--source".into(),
            source_name.into(),
            "--destination".into(),
            action_job
                .workdir
                .clone()
                .unwrap_or(RuntimeConfig::get().action.workdir.mount_path.clone()),
        ];

        let (ref_type, ref_name) = source_ref.to_refname(Some("--"));
        args.push(ref_type);
        args.push(ref_name.clone());

        if self.action_job_spec().preserve_git_folder {
            args.push("--preserve-git-folder".into());
        }

        if action_job.enable_cloner_debug {
            args.push("--debug".into());
        }

        args
    }
}

impl ActionInternals for ClusterAction {
    fn action_job_spec(&self) -> ActionJob {
        self.spec.action_job.clone()
    }

    fn source_override_spec(&self) -> Option<ActionSourceOverride> {
        // Since ClusterAction restricts a repo type to the ClusterGitRepo only,
        // we create ActionSourceOverride from ClusterActionSourceOverride
        self.spec.source_override.as_ref().map(|value| ActionSourceOverride {
            kind: TriggerSourceKind::ClusterGitRepo,
            name: value.name.clone(),
            reference: value.reference.clone(),
        })
    }

    /// returns reference to action to set as an owner of the Job
    #[cfg(not(test))]
    fn get_owner_reference(&self) -> OwnerReference {
        self.controller_owner_ref(&())
            .expect("unable to get owner reference, looks like a BUG!")
    }

    #[cfg(test)]
    fn get_owner_reference(&self) -> OwnerReference {
        OwnerReference::default()
    }
}

impl ActionInternals for Action {
    fn action_job_spec(&self) -> ActionJob {
        self.spec.action_job.clone()
    }

    fn source_override_spec(&self) -> Option<ActionSourceOverride> {
        self.spec.source_override.clone()
    }

    #[cfg(not(test))]
    fn get_owner_reference(&self) -> OwnerReference {
        self.controller_owner_ref(&())
            .expect("unable to get owner reference, looks like a BUG!")
    }

    #[cfg(test)]
    fn get_owner_reference(&self) -> OwnerReference {
        OwnerReference::default()
    }
}

/// helper to create unified env var.
/// uses configured prefix to attach to each var.
fn format_action_job_env_var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: format!(
            "{}{name}",
            RuntimeConfig::get().action.containers.worker.variables_prefix.clone()
        ),
        value: Some(value.to_owned()),
        value_from: None,
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_yaml_snapshot;
    use k8s_openapi::{
        api::core::v1::{Namespace, PodAffinityTerm, PodAntiAffinity},
        apimachinery::pkg::apis::meta::v1::{LabelSelector, LabelSelectorRequirement},
    };
    use kube::{
        api::{DeleteParams, PostParams},
        Api, Client,
    };
    use tokio::sync::OnceCell;

    use crate::tests;

    use super::*;

    const TEST_DEFAULT_ACTION_NAME: &str = "default-test-action";
    const TEST_CUSTOMIZED_ACTION_NAME: &str = "customized-test-action";
    const TEST_ACTION_NAMESPACE: &str = "actions-test";
    const TEST_SOURCE_NAME: &str = "action-test-source";
    const TEST_SOURCE_COMMIT: &str = "e03087d8f722a423bc13fd31542fb9545da784dd";
    const TEST_TRIGGER_KIND: &str = "ScheduleTrigger";
    const TEST_TRIGGER_NAME: &str = "test-schedule-trigger-name";

    static INITIALIZED: OnceCell<()> = OnceCell::const_new();

    async fn init() -> anyhow::Result<Client> {
        INITIALIZED
            .get_or_init(|| async {
                tests::init_crypto_provider().await;
                let client = tests::get_test_kube_client().await.unwrap();
                create_namespace(client).await;
            })
            .await;

        tests::get_test_kube_client().await
    }

    fn default_action(name: impl Into<String>, namespace: impl Into<String>) -> Action {
        Action {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(namespace.into()),
                ..Default::default()
            },
            spec: ActionSpec::default(),
        }
    }

    fn customized_action_job_spec() -> ActionJob {
        ActionJob {
            workdir: Some("/custom-workdir".into()),
            cloner_image: Some("custom-cloner-image:latest".into()),
            action_image: Some("custom-action-image:latest".into()),
            args: Some(vec!["arg1".into(), "arg2".into()]),
            command: Some(vec!["command1".into(), "command2".into()]),
            service_account: Some("custom-service-account-name".into()),
            annotations: Some(BTreeMap::<String, String>::from([
                ("custom-annotation-key-1".into(), "custom-annotation-value-1".into()),
                ("custom-annotation-key-2".into(), "custom-annotation-value-2".into()),
            ])),
            labels: Some(BTreeMap::<String, String>::from([
                ("custom-label-key-1".into(), "custom-label-value-1".into()),
                ("custom-label-key-2".into(), "custom-label-value-2".into()),
            ])),
            tolerations: Some(vec![
                Toleration {
                    effect: Some("NoSchedule".into()),
                    key: Some("toleration-key-1".into()),
                    operator: Some("In".into()),
                    toleration_seconds: Some(234),
                    value: Some("toleration-value-1".into()),
                },
                Toleration {
                    effect: Some("NoSchedule".into()),
                    key: Some("toleration-key-2".into()),
                    operator: Some("In".into()),
                    toleration_seconds: Some(345),
                    value: Some("toleration-value-2".into()),
                },
            ]),
            affinity: Some(Affinity {
                node_affinity: None,
                pod_affinity: None,
                pod_anti_affinity: Some(PodAntiAffinity {
                    preferred_during_scheduling_ignored_during_execution: None,
                    required_during_scheduling_ignored_during_execution: Some(vec![PodAffinityTerm {
                        label_selector: Some(LabelSelector {
                            match_expressions: Some(vec![LabelSelectorRequirement {
                                key: "selector-key".into(),
                                operator: "In".into(),
                                values: Some(vec!["selector-value-1".into(), "selector-value-2".into()]),
                            }]),
                            match_labels: Some(BTreeMap::<String, String>::from([
                                ("custom-label-key-3".into(), "custom-label-value-3".into()),
                                ("custom-label-key-4".into(), "custom-label-value-4".into()),
                            ])),
                        }),
                        namespace_selector: None,
                        namespaces: Some(vec!["ns1".into(), "ns2".into()]),
                        topology_key: "kubernetes.io/hostname".into(),
                        match_label_keys: None,
                        mismatch_label_keys: None,
                    }]),
                }),
            }),
            node_selector: Some(BTreeMap::<String, String>::from([
                ("custom-node-label-key-1".into(), "custom-node-label-value-1".into()),
                ("custom-node-label-key-2".into(), "custom-node-label-value-2".into()),
            ])),
            ttl_seconds_after_finished: Some(231),
            active_deadline_seconds: Some(321),
            job_waiting_timeout_seconds: Some(123),
            enable_cloner_debug: true,
            preserve_git_folder: true,
        }
    }

    fn customized_action(name: impl Into<String>, namespace: impl Into<String>) -> Action {
        Action {
            metadata: ObjectMeta {
                name: Some(name.into()),
                namespace: Some(namespace.into()),
                ..Default::default()
            },
            spec: ActionSpec {
                source_override: Some(ActionSourceOverride {
                    kind: TriggerSourceKind::GitRepo,
                    name: "git-repo-override".into(),
                    reference: TriggerGitRepoReference::Branch("overridden-branch".into()),
                }),
                action_job: customized_action_job_spec(),
            },
        }
    }

    fn default_cluster_action(name: impl Into<String>) -> ClusterAction {
        ClusterAction {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: ClusterActionSpec::default(),
        }
    }

    fn customized_cluster_action(name: impl Into<String>) -> ClusterAction {
        ClusterAction {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: ClusterActionSpec {
                source_override: Some(ClusterActionSourceOverride {
                    kind: TriggerClusterSourceKind::ClusterGitRepo,
                    name: "git-repo-override".into(),
                    reference: TriggerGitRepoReference::Branch("overridden-branch".into()),
                }),
                action_job: customized_action_job_spec(),
            },
        }
    }

    /// Unattended namespace creation
    async fn create_namespace(client: Client) {
        let api = Api::<Namespace>::all(client);
        let pp = PostParams::default();

        let mut data = Namespace::default();
        data.meta_mut().name = Some(String::from(TEST_ACTION_NAMESPACE));
        api.create(&pp, &data).await.unwrap_or_default();
    }

    #[test]
    fn correct_env_var_prefix() {
        let var = format_action_job_env_var("TEST_VAR", "test_value");
        let prefix = RuntimeConfig::get().action.containers.worker.variables_prefix.clone();
        assert_eq!(var.name, format!("{prefix}TEST_VAR"));
        assert_eq!(var.value, Some(String::from("test_value")));
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn default_action_job() {
        let client = init().await.unwrap();
        let api = Api::<Action>::namespaced(client, TEST_ACTION_NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create test action
        let action = default_action(TEST_DEFAULT_ACTION_NAME, TEST_ACTION_NAMESPACE);
        api.create(&pp, &action).await.unwrap();

        for reference in [
            TriggerGitRepoReference::Branch("main".into()),
            TriggerGitRepoReference::Tag("tag".into()),
            TriggerGitRepoReference::Commit(TEST_SOURCE_COMMIT.into()),
        ] {
            // Assert job: GitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::GitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_DEFAULT_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});

            // Assert job: ClusterGitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::ClusterGitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_DEFAULT_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});
        }

        // Clean up
        let _ = api.delete(TEST_DEFAULT_ACTION_NAME, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn customized_action_job() {
        let client = init().await.unwrap();
        let api = Api::<Action>::namespaced(client, TEST_ACTION_NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create test action
        let action = customized_action(TEST_CUSTOMIZED_ACTION_NAME, TEST_ACTION_NAMESPACE);
        api.create(&pp, &action).await.unwrap();

        for reference in [
            TriggerGitRepoReference::Branch("main".into()),
            TriggerGitRepoReference::Tag("tag".into()),
            TriggerGitRepoReference::Commit(TEST_SOURCE_COMMIT.into()),
        ] {
            // Assert job: GitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::GitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_CUSTOMIZED_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});

            // Assert job: ClusterGitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::ClusterGitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_CUSTOMIZED_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});
        }

        // Clean up
        let _ = api.delete(TEST_CUSTOMIZED_ACTION_NAME, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn default_cluster_action_job() {
        let client = init().await.unwrap();
        let api = Api::<ClusterAction>::all(client);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create test action
        let action = default_cluster_action(TEST_DEFAULT_ACTION_NAME);
        api.create(&pp, &action).await.unwrap();

        for reference in [
            TriggerGitRepoReference::Branch("main".into()),
            TriggerGitRepoReference::Tag("tag".into()),
            TriggerGitRepoReference::Commit(TEST_SOURCE_COMMIT.into()),
        ] {
            // Assert job: GitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::GitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_DEFAULT_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});

            // Assert job: ClusterGitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::ClusterGitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_DEFAULT_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});
        }
        // Clean up
        let _ = api.delete(TEST_DEFAULT_ACTION_NAME, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "needs docker"]
    async fn customized_cluster_action_job() {
        let client = init().await.unwrap();
        let api = Api::<ClusterAction>::all(client);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create test action
        let action = customized_cluster_action(TEST_CUSTOMIZED_ACTION_NAME);
        api.create(&pp, &action).await.unwrap();

        for reference in [
            TriggerGitRepoReference::Branch("main".into()),
            TriggerGitRepoReference::Tag("tag".into()),
            TriggerGitRepoReference::Commit(TEST_SOURCE_COMMIT.into()),
        ] {
            // Assert job: GitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::GitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_CUSTOMIZED_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});

            // Assert job: ClusterGitRepo
            let job_spec = action
                .create_job_spec(
                    &TriggerSourceKind::ClusterGitRepo,
                    TEST_SOURCE_NAME,
                    TEST_SOURCE_COMMIT,
                    &reference,
                    TEST_ACTION_NAMESPACE,
                    TEST_TRIGGER_KIND,
                    TEST_TRIGGER_NAME,
                )
                .unwrap();
            assert_yaml_snapshot!(job_spec, {".metadata.name" => insta::dynamic_redaction(|_, _| {format!("{TEST_CUSTOMIZED_ACTION_NAME}-YYYYMMDD-HHMMSS-xx")})});
        }
        // Clean up
        let _ = api.delete(TEST_CUSTOMIZED_ACTION_NAME, &dp).await.unwrap();
    }
}
