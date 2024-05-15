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
