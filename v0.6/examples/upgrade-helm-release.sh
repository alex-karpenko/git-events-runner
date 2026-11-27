#!/bin/bash

helm repo add alex-karpenko https://alex-karpenko.github.io/helm-charts
helm repo update

helm upgrade git-events-runner alex-karpenko/git-events-runner --namespace git-events-runner
