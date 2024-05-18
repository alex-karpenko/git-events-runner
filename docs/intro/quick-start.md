# Getting started

## Installation

The easiest way to install Git Events Runner operator to your Kubernetes cluster is to
use [Helm chart](https://github.com/alex-karpenko/helm-charts/tree/main/charts/git-events-runner).
Add repo:

```bash
helm repo add alex-karpenko https://alex-karpenko.github.io/helm-charts
helm repo update
```

and install Helm release with default config to `git-events-runner` namespace:

```bash
helm install git-events-runner alex-karpenko/git-events-runner \
    --namespace git-events-runner --create-namespace
```

By default, Helm creates single replica controller,
service accounts for controller and jobs in the release namespace with access to secrets in the release namespace only.
For details of configuration please refer
to [default chart config](https://github.com/alex-karpenko/helm-charts/blob/main/charts/git-events-runner/values.yaml)
and [configuration section](../guides/config.md).

## Repository

Now it's time to create our first [GitRepo](../resources/sources.md#gitrepo) resource.
Let's use this repo just for example.
Create `controller-gitrepo.yaml` file with following content:

```yaml title="controller-gitrepo.yaml"
--8<-- "docs/examples/controller-gitrepo.yaml"
```

and apply it to your cluster:

```shell
kubectl apply -f controller-gitrepo.yaml
```

That repository is public, so no authorization is needed.
Please refer to [detailed](../resources/sources.md) documentation to create more sophisticated resources with different
security and transport options.

## Action

Our example task is to upgrade...
Git Events Runner controller in our cluster.
Yes, it's possible,
just for example, but in real life don't trust public repositories with content not under your control.

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

As usual, this example is pretty straightforward,
to understand all possible ways
to customize [Action](../resources/actions.md#action) and [ClusterAction](../resources/actions.md#clusteraction) please
refer to [detailed resources'
documentation](../resources/actions.md).

## Triggers

The next step is
to create triggers which will be watching on changes in that repo and call our action to get an actual result.
Let's create two triggers:

* [ScheduleTrigger](../resources/triggers.md#scheduletrigger) which checks our repository for changes in the `main`
  branch every hour and upgrades our controller if anything were changed there.
* [WebhookTrigger](../resources/triggers.md#webhooktrigger) which does almost the same but regardless of changes in the
  repo, just on our request and use `v0.1.0` tag (first stable at the time of writing) instead of `main` branch.

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

As you can see, our WebhookTrigger refers to the Secret with webhook token, let's create example token.
You clearly understand in real life secrets should be created in secret way:

```shell
kubectl create secret generic webhook-auth --namespace git-events-runner \
    --from-literal token="some-super-secret-token"
```

To get more details about triggers configuration, please look into [triggers'
reference](../resources/triggers.md) in this guide.

## Quick result

Let's look into our cluster.
Just after creating of our first ScheduleTrigger controller's Helm release was upgraded and trigger got status with:

* last time of trigger run;
* time of the last changes in the source branch;
* commit hash on which trigger run action.

```shell
kubectl describe scheduletrigger git-events-runner-trigger -n git-events-runner
```

```yaml
Name: git-events-runner-trigger
Namespace: git-events-runner
Labels: <none>
Annotations: <none>
API Version: git-events-runner.rs/v1alpha1
Kind: ScheduleTrigger
Metadata:
  Creation Timestamp: 2024-05-18T17:19:16Z
  Finalizers:
    scheduletriggers.git-events-runner.rs
  Generation: 1
  Resource Version: 1042
  UID: 790f5bf6-b757-4b41-bdec-1968a1f03074
Spec:
  Action:
    Kind: Action
    Name: upgrade-controller
  Schedule:
    Interval: 1m
  Sources:
    Kind: GitRepo
    Names:
      git-events-runner-repo
    Watch On:
      On Change Only: true
      Reference:
        Branch: improve-docs
Status:
  Checked Sources:
    Git - Events - Runner - Repo:
      Changed: 2024-05-18T17:19:17Z
      Commit Hash: 8803515889f034ec9aa22b0234ebd950f8f1a5e8
  Last Run: 2024-05-18T17:19:17Z
  State: Idle
Events: <none>
```

Looks good, our trigger run recently on `8803515889f034ec9aa22b0234ebd950f8f1a5e8` commit.
Let's examine pods and jobs in the controllers' namespace:

```shell
kubectl get pods -n git-events-runner
```

```text
NAME                                          READY   STATUS    RESTARTS   AGE
git-events-runner-86d475584d-nfcwm            1/1     Running   0          2m36s
upgrade-controller-20240518-171917-ae-h4jf4   0/1     Error     0          109s
```

```shell
kubectl get jobss -n git-events-runner
```

```text
NAME                                    COMPLETIONS   DURATION   AGE
upgrade-controller-20240518-171917-ae   0/1           6m18s      115s
```

Something went wrong with the job, let's try to investigate deeply:

```shell
kubectl logs -n git-events-runner upgrade-controller-20240518-171917-ae-h4jf4
```

```text
Defaulted container "action-worker" out of: action-worker, action-cloner (init)
"alex-karpenko" has been added to your repositories
Hang tight while we grab the latest from your chart repositories...
...Successfully got an update from the "alex-karpenko" chart repository
Update Complete. ⎈Happy Helming!⎈
Error: UPGRADE FAILED: query: failed to query with labels: secrets is forbidden: User "system:serviceaccount:git-events-runner:git-events-runner-action-job" cannot list resource "secrets" in API group "" in the namespace "git-events-runner"
```

It looks
like Job's ServiceAccount `git-events-runner-action-job` has no enough permissions
to upgrade Helm release because there are lots of
various resources are involved: deployment, service accounts, roles, secrets,
etc. But default ServiceAccount is quite restricted and has minimal permissions just to clone source repo.

So if we need to do such a powerful job like upgrading Helm release, we have to add some power to our Action.
Right now (just for our example Action) the easiest way to empower it is to bind an additional role to the existing
service account, like below:

```yaml title="power-job-role.yaml"
--8<-- "docs/examples/power-job-role.yaml"
```

```shell
kubectl apply -f power-job-role.yaml 
```

And instead of waiting an hour for the next ScheduleTrigger run, let's trigger it via webhook.
In real life, you'll open access to webhook service via Ingress, but right now we can use `port-forward` feature
of `kubectl` to jump directly into the cluster.
Let's create port forwarding:

```shell
kubectl port-forward service/git-events-runner 8080 -n git-events-runner --address=localhost 
```

and call our WebhookTrigger in another terminal window by path like `/namespace/trigger-name/source-name`:

```shell
curl -X POST -H 'x-trigger-auth: some-super-secret-token' http://localhost:8080/git-events-runner/git-events-runner-trigger/git-events-runner-repo
```

```text
{"status":"ok","message":"job has been scheduled","task_id":"2490ea84-ce5d-4a42-8864-d33145770134"}
```

Cool! We got expected response about job scheduling, let's check the result:

```shell
kubectl get pods -n git-events-runner
```

```text
NAME                                          READY   STATUS      RESTARTS   AGE
git-events-runner-86d475584d-nfcwm            1/1     Running     0          10m
upgrade-controller-20240518-171917-ae-h4jf4   0/1     Error       0          12m
upgrade-controller-20240518-175031-yb-47r97   0/1     Completed   0          30s
```

It looks like the second job completed successfully:

```shell
kubectl logs upgrade-controller-20240518-175031-yb-47r97 -n git-events-runner
```

```text
Defaulted container "action-worker" out of: action-worker, action-cloner (init)
"alex-karpenko" has been added to your repositories
Hang tight while we grab the latest from your chart repositories...
...Successfully got an update from the "alex-karpenko" chart repository
Update Complete. ⎈Happy Helming!⎈
Release "git-events-runner" has been upgraded. Happy Helming!
NAME: git-events-runner
LAST DEPLOYED: Sat May 18 17:50:35 2024
NAMESPACE: git-events-runner
STATUS: deployed
REVISION: 2
```

Since both schedule and webhook triggers use the same service account, the next schedule trigger run also will be
completed, but it will create an actual job only in case if something changed in the source repo.
All other runs it will just watch for such changes and do nothing.

You can find all possible installation options in the detailed [installation guide](../guides/install.md).  
