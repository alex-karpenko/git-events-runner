pub mod action;
pub mod git_repo;
pub mod trigger;

use crate::{Error, Result};
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub(crate) name: String,
}

pub(crate) fn random_string(len: usize) -> String {
    let rand: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect();
    rand
}

async fn get_secret_strings<'a>(
    client: Client,
    secret_name: &'a String,
    secret_keys: HashMap<&String, String>,
    ns: &'a str,
) -> Result<HashMap<String, String>> {
    let secret_api: Api<Secret> = Api::namespaced(client, ns);
    let secret_ref = secret_api
        .get(secret_name)
        .await
        .map_err(Error::KubeError)?;
    let mut secrets: HashMap<String, String> = HashMap::new();

    for (secret_key, expected_key) in secret_keys.into_iter() {
        let secret_data_b64 = secret_ref.clone().data.ok_or_else(|| {
            Error::SecretDecodingError(format!("no `data` part in the secret `{}`", secret_name))
        })?;
        let secret_data = secret_data_b64
            .get(secret_key)
            .ok_or_else(|| {
                Error::SecretDecodingError(format!(
                    "no `{}` key in the secret `{}`",
                    secret_key, secret_name
                ))
            })?
            .to_owned();
        let secret_data = String::from_utf8(secret_data.0).map_err(|_e| {
            Error::SecretDecodingError(format!(
                "error converting string `{}` from UTF8 in the secret `{}`",
                secret_key, secret_name
            ))
        })?;
        secrets.insert(expected_key, secret_data);
    }

    Ok(secrets)
}
