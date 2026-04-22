# Migration Guide

How to migrate DPM deployment files from the old format to the new format.

## Configuration: TOML File Replaced by Environment Variables

The old format used a `node.toml` file mounted via a ConfigMap:

```toml
[node]
listen_address = "0.0.0.0"
public_address = "<address>"
port = 9000

[canton]
admin_api_host = "<host>"
admin_api_port = 5002
ledger_api_host = "<host>"
ledger_api_port = 5001
synchronizer = "global"
network = "<network>"

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5
```

The new format uses `DECPM_*` environment variables in a Secret:

```yaml
stringData:
  DECPM_PUBLIC_ADDRESS: "<address>"
  DECPM_CANTON_ADMIN_HOST: "<host>"
  DECPM_CANTON_ADMIN_PORT: "5002"
  DECPM_CANTON_LEDGER_HOST: "<host>"
  DECPM_CANTON_LEDGER_PORT: "5001"
  DECPM_CANTON_NETWORK: "<network>"
  DECPM_CANTON_SYNCHRONIZER: "global"
  DECPM_KEYCLOAK_URL: "<keycloak-url>"
  DECPM_KEYCLOAK_REALM: "<realm>"
  DECPM_KEYCLOAK_CLIENT_ID: "<client-id>"
  DECPM_TIMEOUT_HANDSHAKE: "30"
  DECPM_TIMEOUT_MESSAGE: "120"
  DECPM_TIMEOUT_RETRY_ATTEMPTS: "3"
  DECPM_TIMEOUT_RETRY_DELAY: "5"
```

TOML field to env var mapping:

| TOML (`node.toml`) | Environment Variable |
|---------------------|---------------------|
| `node.listen_address` | `DECPM_LISTEN_ADDRESS` (defaults to `0.0.0.0`) |
| `node.public_address` | `DECPM_PUBLIC_ADDRESS` |
| `node.port` | `DECPM_NOISE_PORT` (defaults to `9000`) |
| `canton.admin_api_host` | `DECPM_CANTON_ADMIN_HOST` |
| `canton.admin_api_port` | `DECPM_CANTON_ADMIN_PORT` |
| `canton.ledger_api_host` | `DECPM_CANTON_LEDGER_HOST` |
| `canton.ledger_api_port` | `DECPM_CANTON_LEDGER_PORT` |
| `canton.synchronizer` | `DECPM_CANTON_SYNCHRONIZER` |
| `canton.network` | `DECPM_CANTON_NETWORK` |
| `timeouts.handshake_timeout_secs` | `DECPM_TIMEOUT_HANDSHAKE` |
| `timeouts.message_timeout_secs` | `DECPM_TIMEOUT_MESSAGE` |
| `timeouts.connection_retry_attempts` | `DECPM_TIMEOUT_RETRY_ATTEMPTS` |
| `timeouts.connection_retry_delay_secs` | `DECPM_TIMEOUT_RETRY_DELAY` |
| *(not in TOML)* | `DECPM_KEYCLOAK_URL` |
| *(not in TOML)* | `DECPM_KEYCLOAK_REALM` |
| *(not in TOML)* | `DECPM_KEYCLOAK_CLIENT_ID` |

The Keycloak variables are new -- they gate frontend access via Keycloak authentication.

## ConfigMap Removed

The old format required a ConfigMap to mount the TOML file and a `copy-config` initContainer to copy it into the persistent volume:

```yaml
# OLD -- remove this
initContainers:
  - name: copy-config
    image: busybox:latest
    command:
      - sh
      - -c
      - |
        mkdir -p /app/config /app/data
        cp /config-readonly/* /app/config/
volumes:
  - name: config
    configMap:
      name: dec-party-manager-1-config
```

The new format replaces it with a simpler initContainer that only ensures the data directory exists, and loads config from the Secret via `envFrom`:

```yaml
# NEW
initContainers:
  - name: init-data
    image: busybox:latest
    command: ["sh", "-c", "mkdir -p /app/data"]
    volumeMounts:
      - name: data
        mountPath: /app
containers:
  - name: dec-party-manager
    env:
      - name: RUST_LOG
        value: dec_party_manager=debug,tokio_noise=error,hyper_noise=error
    envFrom:
      - secretRef:
          name: dec-party-manager-1-secrets
```

The ConfigMap resource can be deleted entirely.

## Service: LoadBalancer to ClusterIP + Ingress

The old format exposed both HTTP and Noise ports via a LoadBalancer service:

```yaml
# OLD
spec:
  type: LoadBalancer
  ports:
    - name: http
      port: 80
      targetPort: 8080
    - name: noise
      port: 9000
      targetPort: 9000
```

The new format uses ClusterIP with a Traefik Ingress for HTTP:

```yaml
# NEW -- Service
spec:
  type: ClusterIP
  ports:
    - name: http
      port: 80
      targetPort: 8080
    - name: noise
      port: 9000
      targetPort: 9000
```

```yaml
# NEW -- Ingress
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: dec-party-manager-1-ingress
  namespace: catalyst-canton
spec:
  ingressClassName: traefik
  rules:
    - host: dec-party-manager-1.<env>.canton.example.com
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: dec-party-manager-1
                port:
                  number: 80
```

## Per-Participant Layout

The old format used a single `deployment.yaml` per environment.

The new format uses a directory per participant, each with its own `deployment.yaml` and `secrets.yaml`:

```
deployments/
  <environment>/
    participant1/
      deployment.yaml
      secrets.yaml
    participant2/
      deployment.yaml
      secrets.yaml
    participant3/
      deployment.yaml
      secrets.yaml
```

Each participant's Secret points to its own Canton node and can have its own Keycloak client. The Deployment labels distinguish coordinator (`app.kubernetes.io/component: coordinator`) from attestors (`app.kubernetes.io/component: attestor`).
