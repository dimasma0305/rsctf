#!/usr/bin/env bash
set -Eeuo pipefail

for command_name in kind kubectl; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "missing required command: $command_name" >&2
    exit 1
  fi
done

cluster_name="rsctf-koth-callback-${RANDOM}"
node_image='kindest/node:v1.36.1@sha256:3489c7674813ba5d8b1a9977baea8a6e553784dab7b84759d1014dbd78f7ebd5'

cleanup() {
  kind delete cluster --name "$cluster_name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

kind create cluster \
  --name "$cluster_name" \
  --image "$node_image" \
  --wait 120s

kubectl create namespace rsctf-system
kubectl create namespace rsctf-challenges

kubectl apply -f - <<'YAML'
apiVersion: v1
kind: Pod
metadata:
  name: reporter
  namespace: rsctf-system
  labels:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-network
    app.kubernetes.io/component: network
spec:
  containers:
    - name: http
      image: registry.k8s.io/e2e-test-images/agnhost:2.53@sha256:99c6b4bb4a1e1df3f0b3752168c89358794d02258ebebc26bf21c29399011a85
      args: ["netexec", "--http-port=8080"]
      ports:
        - name: http
          containerPort: 8080
---
apiVersion: v1
kind: Service
metadata:
  name: rsctf-network
  namespace: rsctf-system
spec:
  selector:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-network
    app.kubernetes.io/component: network
  ports:
    - name: http
      port: 8080
      targetPort: http
---
apiVersion: v1
kind: Pod
metadata:
  name: unrelated
  namespace: rsctf-system
  labels:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-web
    app.kubernetes.io/component: web
spec:
  containers:
    - name: http
      image: registry.k8s.io/e2e-test-images/agnhost:2.53@sha256:99c6b4bb4a1e1df3f0b3752168c89358794d02258ebebc26bf21c29399011a85
      args: ["netexec", "--http-port=8080"]
      ports:
        - name: http
          containerPort: 8080
---
apiVersion: v1
kind: Service
metadata:
  name: rsctf-web
  namespace: rsctf-system
spec:
  selector:
    app.kubernetes.io/name: rsctf
    app.kubernetes.io/instance: rsctf-web
    app.kubernetes.io/component: web
  ports:
    - name: http
      port: 8080
      targetPort: http
---
apiVersion: v1
kind: Pod
metadata:
  name: callback-client
  namespace: rsctf-challenges
  labels:
    app: rsctf-koth-callback-test
spec:
  containers:
    - name: curl
      image: curlimages/curl:8.16.0@sha256:463eaf6072688fe96ac64fa623fe73e1dbe25d8ad6c34404a669ad3ce1f104b6
      command: ["sleep", "3600"]
YAML

kubectl wait --namespace rsctf-system \
  --for=condition=Ready pod/reporter pod/unrelated --timeout=120s
kubectl wait --namespace rsctf-challenges \
  --for=condition=Ready pod/callback-client --timeout=120s

callback_url='http://rsctf-network.rsctf-system.svc:8080/echo?msg=callback-ok'
unrelated_url='http://rsctf-web.rsctf-system.svc:8080/echo?msg=unexpected'

callback_response="$(kubectl exec --namespace rsctf-challenges callback-client -- \
  curl --fail --silent --show-error --connect-timeout 2 --max-time 5 "$callback_url")"
grep -Fq 'callback-ok' <<<"$callback_response"
kubectl exec --namespace rsctf-challenges callback-client -- \
  curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
  "$unrelated_url" >/dev/null

if kubectl exec --namespace rsctf-challenges callback-client -- \
  curl --fail --silent --connect-timeout 2 --max-time 5 \
  'http://rsctf-network:8080/echo?msg=wrong-namespace' >/dev/null 2>&1; then
  echo 'bare callback Service unexpectedly resolved from the challenge namespace' >&2
  exit 1
fi

kubectl apply -f - <<'YAML'
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: managed-koth-callback
  namespace: rsctf-challenges
spec:
  podSelector:
    matchLabels:
      app: rsctf-koth-callback-test
  policyTypes:
    - Egress
  egress:
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: rsctf-system
          podSelector:
            matchLabels:
              app.kubernetes.io/name: rsctf
              app.kubernetes.io/instance: rsctf-network
              app.kubernetes.io/component: network
      ports:
        - protocol: TCP
          port: 8080
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: kube-system
          podSelector:
            matchLabels:
              k8s-app: kube-dns
      ports:
        - protocol: UDP
          port: 53
        - protocol: TCP
          port: 53
YAML

policy_enforced=0
for _ in $(seq 1 30); do
  if callback_response="$(kubectl exec --namespace rsctf-challenges callback-client -- \
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 "$callback_url")" \
    && grep -Fq 'callback-ok' <<<"$callback_response" \
    && ! kubectl exec --namespace rsctf-challenges callback-client -- \
      curl --fail --silent --connect-timeout 1 --max-time 2 \
      "$unrelated_url" >/dev/null 2>&1; then
    policy_enforced=1
    break
  fi
  sleep 1
done

if [[ "$policy_enforced" != 1 ]]; then
  echo 'NetworkPolicy did not preserve the exact callback while blocking unrelated rsctf Pods' >&2
  kubectl get pods,services,networkpolicies --all-namespaces -o wide >&2
  exit 1
fi

echo 'Kubernetes managed KotH callback DNS and egress isolation passed.'
