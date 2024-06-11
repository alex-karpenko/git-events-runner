// Since `kubert` still uses outdated `kube` and `k8s_openapi` crates
// we have to depend to two different versions of those crates
// by importing old version with different names with prefix `kubert_`.
// This module is only place where it's used.
use crate::Result;
use kubert::{
    lease::{Claim, ClaimParams, Error},
    LeaseManager,
};
use kubert_k8s_openapi::{api::coordination::v1 as coordv1, apimachinery::pkg::apis::meta::v1 as metav1};
use kubert_kube::Client;
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinHandle};
use tracing::{debug, info};

/// Create lease, manager, tokio task to handle state changes
pub async fn leader_lease_handler(
    identity: &String,
    namespace: Option<String>,
    name: &String,
    duration: u64,
    grace: u64,
) -> Result<(watch::Receiver<Arc<Claim>>, JoinHandle<Result<(), Error>>)> {
    let client = Client::try_default().await?;
    let namespace = namespace.unwrap_or("default".into());

    // Create Lease
    let api = kubert_kube::Api::<coordv1::Lease>::namespaced(client.clone(), &namespace);
    let lease = api
        .create(
            &Default::default(),
            &coordv1::Lease {
                metadata: metav1::ObjectMeta {
                    name: Some(name.clone()),
                    namespace: Some(namespace.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;

    // Handle 409 error as good/expected state: lease already exists
    if let Err(err) = lease {
        if let kubert_kube::Error::Api(err) = err {
            if err.code != 409 {
                panic!("unable to create leader lock {namespace}/{name}: {err:?}");
            }
        } else {
            debug!(%namespace, lease = %name, "leader lock Lease already exists");
        }
    } else {
        info!(%namespace, lease = %name, "creating leader lock Lease");
    }

    let manager = LeaseManager::init(api, name.clone()).await?;
    let params = ClaimParams {
        lease_duration: Duration::from_secs(duration),
        renew_grace_period: Duration::from_secs(grace),
    };

    Ok(manager.spawn(&identity, params).await?)
}
