{{/* Expand the chart name. */}}
{{- define "rsctf.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Create a stable, DNS-safe resource prefix. */}}
{{- define "rsctf.fullname" -}}
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

{{- define "rsctf.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "rsctf.labels" -}}
helm.sh/chart: {{ include "rsctf.chart" . }}
{{ include "rsctf.selectorLabels" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "rsctf.selectorLabels" -}}
app.kubernetes.io/name: {{ include "rsctf.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "rsctf.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "rsctf.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "rsctf.secretName" -}}
{{- default (include "rsctf.fullname" .) .Values.existingSecret.name }}
{{- end }}

{{- define "rsctf.storageSecretName" -}}
{{- default (printf "%s-storage" (include "rsctf.fullname" .)) .Values.storage.s3.existingSecret.name }}
{{- end }}

{{- define "rsctf.postgresqlName" -}}
{{- printf "%s-postgresql" (include "rsctf.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "rsctf.redisName" -}}
{{- printf "%s-redis" (include "rsctf.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "rsctf.challengeNamespace" -}}
{{- default (printf "%s-challenges" (include "rsctf.fullname" .)) .Values.kubernetes.challengeNamespace | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Local runtime used for A&D/KotH and host-side maintenance operations. */}}
{{- define "rsctf.localContainerBackend" -}}
{{- if eq .Values.containerBackend "worker" -}}
{{- .Values.workerBackend.localBackend -}}
{{- else -}}
{{- .Values.containerBackend -}}
{{- end -}}
{{- end }}

{{/* Fail early with messages that explain exactly what the operator must set. */}}
{{- define "rsctf.validateValues" -}}
{{- $backend := .Values.containerBackend -}}
{{- $localBackend := include "rsctf.localContainerBackend" . | trim -}}
{{- $role := .Values.runtimeRole -}}
{{- $splitRole := and (ne $role "all") (ne $role "migrate") -}}
{{- if not (has $role (list "all" "web" "control" "engine" "network" "migrate")) -}}
{{- fail "runtimeRole must be one of: all, web, control, engine, network, migrate" -}}
{{- end -}}
{{- $checkerUidEnd := add .Values.config.checkerUidBase .Values.config.checkerProcessBudget -1 -}}
{{- if gt (int $checkerUidEnd) 65534 -}}
{{- fail "config.checkerUidBase + config.checkerProcessBudget - 1 must be at most 65534" -}}
{{- end -}}
{{- if and (has $role (list "all" "control" "network" "migrate")) (ne (int .Values.replicaCount) 1) -}}
{{- fail (printf "runtimeRole=%s requires replicaCount=1" $role) -}}
{{- end -}}
{{- if and (has $role (list "all" "control" "network")) (ne .Values.strategy.type "Recreate") -}}
{{- fail (printf "runtimeRole=%s requires strategy.type=Recreate because the singleton network lease is fail-fast and has no rolling standby" $role) -}}
{{- end -}}
{{- $imageTag := default .Chart.AppVersion .Values.image.tag -}}
{{- if and (ne $role "all") (eq (lower $imageTag) "latest") -}}
{{- fail (printf "runtimeRole=%s cannot use image.tag=latest; pin migration and every role release to the same reviewed version" $role) -}}
{{- end -}}
{{- if $splitRole -}}
  {{- if empty .Values.existingSecret.name -}}
    {{- fail (printf "runtimeRole=%s requires existingSecret.name with the shared external PostgreSQL, Redis, and JWT configuration" $role) -}}
  {{- end -}}
  {{- if .Values.postgresql.enabled -}}
    {{- fail (printf "runtimeRole=%s requires postgresql.enabled=false; split releases must use one externally managed PostgreSQL database" $role) -}}
  {{- end -}}
  {{- if .Values.redis.enabled -}}
    {{- fail (printf "runtimeRole=%s requires redis.enabled=false; split releases must use one externally managed Redis service through existingSecret" $role) -}}
  {{- end -}}
  {{- if eq $localBackend "kubernetes" -}}
    {{- if empty .Values.kubernetes.challengeNamespace -}}
      {{- fail (printf "runtimeRole=%s requires an explicit shared kubernetes.challengeNamespace" $role) -}}
    {{- end -}}
    {{- if .Values.kubernetes.createChallengeNamespace -}}
      {{- fail (printf "runtimeRole=%s requires kubernetes.createChallengeNamespace=false; pre-create the shared namespace outside every role release" $role) -}}
    {{- end -}}
  {{- end -}}
  {{- if or (not .Values.persistence.enabled) (empty .Values.persistence.existingClaim) -}}
    {{- fail (printf "runtimeRole=%s requires persistence.enabled=true and an explicit shared persistence.existingClaim; S3 covers blobs but not repository, checker, capture, and snapshot paths" $role) -}}
  {{- end -}}
  {{- if not (has "ReadWriteMany" .Values.persistence.accessModes) -}}
    {{- fail (printf "runtimeRole=%s requires ReadWriteMany in persistence.accessModes to declare that persistence.existingClaim is shared across replicas" $role) -}}
  {{- end -}}
{{- end -}}
{{- if and .Values.ingress.enabled (has $role (list "control" "engine" "network" "migrate")) -}}
{{- fail (printf "runtimeRole=%s cannot expose the generic Ingress; expose it from the web release and configure ingress.statefulRoutes.serviceName for the control/network Service" $role) -}}
{{- end -}}
{{- if and .Values.ingress.enabled (eq $role "web") (not .Values.ingress.statefulRoutes.enabled) -}}
{{- fail "runtimeRole=web with ingress.enabled=true requires ingress.statefulRoutes.enabled=true so stateful connections cannot land on the web pool" -}}
{{- end -}}
{{- if and .Values.ingress.enabled (eq $role "web") (empty .Values.ingress.statefulRoutes.serviceName) -}}
{{- fail "runtimeRole=web with ingress.enabled=true requires ingress.statefulRoutes.serviceName to name the control or network Service" -}}
{{- end -}}
{{- if and .Values.ingress.statefulRoutes.enabled (ne $role "web") -}}
{{- fail "ingress.statefulRoutes is valid only for runtimeRole=web" -}}
{{- end -}}
{{- if .Values.workerPlane.enabled -}}
  {{- if ne $backend "worker" -}}
    {{- fail "workerPlane.enabled=true requires containerBackend=worker" -}}
  {{- end -}}
  {{- if not (has $role (list "all" "control" "network")) -}}
    {{- fail (printf "workerPlane.enabled=true requires the singleton all, control, or network role; runtimeRole=%s cannot own worker sessions" $role) -}}
  {{- end -}}
  {{- $_ := required "workerPlane.publicEndpoint is required when the worker plane is enabled (for example workers.ctf.example:9443)" .Values.workerPlane.publicEndpoint -}}
  {{- $_ := required "workerPlane.serverName is required when the worker plane is enabled and must match the server certificate SAN" .Values.workerPlane.serverName -}}
  {{- $_ := required "workerPlane.existingSecret.name is required when the worker plane is enabled" .Values.workerPlane.existingSecret.name -}}
  {{- $_ := required "workerPlane.existingSecret.caCertKey is required when the worker plane is enabled" .Values.workerPlane.existingSecret.caCertKey -}}
  {{- $_ := required "workerPlane.existingSecret.caKeyKey is required when the worker plane is enabled" .Values.workerPlane.existingSecret.caKeyKey -}}
  {{- $_ := required "workerPlane.existingSecret.serverCertKey is required when the worker plane is enabled" .Values.workerPlane.existingSecret.serverCertKey -}}
  {{- $_ := required "workerPlane.existingSecret.serverKeyKey is required when the worker plane is enabled" .Values.workerPlane.existingSecret.serverKeyKey -}}
{{- end -}}
{{- if and (eq $backend "worker") (has $role (list "all" "control" "network")) (not .Values.workerPlane.enabled) -}}
{{- fail (printf "runtimeRole=%s with containerBackend=worker must enable workerPlane on this singleton network owner" $role) -}}
{{- end -}}
{{- if and (eq $role "migrate") (empty .Values.existingSecret.name) -}}
{{- fail "runtimeRole=migrate requires existingSecret.name because the pre-install migration hook runs before chart-managed Secrets exist" -}}
{{- end -}}
{{- if and (eq $role "migrate") .Values.postgresql.enabled -}}
{{- fail "runtimeRole=migrate requires postgresql.enabled=false and an existing Secret pointing at the shared database" -}}
{{- end -}}
{{- if and (eq $role "migrate") .Values.redis.enabled -}}
{{- fail "runtimeRole=migrate requires redis.enabled=false; migrations use only the shared PostgreSQL database" -}}
{{- end -}}
{{- if not (has $backend (list "none" "kubernetes" "docker" "worker")) -}}
{{- fail "containerBackend must be one of: none, kubernetes, docker, worker" -}}
{{- end -}}
{{- if not (has .Values.workerBackend.localBackend (list "none" "docker" "kubernetes")) -}}
{{- fail "workerBackend.localBackend must be one of: none, docker, kubernetes" -}}
{{- end -}}
{{- if and (ne $backend "worker") (ne .Values.workerBackend.localBackend "none") -}}
{{- fail "workerBackend.localBackend is valid only when containerBackend=worker; otherwise set it to none" -}}
{{- end -}}
{{- if and (eq $backend "worker") (ne $role "all") (ne $localBackend "none") -}}
{{- fail "hybrid worker local backends currently require runtimeRole=all; split roles do not yet delegate local lifecycle requests from web to the singleton runtime owner" -}}
{{- end -}}
{{- $scanConnections := mul 4 .Values.config.repoScanConcurrency -}}
{{- $minimumConnections := add $scanConnections (mul 2 .Values.config.provisioningConcurrency) 1 -}}
{{- if eq $role "migrate" -}}
  {{- $minimumConnections = 2 -}}
{{- else if and .Values.vpn.enabled (has $role (list "all" "control" "network")) -}}
  {{- $minimumConnections = add $scanConnections (mul 2 .Values.config.provisioningConcurrency) 6 -}}
{{- else if has $role (list "all" "control" "network") -}}
  {{- $minimumConnections = add $scanConnections (mul 2 .Values.config.provisioningConcurrency) 3 -}}
{{- end -}}
{{- if and (ne $role "migrate") (has $role (list "all" "web")) -}}
  {{- $minimumConnections = add $minimumConnections 8 -}}
{{- end -}}
{{- if lt (int .Values.config.dbMaxConnections) (int $minimumConnections) -}}
{{- fail (printf "runtimeRole=%s with vpn.enabled=%v, config.repoScanConcurrency=%v, and config.provisioningConcurrency=%v requires config.dbMaxConnections >= %v" $role .Values.vpn.enabled .Values.config.repoScanConcurrency .Values.config.provisioningConcurrency $minimumConnections) -}}
{{- end -}}

{{- if not (has .Values.storage.backend (list "local" "s3")) -}}
{{- fail "storage.backend must be local or s3" -}}
{{- end -}}
{{- if eq .Values.storage.backend "s3" -}}
  {{- $_ := required "storage.s3.bucket is required when storage.backend=s3" .Values.storage.s3.bucket -}}
  {{- $_ := required "storage.s3.existingSecret.accessKeyKey is required when storage.backend=s3" .Values.storage.s3.existingSecret.accessKeyKey -}}
  {{- $_ := required "storage.s3.existingSecret.secretKeyKey is required when storage.backend=s3" .Values.storage.s3.existingSecret.secretKeyKey -}}
  {{- if empty .Values.storage.s3.existingSecret.name -}}
    {{- $_ := required "storage.s3.accessKey is required when no S3 existing Secret is selected" .Values.storage.s3.accessKey -}}
    {{- $_ := required "storage.s3.secretKey is required when no S3 existing Secret is selected" .Values.storage.s3.secretKey -}}
  {{- end -}}
{{- end -}}
{{- $_ := required "existingSecret.databaseUrlKey is required" .Values.existingSecret.databaseUrlKey -}}
{{- $_ := required "existingSecret.jwtSecretKey is required" .Values.existingSecret.jwtSecretKey -}}
{{- $_ := required "existingSecret.bootstrapTokenKey is required" .Values.existingSecret.bootstrapTokenKey -}}
{{- if .Values.postgresql.enabled -}}
  {{- $_ := required "existingSecret.postgresqlPasswordKey is required for bundled PostgreSQL" .Values.existingSecret.postgresqlPasswordKey -}}
{{- end -}}
{{- if .Values.redis.enabled -}}
  {{- $_ := required "existingSecret.redisPasswordKey is required for bundled Redis" .Values.existingSecret.redisPasswordKey -}}
  {{- $_ := required "existingSecret.redisUrlKey is required for bundled Redis" .Values.existingSecret.redisUrlKey -}}
{{- end -}}

{{- if empty .Values.existingSecret.name -}}
  {{- $jwt := required "secrets.jwtSecret is required when existingSecret.name is empty (use: openssl rand -hex 32)" .Values.secrets.jwtSecret -}}
  {{- if lt (len $jwt) 32 -}}
    {{- fail "secrets.jwtSecret must contain at least 32 characters of random data" -}}
  {{- end -}}
  {{- if has $jwt (list "insecure-dev-secret-change-me" "change-me-in-production") -}}
    {{- fail "secrets.jwtSecret uses a known insecure value; generate a unique secret" -}}
  {{- end -}}
  {{- if and (not (empty .Values.secrets.bootstrapToken)) (lt (len .Values.secrets.bootstrapToken) 32) -}}
    {{- fail "secrets.bootstrapToken must contain at least 32 characters when explicitly set" -}}
  {{- end -}}
  {{- if and (not .Values.postgresql.enabled) (empty .Values.database.url) -}}
    {{- fail "database.url is required when postgresql.enabled=false and existingSecret.name is empty" -}}
  {{- end -}}
{{- end -}}

{{- if and (eq $localBackend "docker") (not .Values.docker.socket.enabled) -}}
{{- fail "the effective local Docker backend requires docker.socket.enabled=true; mounting the Docker socket is root-equivalent and intended only for advanced deployments" -}}
{{- end -}}
{{- if and (eq $localBackend "kubernetes") .Values.trafficCapture.enabled -}}
{{- fail "the effective local Kubernetes backend does not support live packet capture; set trafficCapture.enabled=false" -}}
{{- end -}}
{{- if and (eq $backend "worker") (eq $localBackend "none") .Values.trafficCapture.enabled -}}
{{- fail "a pure remote worker backend does not support control-host packet capture; set trafficCapture.enabled=false or use workerBackend.localBackend=docker with runtimeRole=all" -}}
{{- end -}}
{{- if and (eq $localBackend "kubernetes") (ne $role "migrate") -}}
  {{- $_ := required "kubernetes.adServiceCidr is required on every Kubernetes runtime role, even without VPN; set it to the cluster Service CIDR used by provisioning and checker isolation" .Values.kubernetes.adServiceCidr -}}
{{- end -}}

{{- if and (eq $localBackend "kubernetes") (eq (include "rsctf.challengeNamespace" .) .Release.Namespace) -}}
{{- fail "kubernetes.challengeNamespace must differ from the Helm release namespace so challenge workloads stay isolated" -}}
{{- end -}}

{{- if and (eq $backend "worker") .Values.vpn.enabled (ne $role "all") -}}
{{- fail "hybrid worker VPN is currently supported only with runtimeRole=all; split lifecycle and web policy coordination are not implemented and must fail closed" -}}
{{- end -}}
{{- if .Values.vpn.enabled -}}
  {{- if not (has $localBackend (list "docker" "kubernetes")) -}}
    {{- fail "vpn.enabled=true requires a local Docker or Kubernetes backend; set containerBackend directly or configure workerBackend.localBackend for a hybrid worker deployment" -}}
  {{- end -}}
  {{- $_ := required "vpn.serverEndpoint is required when VPN is enabled (for example vpn.ctf.example:51820)" .Values.vpn.serverEndpoint -}}
  {{- $_ := required "vpn.clientCidr is required when VPN is enabled" .Values.vpn.clientCidr -}}
  {{- $_ := required "vpn.servicesCidr is required when VPN is enabled" .Values.vpn.servicesCidr -}}
  {{- $_ := required "vpn.devicePath is required when VPN is enabled" .Values.vpn.devicePath -}}
  {{- if eq $localBackend "kubernetes" -}}
    {{- if not .Values.kubernetes.isolatedPodNetns -}}
      {{- fail "kubernetes.isolatedPodNetns must be true for Kubernetes A&D over VPN" -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- end }}
