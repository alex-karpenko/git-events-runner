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

For the multi-tenant clusters, you also can use the same chart, but probably
some [additional customization](#multi-namespace-installation) is needed.

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

The default name of the controllers' service account is its `{{ fullname }}` template: usually this is release name
or `fullnameOverride` value (if specified).
The default name of the jobs' service account id `{{ fullname }}-action-job`.
So if you need additional permissions for controller of jobs, you can attach it to their service accounts.

In the `actionJobServiceAccounts.namespaces` you may specify a list of namespaces where you plan to deploy custom
resources.
As a result, Helm will template and deploy service accounts, roles, etc. for action jobs to those namespaces.

In the `rbac.controller` section you can specify additional namespaces to grant controller access on
Secrets (`secretsNamespaces`) and to run Jobs (`jobsNamespaces`).

#### Controller configuration

Two additional non-standard sections to pay attention to:

* `controllerOptions`: startup controller configuration, or static config options that can't be changed without restart
  of the controller.
* `runtimeConfig`: dynamic controller config, Helm deploys it as ConfigMap in the controllers' namespace and controller
  watches on changes in this CM and reloads values in case of changes.

More details about configuration is in the [dedicated section](config.md).
Configuration provided in the default `runtimeConfig` section reflects the actual controllers' defaults and may be used
as a handy template to set yours custom values.

#### Multi namespace installation

As we mentioned before, typical installation is single-namespaced: controller with all supplementary and custom
resources are deployed to the single namespace.
But in multi-tenant clusters you may need to spread custom resources (triggers, sources, actions) to different
namespaces to reflect your responsibility model

In this case, you have to add some additional customization to the Helm release config using the following variables:

* `controllerNamespace`: namespace where controllers' resources should be deployed;
* `createNamespace`: boolean value to specify whether to create controllers' namespace.

The preferred way is to create controllers' namespace manually and specify it either in yours custom `values.yaml` or
via `--set controllerNamespace=...` Helm command line option.

Besides that, you have to add tenants namespaces to the lists for creating service accounts and RBAC resources, please
refer to the comments in the
chart [`values.yaml`](https://github.com/alex-karpenko/helm-charts/blob/main/charts/git-events-runner/values.yaml)
file, sections `actionJobServiceAccounts` and `rbac`.

## Docker images

All project Dockerfiles are self-contained and don't require any additional dependencies to build.
So you can use the content
of [dockerfiles folder](https://github.com/alex-karpenko/git-events-runner/tree/main/docker-build) to build yours
own variants of the images.

There is a tiny handy script (`local-build.sh`) to build everything locally with `local` tag, which is default in the
charts' `ci/local-default.yaml` test config.

You can find information about command line parameters in
the [configuration section](config.md#command-line-parameters).

## Build from sources

To build controller from sources, you have to have Rust toolchain installed.
Please read the [official documentation](https://www.rust-lang.org/tools/install) to get know how to prepare Rust build
environment.

Clone project repository from GitHub:

```bash
git clone https://github.com/alex-karpenko/git-events-runner.git
```

To build and run controller locally with default Kubernetes config:

```bash
# Build controller and gitrepo-cloner binaries
cargo build

# Install CRD manifests to the cluster
cargo run --bin git-events-runner -- crds | kubectl apply -f -

# Run controller locally with info loglevel
cargo run --bin git-events-runner -- run -v
```

You can specify an alternative Kubernetes config file path using `KUBECONFIG` environment variable.
