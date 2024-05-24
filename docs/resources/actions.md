# Actions

Describes actions what should be run as reaction to fired trigger.

There are only two differences between these resources:

* ClusterAction is a cluster wide resource, but Action is namespaced one.
* `sourceOverride` section of the Action can refer to any kind of sources whereas ClusterAction can use cluster scoped
  sources only (ClusterGitRepo).

Actual defaults that will be used instead of omitted fields depend on the values from dynamic controllers' config, so
defaults mentioned below are actually defaults of the controller but may be overridden in controllers' configuration.

Below are examples of each resource kind with detailed comments. You see that minimal action resource may look like:

```yaml
apiVersion: git-events-runner.rs/v1alpha1
kind: Action
metadata:
  name: some-action
  namespace: default
spec:
  actionJob:
```

and in this case, all defaults will be applied: default images, entrypoints, arguments, etc.

## Action

```yaml title="Action"
--8<-- "docs/examples/action.yaml"
```

## ClusterAction

```yaml title="ClusterAction"
--8<-- "docs/examples/clusteraction.yaml"
```
