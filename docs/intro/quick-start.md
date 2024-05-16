# Getting started

## Installation

The easiest way to install Git Events Runner operator to your Kubernetes cluster is to use [Helm chart](https://github.com/alex-karpenko/helm-charts/tree/main/charts/git-events-runner). Add repo:


```bash
helm repo add alex-karpenko https://alex-karpenko.github.io/helm-charts
helm repo update
```

and install Helm release with default config to `git-events-runner` namespace:

```bash
helm install git-events-runner alex-karpenko/git-events-runner \
    --namespace git-events-runner --create-namespace
```

By default Helm creates single replica controller, service accounts for controller and jobs in the release namespace with access to secrets in the release namespace only. For details of configuration please refer to [default chart config](https://github.com/alex-karpenko/helm-charts/blob/main/charts/git-events-runner/values.yaml) and [configuration section](../guides/config.md).

## Repository

Now it's time to create our first [GitRepo](../resources/sources.md#gitrepo) resource. Lets use this repo just for example. Create `controller-gitrepo.yaml` file with following content:

```yaml title="controller-gitrepo.yaml"
--8<-- "docs/examples/controller-gitrepo.yaml"
```

and apply it to your cluster:

```shell
kubectl apply -f controller-gitrepo.yaml
```

That repository is public, so no authorization is needed. Please refer to [detailed](../resources/sources.md) documentation to create more sophisticated resources with different security and transport options.

## Action

Our example task is to upgrade... Git Events Runner controller in our cluster. Yes, it's possible, just for example, but in real life don't trust public repositories with content not under you control.

For this purpose there is example script `docs/examples/upgrade-helm-release.sh`:

```shell title="upgrade-helm-release.sh"
--8<-- "docs/examples/upgrade-helm-release.sh"
```

So our [Action](../resources/actions.md#action) resource defines way to run that script:

```yaml title="upgrade-controller-action.yaml"
--8<-- "docs/examples/upgrade-controller-action.yaml"
```

Push action resource to the cluster:

```shell
kubectl apply -f upgrade-controller-action.yaml
```

As usual this example is pretty simple, to understand all possible ways to customize [Action](../resources/actions.md#action) and [ClusterAction](../resources/actions.md#clusteraction) please refer to [detailed resources documentation](../resources/actions.md).

## Triggers

Next step is to create triggers which will be watching on changes in that repo and call our action to get actual result. Lets create two triggers:

* [ScheduleTrigger](../resources/triggers.md#scheduletrigger) which checks our repository for changes in the `main` branch every hour and, obviously, upgrade our controller if anything were changed there.
* [WebhookTrigger](../resources/triggers.md#webhooktrigger) which does almost the same but regardless of changes in the repo, just on our request and use `v0.1.0` tag (first stable at the time of writing) instead of `main` branch.

Minimal versions of our example triggers look like these:

```yaml title="schedule-trigger.yaml"
--8<-- "docs/examples/schedule-trigger.yaml"
```

```yaml title="webhook-trigger.yaml"
--8<-- "docs/examples/webhook-trigger.yaml"
```

Apply to the cluster:

```shell
kubectl apply -f schedule-trigger.yaml
kubectl apply -f webhook-trigger.yaml
```

As you can see our WebhookTrigger refers to the Secret with webhook token,lets create example token. You clearly understand that in real life secrets should be created in secret way:

```shell
kubectl create secret generic webhook-auth --namespace git-events-runner \
    --from-literal token="some-super-secret-token"
```

To get more details about triggers configuration please look into [triggers reference](../resources/triggers.md) in this guide.

## Quick result

Lets look into our cluster. Just after creating of our first ScheduleTrigger controller's Helm release was upgraded and trigger got status with:

* last time of trigger run;
* time of the last changes in the source branch;
* commit hash on which trigger run action.

```shell
kubectl describe scheduletrigger git-events-runner-trigger -n git-events-runner

```
