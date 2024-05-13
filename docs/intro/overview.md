# What is Git Events Runner

As mentioned [above](../index.md#briefly) this is custom Kubernetes operator which serves to:

* Watch on some Git repositories and run `Jobs` if something was changed in the watched branch/tag.
* Run the same `Jobs` when webhook has been received.
* Do the same just periodically even if nothing was changed.

## How it works

- Central resources of the `Git Events Runner` are [**triggers**](../resources/triggers.md), there are two - [`ScheduleTrigger`](../resources/triggers.md#scheduletrigger) and [`WebhookTrigger`](../resources/triggers.md#webhooktrigger).
- Obviously triggers interact with some [**sources**](../resources/sources.md) to watch on changes, there are two kinds of sources: [`GitRepo`](../resources/sources.md#gitrepo) and [`ClusterGitRepo`](../resources/sources.md#clustergitrepo).
- Finally, if some trigger fires (something changes in the branch or just time arrived or webhook received) it pushes third type of resource - [**actions**](../resources/actions.md) - to perform its dedication and run the `Job` (yes, it's just a classic [Kubernetes Job](https://kubernetes.io/docs/concepts/workloads/controllers/job/)) which is described by [`Action`](../resources/actions.md#action) and [`ClusterAction`](../resources/actions.md#clusteraction) resources.

Detailed explanations of all custom resources with examples can be found in the [**Guides**](../guides/concepts.md) section.

### Typical flow

Imagine we need to run script `deploy.sh` from the root of Git repo `https://git.example.com/the-best-team/cool-project` when anything was changed in the `main` branch (we have fine-grained branch protection and no one can merge to the `main` without approvement). That script, for example, upgrades Helm release in our K8s cluster in the namespace of the trigger:

```bash
#!/bin/bash

# 1st argument is path to the folder with Helm chart

helm upgrade ${TRIGGER_SOURCE_NAME} ${1} -f configs/values.yaml --install --wait && \
    (echo "Has been successfully deployed"; exit 0) || \
    (echo "Something went wrong"; exit 1)
```
and we need to check for branch changes `every 5 minutes`.

To implement this requirements (except putting script to the repo) we create three simple resources:

* GitRepo:
```yaml
apiVersion: git-events-runner.rs/v1alpha1
kind: GitRepo
metadata:
  name: cool-project-repo
  namespace: default
spec:
  repoUri: https://git.example.com/the-best-team/cool-project.git
  authConfig:
    type: basic
    secretRef:
      name: cool-project-creds
```
* ScheduleTrigger:
```yaml
apiVersion: git-events-runner.rs/v1alpha1
kind: ScheduleTrigger
metadata:
  name: cool-trigger
  namespace: default
spec:
  sources:
    kind: GitRepo
    names:
      - cool-project-repo
    watchOn:
      onChangeOnly: true
      reference:
        branch: main
  schedule:
    interval: 5m
  action:
    kind: Action
    name: run-deploy-sh
```
* Action:
```yaml
apiVersion: git-events-runner.rs/v1alpha1
kind: Action
metadata:
  name: run-deploy-sh
  namespace: default
spec:
  actionJob:
    args:
      - ./deploy.sh
      - ./chart/
    serviceAccount: git-events-runner-jobs
```

Using these resources `Git Events Runner` does following for each repo mentioned in the list of sources of the trigger:

1. Clone specified branch (main is here) of the repo using URI and credentials (if needed) defined in GitRepo resource.
2. Compare the latest commit hash with previously stored value (from previous run, if it was).
3. If hash differs it uses Action resource to create Job with two containers:
      * init (cloner) container runs special image which clones repo to volume shared with second (worker) container.
      * worker container set current directory to folder with cloned repo and "runs" worker image with arguments specified in Action resource.
4. Stores last commit hash for the next run.
5. Repeats the same for each source in the trigger.
6. Sleep for specified interval and repeats everything.

> Notes:
>
> - `GitRepo` and `Action` instances can be used by several different triggers, as well as `ScheduleTrigger` can refer to several sources of some kind.
> - If source refers to some secret(s) with credentials (if needed) that secret(s) should be created separately.

One more chance to run the same Job is to define WebhookTrigger like this one:

```yaml
apiVersion: git-events-runner.rs/v1alpha1
kind: WebhookTrigger
metadata:
  name: coll-trigger
  namespace: default
spec:
  sources:
    kind: GitRepo
    names:
      - cool-project-repo # the same as before
    watchOn:
      onChangeOnly: false # pay attention to this
      reference:
        branch: main
  action:
    kind: Action
    name: run-deploy-sh # the same action
  webhook:
    multiSource: true
    authConfig:
      secretRef:
        name: webhook-auth
      key: token
```

Notice that we:

* Reused existing GitRepo and Action for another trigger, even of other type.
* Set `onChangeOnly` to `false` to force running Action every time trigger fires (webhook request received) even if there were no changes in the main repo branch.

### Namespaced vs cluster resources

Both types of triggers are namespaced resources. However Acton and GitRepo have their cluster-level counterparts. This means:

* Triggers can refer any kind of actions or sources: namespaced or cluster-wide.
* WebhookTrigger can use Secrets in its own namespace only.
* GitRepo can use Secrets in its own namespace only, but ClusterGitRepo can refer secrets in other namespaces but with respect to controller's permissions.
* Both Action and ClusterAction can create Jobs in the trigger's namespace only.

Cluster-wide sources and actions are useful:

* in multi-tenant clusters;
* to share restricted configs between tenants/namespaces;
* just to avoid repetitions of configurations.

## Security aspects

TBW

## Differences from GitOps

TBW
