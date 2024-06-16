pub mod action;
pub mod git_repo;
pub mod trigger;

use crate::{controller::Context, Result};
use kube::runtime::controller::Action as ReconcileAction;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::sync::Arc;

const API_GROUP: &str = "git-events-runner.rs";
const CURRENT_API_VERSION: &str = "v1alpha1";

pub(crate) trait CustomApiResource {
    fn crd_kind() -> &'static str;
}

#[allow(async_fn_in_trait)]
pub trait Reconcilable<S> {
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<ReconcileAction>;
    async fn cleanup(&self, ctx: Arc<Context>) -> Result<ReconcileAction>;
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
