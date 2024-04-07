use kubert::{
    lease::{Claim, ClaimParams, Error},
    LeaseManager,
};
use kubert_k8s_openapi::{
    api::coordination::v1 as coordv1, apimachinery::pkg::apis::meta::v1 as metav1,
};
use kubert_kube::Client;
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinHandle};
use tracing::{debug, info};

const DEFAULT_LEADER_LOCK_LEASE_NAME: &str = "git-events-runner-leader-lock";
const DEFAULT_LEADER_LOCK_LEASE_DERATION_SEC: u64 = 30;
const DEFAULT_LEADER_LOCK_LEASE_GRACE_SEC: u64 = 20;

pub async fn new(
    identity: &String,
    namespace: Option<String>,
) -> (watch::Receiver<Arc<Claim>>, JoinHandle<Result<(), Error>>) {
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
    let namespace = namespace.unwrap_or("default".into());
    let name: String = DEFAULT_LEADER_LOCK_LEASE_NAME.into();

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

    if let Err(err) = lease {
        if let kubert_kube::Error::Api(err) = err {
            if err.code != 409 {
                panic!("unable to create leader lock {namespace}/{name}: {err:?}");
            }
        } else {
            debug!("Leader lock Lease {namespace}/{name} already exists");
        }
    } else {
        info!("Leader lock Lease {namespace}/{name} has been created");
    }

    let manager = LeaseManager::init(api, name.clone())
        .await
        .expect("unable to create LeaseManager instance");
    let params = ClaimParams {
        lease_duration: Duration::from_secs(DEFAULT_LEADER_LOCK_LEASE_DERATION_SEC),
        renew_grace_period: Duration::from_secs(DEFAULT_LEADER_LOCK_LEASE_GRACE_SEC),
    };

    manager
        .spawn(&identity, params)
        .await
        .expect("unable to create LeaseManager")
}
