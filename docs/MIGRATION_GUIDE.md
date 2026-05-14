# Dec Party Manager Migration Guide

How to move an existing single-participant Dec Party Manager deployment to the current release.

The format has changed substantially: configuration moved from a mounted TOML file to environment variables, the Service type changed from `LoadBalancer` to `ClusterIP` behind an Ingress, the ConfigMap is gone, and the admin UI is now gated by Keycloak. Rather than patching the old deployment in place, **the cleanest path is a fresh deployment**: tear the old deployment down, apply the new manifests, and re-establish your peer list with the other participants once everyone has redeployed. Because the new deployment generates a fresh Noise keypair on first start (and every other participant going through the same migration will too), there's no value in carrying the old peer list across — every public key in it is about to change. Treat this as a coordinated rebuild, not an in-place upgrade.

This guide assumes:
- One participant per cluster (the most common external setup).
- A working Kubernetes cluster with a Traefik (or other) Ingress controller.
- Access to a Keycloak realm you control (or that an operator has set up for you).
- You already have a Canton participant node running and reachable from the cluster.

## Migration at a glance

1. **Tear down** the old Deployment, ConfigMap, Secret, and LoadBalancer Service.
2. **Pull the latest image tag** and prepare new manifests (Secret, Deployment + PVC, Service, Ingress).
3. **Set up the Keycloak client** that gates the admin UI (one public SPA client per deployment).
4. **Apply the new manifests** and let it start clean.
5. **Re-share public keys** with the other participants and add them as peers in the new UI.
6. **Re-enter party credentials** (Keycloak per-party client info) through the new UI.

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

The Dec Party Manager is published as a public container image:

```
public.ecr.aws/dlc-link/canton-decparty-manager:<tag>
```

Use the latest tagged release (for example `0.0.30`). Pin the version explicitly — do not use `latest`. If your cluster cannot pull from Public ECR directly, mirror the image into your own registry first.

To check the version of a running pod:

```bash
kubectl -n <your-namespace> get deploy dec-party-manager -o jsonpath='{.spec.template.spec.containers[0].image}'
```

Bumping the tag in the Deployment manifest and re-applying is how you upgrade going forward. The application runs any required SQL migrations against its SQLite database automatically on startup, so a tag bump is the only operator step between releases.

## 3 — Set up the Keycloak client

The admin UI authenticates users through Keycloak using the **Authorization Code flow with PKCE**, so it needs a dedicated **public** client in your realm. The client ID you choose here is what goes into `DECPM_KEYCLOAK_CLIENT_ID` in the Secret below; there is no client secret to copy because public SPA clients don't have one.

In the Keycloak admin console, **Clients → Create client**:

| Setting | Value |
|---|---|
| Client type | OpenID Connect |
| Client ID | `dec-party-manager` (or any value — must match `DECPM_KEYCLOAK_CLIENT_ID`) |
| Client authentication | **Off** (public client) |
| Authentication flow | **Standard flow** on; everything else off |
| Root URL | `https://<your-ui-host>` |
| Home URL | `https://<your-ui-host>` |
| Valid redirect URIs | `https://<your-ui-host>/*` |
| Valid post-logout redirect URIs | `https://<your-ui-host>/*` |
| Web origins | `https://<your-ui-host>` (or `+` to derive from the redirect URIs) |

**Web Origins** is the one that catches people out: without it, the browser blocks the SPA's token requests with a CORS error and login silently fails. Set it to your UI host explicitly, or use `+` to mirror the redirect URIs.

In **Advanced → Proof Key for Code Exchange Code Challenge Method**, set the value to `S256`. The frontend always sends PKCE; Keycloak rejects the request if the client isn't configured to expect it.

If you set `DECPM_ADMIN_ROLE` in the Secret (recommended for any shared deployment), also create a realm role with that exact name and assign it to whichever users should have admin powers. Without `DECPM_ADMIN_ROLE` set, every authenticated user is treated as admin.

Per-party Keycloak clients used to fetch Canton ledger tokens are **different** — they are confidential clients (with a secret), one per decentralized party, and you wire them up through the admin UI in Step 6, not here.

### Auth0 alternative

If your organization already uses Auth0, you can run the admin UI against Auth0 instead of Keycloak. Set the `DECPM_AUTH0_*` trio in the Secret (and leave the `DECPM_KEYCLOAK_*` trio unset) — the two providers are mutually exclusive at config-load time. The setup mirrors the Keycloak one: a public SPA application for the browser, plus an API resource whose identifier becomes the audience the SPA requests tokens for.

**1. Create the API** (Auth0 dashboard → **Applications → APIs → Create API**):

| Setting | Value |
|---|---|
| Name | `Decentralization Manager` (display only) |
| Identifier | `https://dec-party-manager/api` (or any unique URI — this is `DECPM_AUTH0_AUDIENCE`) |
| Signing algorithm | `RS256` |

The Identifier never needs to resolve — Auth0 treats it as an opaque string. Pick something stable; changing it later invalidates all issued tokens.

**2. Create the SPA application** (Auth0 dashboard → **Applications → Applications → Create Application**):

| Setting | Value |
|---|---|
| Application type | **Single Page Application** |
| Allowed Callback URLs | `https://<your-ui-host>` |
| Allowed Logout URLs | `https://<your-ui-host>` |
| Allowed Web Origins | `https://<your-ui-host>` |
| Token Endpoint Authentication Method | None (public client) |
| Grant Types | Authorization Code, Refresh Token |

In the application's **APIs** tab, **authorize** it against the API you just created so the SPA is allowed to request tokens with that audience.

**3. Fill the Secret** using values from the Auth0 dashboard:

- `DECPM_AUTH0_DOMAIN` = the tenant domain shown on the application page (for example `your-tenant.us.auth0.com` — no scheme).
- `DECPM_AUTH0_CLIENT_ID` = the application's **Client ID**.
- `DECPM_AUTH0_AUDIENCE` = the API **Identifier** from step 1.

**Allowed Web Origins** plays the same role as Keycloak's Web Origins — without it the browser blocks the SPA's token requests with CORS errors. Set it to your UI host; do not rely on Auth0 deriving it from callback URLs.

**Admin role**: Auth0 does not embed roles in access tokens by default. If you set `DECPM_ADMIN_ROLE`, you must also add an Action under **Actions → Library → Build Custom** that copies the user's role into the token. A minimal Action looks like:

```js
exports.onExecutePostLogin = async (event, api) => {
  const roles = event.authorization?.roles || [];
  api.accessToken.setCustomClaim("roles", roles);
};
```

Attach the Action to the Login flow, then create the matching role under **User Management → Roles** and assign it to the appropriate users.

## 4 — Apply the new manifests

Below is the core single-participant manifest set. Replace every `<...>` placeholder with values for your environment, save as a file, and apply with `kubectl apply -f <file>.yaml`. Public exposure of the Noise port is environment-specific and is covered separately under "Service" below.

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
  # (Keycloak client secrets per party). Any sufficiently long random
  # passphrase — it is hashed with SHA-256 to derive the actual 32-byte
  # key. If unset, secrets are stored in plaintext in the DB.
  # DECPM_DB_ENCRYPTION_KEY: "<long-random-passphrase>"

  # Optional: tighten CORS to a specific origin if the UI is served from a
  # different host than the API (reverse-proxy / dev server). Defaults to
  # same-origin only, which is correct for the Ingress setup below.
  # DECPM_ALLOWED_ORIGIN: "https://<your-ui-host>"

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

## 5 — Re-establish peers

The fresh deployment has generated a new Noise keypair on first start. None of the other participants know it yet, and you do not yet know any of theirs (assuming they are also redeploying). The peer list has to be rebuilt from scratch by exchanging public keys out-of-band.

The UI makes the round-trip simple. For each participant in your network:

1. Open the **Network** panel.
2. Click **Share my data** — this copies your own peer entry (participant id, friendly name, public address, Noise port, public key) to the clipboard as JSON.
3. Send that JSON to your peer through whatever out-of-band channel you use (chat, email, ticket).
4. When a peer sends you their JSON, click **Paste from Clipboard** in your **Network** panel and save. The peer is added with all the right fields filled in.

Repeat with every participant. The "Participants Status" indicators turn green within seconds once both sides have added each other.

If a peer is migrating ahead of you, share your old peer data and let them add you with those values temporarily — once you have redeployed, click **Share my data** again and re-send so they can update their entry.

## 6 — Re-enter party credentials

For each decentralized party your node manages, open the "Party Config" dialog and re-enter the Keycloak settings (URL, realm, client ID, client secret). The application uses these to obtain Canton ledger tokens on behalf of each party.

## Configuration reference

Most variables have a default that's only useful for local development (loopback Canton, devnet, etc.). For a Kubernetes deployment you should set every variable in the "Set for K8s" column even when there is a code default — the defaults shown are what the binary falls back to if the variable is unset, not what your cluster wants.

| Variable | Code default | Set for K8s? | Notes |
|---|---|---|---|
| `DECPM_LISTEN_ADDRESS` | `0.0.0.0` | optional | Noise transport bind address |
| `DECPM_NOISE_PORT` | `9000` | optional | Noise transport port |
| `DECPM_PUBLIC_ADDRESS` | falls back to `DECPM_LISTEN_ADDRESS` | **yes** | Hostname peers use to reach this node from the public internet |
| `DECPM_CANTON_ADMIN_HOST` | `127.0.0.1` | **yes** | Canton Admin API host |
| `DECPM_CANTON_ADMIN_PORT` | `5002` | optional | Canton Admin API port |
| `DECPM_CANTON_LEDGER_HOST` | `127.0.0.1` | **yes** | Canton Ledger API host |
| `DECPM_CANTON_LEDGER_PORT` | `5001` | optional | Canton Ledger API port |
| `DECPM_CANTON_SYNCHRONIZER` | `global` | optional | Synchronizer name |
| `DECPM_CANTON_NETWORK` | `devnet` | **yes** | `mainnet`, `testnet`, or `devnet` |
| `DECPM_KEYCLOAK_URL` | unset | **yes**¹ | Keycloak server URL for frontend auth |
| `DECPM_KEYCLOAK_REALM` | unset | **yes**¹ | Keycloak realm |
| `DECPM_KEYCLOAK_CLIENT_ID` | unset | **yes**¹ | Keycloak client used by the SPA |
| `DECPM_AUTH0_DOMAIN` | unset | **yes**¹ | Auth0 tenant domain for frontend auth (mutually exclusive with `DECPM_KEYCLOAK_*`) |
| `DECPM_AUTH0_CLIENT_ID` | unset | **yes**¹ | Auth0 SPA client ID |
| `DECPM_AUTH0_AUDIENCE` | unset | **yes**¹ | Auth0 API audience targeted by SPA tokens |
| `DECPM_ADMIN_ROLE` | unset | recommended | IdP role required for privileged endpoints. If unset, every authenticated caller is treated as admin. |
| `DECPM_ALLOWED_ORIGIN` | same-origin | optional | CORS origin if UI host ≠ API host |
| `DECPM_DB_ENCRYPTION_KEY` | unset | recommended | Random passphrase (hashed via SHA-256) protecting party secrets at rest. If unset, secrets are stored in plaintext in the SQLite DB. |
| `DECPM_TIMEOUT_HANDSHAKE` | `30` | optional | Noise handshake timeout (seconds) |
| `DECPM_TIMEOUT_MESSAGE` | `120` | optional | Noise message timeout (seconds) |
| `DECPM_TIMEOUT_RETRY_ATTEMPTS` | `3` | optional | Connection retry attempts |
| `DECPM_TIMEOUT_RETRY_DELAY` | `5` | optional | Connection retry delay (seconds) |

¹ Required only for the chosen provider. Set the `DECPM_KEYCLOAK_*` trio **or** the `DECPM_AUTH0_*` trio, not both.

## Note: keeping the existing Noise keypair

If you would rather not regenerate your Noise keypair (so peers don't have to update their entry for you), you can preserve the keypair file from your old PVC. The keypair is stored as a file in the data directory (not in the SQLite database), so it survives a clean redeploy as long as you keep the same PVC mounted at `/app`.

The peer list and party credentials do **not** carry over: those live in the SQLite database, which the new format introduces. The first time the new application starts on the preserved PVC it will create a fresh database alongside the existing keypair file. You will still need to re-establish peers (Step 5) and re-enter party credentials (Step 6) — the only thing you save is the round-trip with peers having to update their entry for you.

## Troubleshooting

- **Pod is `CrashLoopBackOff`**: `kubectl logs` will usually show a missing required env var. Compare against the configuration reference above.
- **UI loads but login fails**: confirm `<your-ui-host>` is registered as a valid redirect URI on your IdP client, and that the `DECPM_KEYCLOAK_*` (or `DECPM_AUTH0_*`) env vars match the IdP. For Auth0, the SPA application must also have the configured audience listed in its Allowed Callback / API Authorization.
- **Peers shown as unreachable**: check that the Noise port (9000) is exposed publicly, that `DECPM_PUBLIC_ADDRESS` resolves to that endpoint, and that the peer has your current public key.
- **Privileged endpoints return 403**: you have `DECPM_ADMIN_ROLE` set but the calling user doesn't have that role assigned in the IdP. Either grant the role or unset the variable.
