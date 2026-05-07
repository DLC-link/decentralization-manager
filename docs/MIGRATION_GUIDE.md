# DPM Migration Guide

How to move an existing single-participant DPM (Dec Party Manager) deployment to the current release.

The format has changed substantially: configuration moved from a mounted TOML file to environment variables, the Service type changed from `LoadBalancer` to `ClusterIP` behind an Ingress, the ConfigMap is gone, and the admin UI is now gated by Keycloak. Rather than patching the old deployment in place, **the cleanest path is a fresh deployment**: tear the old deployment down, apply the new manifests, and re-establish your peer list with the other participants once everyone has redeployed. Because the new deployment generates a fresh Noise keypair on first start (and every other participant going through the same migration will too), there's no value in carrying the old peer list across — every public key in it is about to change. Treat this as a coordinated rebuild, not an in-place upgrade.

This guide assumes:
- One participant per cluster (the most common external setup).
- A working Kubernetes cluster with a Traefik (or other) Ingress controller.
- Access to a Keycloak realm you control (or that an operator has set up for you).
- You already have a Canton participant node running and reachable from the cluster.

## Migration at a glance

1. **Tear down** the old Deployment, ConfigMap, Secret, and LoadBalancer Service.
2. **Pull the latest image tag** and prepare new manifests (Secret, Deployment + PVC, Service, Ingress).
3. **Apply the new manifests** and let DPM start clean.
4. **Re-share public keys** with the other participants and add them as peers in the new UI.
5. **Re-enter party credentials** (Keycloak per-party client info) through the new UI.

Total downtime is typically a few minutes per participant. The longer step is the cross-team coordination needed to re-share Noise public keys after everyone redeploys.

## 1 — Tear down the old deployment

Delete the old resources cleanly. From a workstation with `kubectl` access:

```bash
kubectl -n <your-namespace> delete deployment <your-old-deployment>
kubectl -n <your-namespace> delete service <your-old-service>
kubectl -n <your-namespace> delete configmap <your-old-configmap>
kubectl -n <your-namespace> delete secret <your-old-secret>
kubectl -n <your-namespace> delete pvc <your-old-pvc>
```

The PVC contains the SQLite database and the Noise keypair; wiping it is what forces a clean start. Your peers will see your new public key once the new deployment is up — that is expected, and they will be doing the same. If you would rather not regenerate the keypair (for example, if you are migrating ahead of your peers and want to stay reachable on your existing key), see the note at the end about keeping the existing PVC.

## 2 — Pull the latest image

The new DPM is published as a public container image:

```
public.ecr.aws/dlc-link/canton-decparty-manager:<tag>
```

Use the latest tagged release (for example `0.0.30`). Pin the version explicitly — do not use `latest`. If your cluster cannot pull from Public ECR directly, mirror the image into your own registry first.

To check the version of a running pod:

```bash
kubectl -n <your-namespace> get deploy dec-party-manager -o jsonpath='{.spec.template.spec.containers[0].image}'
```

Bumping the tag in the Deployment manifest and re-applying is how you upgrade DPM going forward — no schema migrations or config rewrites are required between minor releases.

## 3 — Apply the new manifests

Below is a complete single-participant manifest set. Replace every `<...>` placeholder with values for your environment, save as a file, and apply with `kubectl apply -f <file>.yaml`.

### 4a. Namespace (skip if it already exists)

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: <your-namespace>
```

### 4b. Secret (configuration)

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: dec-party-manager-secrets
  namespace: <your-namespace>
type: Opaque
stringData:
  # Public address that peers use to reach this node over the Noise transport.
  # Must be reachable from the public internet on the Noise port (9000 by default).
  DECPM_PUBLIC_ADDRESS: "<your-public-host>"

  # Canton participant node connection (Admin + Ledger gRPC APIs).
  DECPM_CANTON_ADMIN_HOST: "<canton-admin-host>"
  DECPM_CANTON_ADMIN_PORT: "5002"
  DECPM_CANTON_LEDGER_HOST: "<canton-ledger-host>"
  DECPM_CANTON_LEDGER_PORT: "5001"
  DECPM_CANTON_NETWORK: "mainnet"          # mainnet | testnet | devnet
  DECPM_CANTON_SYNCHRONIZER: "global"

  # Keycloak (gates the admin UI).
  DECPM_KEYCLOAK_URL: "https://<your-keycloak-host>"
  DECPM_KEYCLOAK_REALM: "<your-realm>"
  DECPM_KEYCLOAK_CLIENT_ID: "<frontend-client-id>"

  # Optional: require a specific Keycloak role on every authenticated caller
  # before they can hit privileged endpoints (PUT /party-config, /kick, etc.).
  # If unset, every authenticated user is treated as admin — fine for a
  # single-operator deployment, dangerous for shared environments.
  # DECPM_ADMIN_ROLE: "dpm-admin"

  # Optional: encryption key for secrets stored in the SQLite database
  # (Keycloak client secrets per party). 32-byte random hex. If unset,
  # secrets are stored in plaintext in the DB.
  # DECPM_DB_ENCRYPTION_KEY: "<64-hex-chars>"

  # Optional: tighten CORS to a specific origin if the UI is served from a
  # different host than the API (reverse-proxy / dev server). Defaults to
  # same-origin only, which is correct for the Ingress setup below.
  # DECPM_ALLOWED_ORIGIN: "https://<ui-host>"

  # Noise transport timeouts (seconds). Defaults are usually fine.
  DECPM_TIMEOUT_HANDSHAKE: "30"
  DECPM_TIMEOUT_MESSAGE: "120"
  DECPM_TIMEOUT_RETRY_ATTEMPTS: "3"
  DECPM_TIMEOUT_RETRY_DELAY: "5"
```

### 4c. Deployment + PersistentVolumeClaim

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: dec-party-manager-data
  namespace: <your-namespace>
spec:
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 1Gi
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: dec-party-manager
  namespace: <your-namespace>
  labels:
    app.kubernetes.io/name: dec-party-manager
spec:
  replicas: 1
  selector:
    matchLabels:
      app.kubernetes.io/name: dec-party-manager
  template:
    metadata:
      labels:
        app.kubernetes.io/name: dec-party-manager
    spec:
      initContainers:
        - name: init-data
          image: busybox:latest
          command: ["sh", "-c", "mkdir -p /app/data"]
          volumeMounts:
            - name: data
              mountPath: /app
      containers:
        - name: dec-party-manager
          image: public.ecr.aws/dlc-link/canton-decparty-manager:<tag>
          imagePullPolicy: Always
          command:
            - dec-party-manager
            - -d
            - /app
            - serve
            - --host
            - 0.0.0.0
            - --port
            - "8080"
          ports:
            - name: http
              containerPort: 8080
            - name: noise
              containerPort: 9000
          volumeMounts:
            - name: data
              mountPath: /app
          resources:
            requests: { memory: "128Mi", cpu: "100m" }
            limits:   { memory: "512Mi", cpu: "500m" }
          env:
            - name: RUST_LOG
              value: dec_party_manager=info,tokio_noise=error,hyper_noise=error
          envFrom:
            - secretRef:
                name: dec-party-manager-secrets
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: dec-party-manager-data
```

### 4d. Service

```yaml
apiVersion: v1
kind: Service
metadata:
  name: dec-party-manager
  namespace: <your-namespace>
spec:
  type: ClusterIP
  ports:
    - name: http
      port: 80
      targetPort: 8080
    - name: noise
      port: 9000
      targetPort: 9000
  selector:
    app.kubernetes.io/name: dec-party-manager
```

The Noise port (9000) must also be reachable from the public internet for peers to connect. Depending on your cluster, you may need a separate `LoadBalancer`-type Service, a `NodePort`, or another mechanism (MetalLB, native cloud load balancer, etc.) for the `noise` port specifically. The HTTP UI does not need to be exposed publicly — it is reached through the Ingress (next).

### 4e. Ingress (Traefik example)

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: dec-party-manager
  namespace: <your-namespace>
spec:
  ingressClassName: traefik
  rules:
    - host: <your-ui-host>
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: dec-party-manager
                port:
                  number: 80
```

Adapt the `ingressClassName` and TLS configuration to your cluster's Ingress controller (nginx, Contour, etc.). Make sure `<your-ui-host>` resolves to the cluster and is registered as a valid redirect URI on your Keycloak client.

### Apply

```bash
kubectl apply -f <each-of-the-above>.yaml
kubectl -n <your-namespace> rollout status deploy/dec-party-manager
```

Once the pod is `Running`, hit `https://<your-ui-host>` in a browser. Keycloak should challenge for login.

## 4 — Re-establish peers

The new DPM has generated a fresh Noise keypair on first start. None of the other participants know it yet, and you do not yet know any of theirs (assuming they are also redeploying). The peer list has to be rebuilt from scratch by exchanging public keys out-of-band.

For each participant in your network:

1. Find your own public key + public address. In the UI, open the "Network Config" panel — your local node's public key is shown there. (Same value is reachable via `GET /keys/status`.) Note your `DECPM_PUBLIC_ADDRESS` and Noise port (default `9000`).
2. Share that information with each peer through whatever out-of-band channel you use (chat, email, ticket). They need: your `participant_id`, a friendly name, the public address, the Noise port, and the public key.
3. Collect the same information from each of them.
4. In the "Network Config" panel, add each peer with their newly-shared values and save.

The "Participants Status" panel shows handshake / reachability state per peer; healthy peers will turn green within seconds of both sides having added each other.

If a peer is migrating ahead of you, share your old public key and let them add you with that value temporarily — once you have redeployed, share the new key and they can update their entry.

## 5 — Re-enter party credentials

For each decentralized party your node manages, open the "Party Config" dialog and re-enter the Keycloak settings (URL, realm, client ID, client secret). DPM uses these to obtain Canton ledger tokens on behalf of each party.

## Configuration reference

| Variable | Default | Notes |
|---|---|---|
| `DECPM_LISTEN_ADDRESS` | `0.0.0.0` | Noise transport bind address |
| `DECPM_NOISE_PORT` | `9000` | Noise transport port |
| `DECPM_PUBLIC_ADDRESS` | — | **Required.** Hostname peers use to reach this node |
| `DECPM_CANTON_ADMIN_HOST` | — | **Required.** Canton Admin API host |
| `DECPM_CANTON_ADMIN_PORT` | — | **Required.** Canton Admin API port (typ. `5002`) |
| `DECPM_CANTON_LEDGER_HOST` | — | **Required.** Canton Ledger API host |
| `DECPM_CANTON_LEDGER_PORT` | — | **Required.** Canton Ledger API port (typ. `5001`) |
| `DECPM_CANTON_SYNCHRONIZER` | — | **Required.** Synchronizer name (`global` for mainnet) |
| `DECPM_CANTON_NETWORK` | — | **Required.** `mainnet`, `testnet`, or `devnet` |
| `DECPM_KEYCLOAK_URL` | — | **Required.** Keycloak server URL for frontend auth |
| `DECPM_KEYCLOAK_REALM` | — | **Required.** Keycloak realm |
| `DECPM_KEYCLOAK_CLIENT_ID` | — | **Required.** Keycloak client used by the SPA |
| `DECPM_ADMIN_ROLE` | unset | Optional Keycloak role required for privileged endpoints |
| `DECPM_ALLOWED_ORIGIN` | same-origin | Optional CORS origin if UI host ≠ API host |
| `DECPM_DB_ENCRYPTION_KEY` | unset | Optional encryption key for secrets at rest in the SQLite DB |
| `DECPM_TIMEOUT_HANDSHAKE` | `30` | Noise handshake timeout (seconds) |
| `DECPM_TIMEOUT_MESSAGE` | `120` | Noise message timeout (seconds) |
| `DECPM_TIMEOUT_RETRY_ATTEMPTS` | `3` | Connection retry attempts |
| `DECPM_TIMEOUT_RETRY_DELAY` | `5` | Connection retry delay (seconds) |

## Note: keeping the existing PVC

If you would rather not regenerate your Noise keypair (so peers don't have to update their entries for you), you can keep the existing PVC and only replace the Deployment, Service, ConfigMap, and Ingress resources. The new DPM reads its existing SQLite database on first start, so your peer list and Noise keypair carry over automatically.

The trade-off: if there have been schema changes between your old version and the latest, the new DPM may need to migrate the database in place. If the migration fails or you hit unexpected behavior, fall back to the clean-slate approach above.

## Troubleshooting

- **Pod is `CrashLoopBackOff`**: `kubectl logs` will usually show a missing required env var. Compare against the configuration reference above.
- **UI loads but Keycloak login fails**: confirm `<your-ui-host>` is registered as a valid redirect URI on your Keycloak client, and that `DECPM_KEYCLOAK_URL` / `_REALM` / `_CLIENT_ID` match the realm.
- **Peers shown as unreachable**: check that the Noise port (9000) is exposed publicly, that `DECPM_PUBLIC_ADDRESS` resolves to that endpoint, and that the peer has your current public key.
- **Privileged endpoints return 403**: you have `DECPM_ADMIN_ROLE` set but the calling user doesn't have that role assigned in Keycloak. Either grant the role or unset the variable.
