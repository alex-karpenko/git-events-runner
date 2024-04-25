pub mod action;
pub mod git_repo;
pub mod trigger;

use rand::{distributions::Alphanumeric, thread_rng, Rng};

pub(crate) fn random_string(len: usize) -> String {
    let rand: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect();
    rand
}
