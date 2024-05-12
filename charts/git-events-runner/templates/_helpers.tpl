{{/*
Expand the name of the chart.
*/}}
{{- define "git-events-runner.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "git-events-runner.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Create chart name and version as used by the chart label.
*/}}
{{- define "git-events-runner.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "git-events-runner.labels" -}}
helm.sh/chart: {{ include "git-events-runner.chart" . }}
{{ include "git-events-runner.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "git-events-runner.selectorLabels" -}}
app.kubernetes.io/name: {{ include "git-events-runner.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "git-events-runner.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "git-events-runner.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Create the name of the action job service account to use
*/}}
{{- define "git-events-runner.actionJobServiceAccountName" -}}
{{- if .Values.actionJobServiceAccounts.create }}
{{- default (printf "%s-action-job" (include "git-events-runner.fullname" .)) .Values.actionJobServiceAccounts.name }}
{{- else }}
{{- default "default" .Values.actionJobServiceAccounts.name }}
{{- end }}
{{- end }}

{{/*
Create the name of the ConfigMap with dynamic controller config
*/}}
{{- define "git-events-runner.configMapName" -}}
{{- printf "%s-config" (include "git-events-runner.fullname" .) }}
{{- end }}

{{/*
Create the name of the Lease with leader lock
*/}}
{{- define "git-events-runner.leaderLeaseName" -}}
{{- printf "%s-leader-lock" (include "git-events-runner.fullname" .) }}
{{- end }}
