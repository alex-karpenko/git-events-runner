use super::{Context, SecretRef, TriggersState};
use crate::{
    controllers::{API_GROUP, CURRENT_API_VERSION},
    Error, GitRepo, Result, TriggerGitRepoReference, TriggerSourceKind,
};
use k8s_openapi::api::core::v1::Secret;
use kube::{
    api::{Patch, PatchParams},
    core::object::HasStatus,
    runtime::{
        controller::Action as ReconcileAction,
        events::{Event, EventType, Recorder},
        finalizer::{finalizer, Event as Finalizer},
    },
    Api, Client, CustomResource, ResourceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use strum_macros::{Display, EnumString};
use tracing::{debug, error, info, warn};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema, PartialEq)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Action",
    group = "git-events-runner.rs",
    version = "v1alpha1",
    namespaced
)]
#[kube(status = "ActionStatus")]
#[serde(rename_all = "camelCase")]
pub struct ActionSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    source_override: Option<ActionSourceOverride>,
}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionStatus {}

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActionSourceOverride {}

pub(crate) async fn reconcile(action: Arc<Action>, ctx: Arc<Context>) -> Result<ReconcileAction> {
    let ns = action.namespace().unwrap();
    let actions: Api<Action> = Api::namespaced(ctx.client.clone(), &ns);

    debug!("{:#?}", action);
    info!("Reconciling action `{}` in {}", action.name_any(), ns);
    finalizer(
        &actions,
        "actions.git-events-runner.rs",
        action,
        |event| async {
            match event {
                Finalizer::Apply(action) => action.reconcile(ctx.clone()).await,
                Finalizer::Cleanup(action) => action.cleanup(ctx.clone()).await,
            }
        },
    )
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

pub(crate) fn error_policy(
    _action: Arc<Action>,
    error: &Error,
    _ctx: Arc<Context>,
) -> ReconcileAction {
    warn!("reconcile failed: {:?}", error);
    ReconcileAction::await_change()
}

impl Action {
    fn hash_key(&self) -> String {
        format!("{}/{}", self.namespace().unwrap(), self.name_any())
    }

    // Reconcile (for non-finalizer related changes)
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        let client = ctx.client.clone();
        let recorder = &ctx.diagnostics.read().await.recorder(client.clone(), self);
        let ns = self.namespace().unwrap();
        let actions_api: Api<Action> = Api::namespaced(client.clone(), &ns);

        // If no events were received, check back 30 minutes
        Ok(ReconcileAction::requeue(Duration::from_secs(30 * 60)))
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<ReconcileAction> {
        info!(
            "Cleanup Action `{}` in {}",
            self.name_any(),
            self.namespace().unwrap()
        );
        Ok(ReconcileAction::await_change())
    }

    pub(crate) async fn execute(
        &self,
        source_kind: &TriggerSourceKind,
        source_name: &str,
        source_commit: &str,
    ) -> Result<()> {
        warn!("TODO: Create job for action");
        Ok(())
    }
}
