# Installation

The easiest and the most elaborated way to install Git Events Runner controller with reasonable defaults and pretty
customizable config is using Helm chart.
This way uses pre-built project Docker images and installs controller as deployment resources to the Kubernetes cluster.

At the same time, you can build and use your own Docker images with the same Helm chart or build and run controller
locally (for example, for debug or development purpose).

## Helm

To use the provided Helm chart, add Helm repository and install release as below:

```bash
# Add repo
helm repo add alex-karpenko https://alex-karpenko.github.io/helm-charts
helm repo update

# Deploy new release to the git-events-runner namespace
# with default config
helm install git-events-runner alex-karpenko/git-events-runner \
    --namespace git-events-runner --create-namespace
```

The usual way to use Git Events Runner in a single tenant cluster is to deploy it to the dedicated namespace where the
controller runs and all other supplementary resources are created.
As well as custom resources like sources, triggers and actions usually reside in the same namespace with their secrets.
Such an approach facilitates secrets management and RBAC tuning.

For the multi-tenant clusters, you also can use the same chart, but probably some additional customization is needed.

### Custom resources definitions

Provided Helm chart contains CRD manifests in the `crds/` folder.
During the first time installation, these CRDs will be deployed to the cluster, but all consecutive upgrades won't
upgrade CRDs, reason is described in
the [Helm documentation](https://helm.sh/docs/chart_best_practices/custom_resource_definitions/).

If you need to manipulate CRDs manually, pull the chart, extract it and deploy or remove CRDs as you need:

```bash
# Extarct chart into the ./git-events-runner froler  
helm pull alex-karpenko/git-events-runner --untar --untardir ./

# Deploy/upgrade all CRDs in the cluster
kubectl apply -f git-events-runner/crds/

# Remove all CRDs from the cluster
kubectl delete -f git-events-runner/crds/
```

> If you're going to uninstall controller and CRDs, delete all custom resources before uninstalling the Helm release.

### Deployment customization

As usual, Helm release can be tuned using customized `values` file or via `--set` command-line options.
Provided chart has pretty well
commented [`values.yaml`](https://github.com/alex-karpenko/helm-charts/blob/main/charts/git-events-runner/values.yaml).

We want to point out to some important parts of a release configuration.

#### Replicas

As [mentioned before](concepts.md#high-availability), Git Events Runner supports running several replicas at once, so
if you need high-availability config, you have to set `replicaCount` to something greater than 1, usually 2 is
reasonable starting choice for production environment to ensure HA and don't waste resources.
Single replica is good for development and testing.

Horizontal Pods Autoscaler (HPA) is option that available in the chart and disabled by default, configure and enable it
in the `autoscaling` section if needed.

#### RBAC and service accounts

The chart provides a way to create controllers' and jobs' service accounts (SA) as well as default set of roles,
cluster roles and necessary bindings to ensure working setup with minimal needed permissions.
The recommended way to extend permissions of the controller or action jobs is to create custom roles and bind them with
existing service accounts.

## Docker images

## Build from sources
