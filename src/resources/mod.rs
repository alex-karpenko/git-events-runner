//! Custom resources definitions
pub mod action;
pub mod git_repo;
pub mod trigger;

use crate::{controller::Context, Result};
use action::{Action, ClusterAction};
use git_repo::{ClusterGitRepo, GitRepo};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::{runtime::controller::Action as ReconcileAction, CustomResourceExt};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::sync::Arc;
use trigger::{ScheduleTrigger, WebhookTrigger};

const API_GROUP: &str = "git-events-runner.rs";
const CURRENT_API_VERSION: &str = "v1alpha1";

pub(crate) trait CustomApiResource {
    fn crd_kind() -> &'static str;
}

/// Behavior of all CRDs that have external state and should be able to reconcile
#[allow(async_fn_in_trait)]
pub trait Reconcilable<S> {
    /// Reconciles `self`
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction>;
    /// Finalizes `self`
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<ReconcileAction>;
    /// Returns finalizer name
    fn finalizer_name(&self) -> Option<&'static str> {
        None
    }
}

pub(crate) fn random_string(len: usize) -> String {
    let rand: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect();
    rand
}

/// Return list with all CRD definitions
pub fn get_all_crds() -> Vec<CustomResourceDefinition> {
    vec![
        GitRepo::crd(),
        ClusterGitRepo::crd(),
        ScheduleTrigger::crd(),
        WebhookTrigger::crd(),
        Action::crd(),
        ClusterAction::crd(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ensure_100_4ch_random_strings() {
        const SET_SIZE: usize = 100;

        let mut control_set = HashSet::new();
        for _ in 0..SET_SIZE {
            control_set.insert(random_string(4));
        }

        assert_eq!(control_set.len(), SET_SIZE);
    }
}
