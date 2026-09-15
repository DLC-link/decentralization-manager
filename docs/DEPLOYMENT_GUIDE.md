# Dec Party Manager Deployment Guide

How to deploy a Dec Party Manager node to Kubernetes from scratch.

You set every configuration value through an environment variable. The node
reads no TOML file and no ConfigMap.

The node serves **one port, 8080**. That port carries the HTTP admin UI and the
API. An Ingress fronts it, and an identity provider gates it — Keycloak or
Auth0. A separate metrics port stays inside the cluster.

**Nodes never connect to each other.** They coordinate through Canton. The
synchronizer topology store carries partially signed topology proposals, and
Daml contracts carry the workflow intent. So a node needs no public address, no
peer-to-peer port, and no transport key.

The node keeps its SQLite database and its ACS spool directory on a
PersistentVolume. It therefore keeps its state across restarts and image
upgrades.

This guide assumes:
- One participant per cluster (the most common external setup).
- A working Kubernetes cluster with a Traefik (or other) Ingress controller.
- Access to a Keycloak realm (or Auth0 tenant) you control, or that an operator has set up for you.
- You already have a Canton participant node running and reachable from the cluster.

## Deployment at a glance

1. **Pull the latest image tag** and prepare the manifests (Secret, Deployment + PVC, Service, Ingress).
2. **Set up the identity-provider client** that gates the admin UI (one public SPA client per deployment).
3. **Apply the manifests** and let the node start clean.
4. **Set the node identity**: allocate the node party, grant its rights, and `PUT /node-identity`.
5. **Exchange identity strings** with the other operators and add each one as a peer.
6. **Enter party credentials** (per-party IdP client info) through the UI.

The longest step is usually the cross-team coordination needed to exchange identity strings once everyone is deployed.

## 1 — Get the image

The Dec Party Manager is published as a public container image:

```
public.ecr.aws/dlc-link/decentralization-manager:<tag>
```

Use the latest tagged release (for example `v1.6.2`). Pin the version explicitly — do not use `latest`. If your cluster cannot pull from Public ECR directly, mirror the image into your own registry first.

> [!NOTE]
> Releases before v1.6.0 were published under the old repository name,
> `public.ecr.aws/dlc-link/canton-decparty-manager`. That repository stays up for
> existing pins, but new releases only go to `decentralization-manager` — update
> your manifests when you upgrade.

The image is distroless: it contains the `dec-party-manager` binary (entrypoint
`dec-party-manager serve`, binary on `PATH`) and no shell or coreutils. Run
anything that needs `sh` — init containers, `kubectl exec` debugging, exec
probes — on a utility image such as `busybox`, not on this image. The
Deployment example below shows the pattern.

### Root or nonroot

Each release is published as two images:

| Tag | Runs as | `DECPM_DIR` default |
|---|---|---|
| `…:<tag>` | uid 0 (root) | `/` |
| `…:<tag>-nonroot` | uid 65532 | `/home/nonroot` |

They hold the same binary and take the same configuration; only the runtime
identity differs. **Prefer `-nonroot`.** It is what an admission policy that
inspects the image expects — a bare `runAsNonRoot: true` with no `runAsUser`
rejects the plain tag, because the image itself declares uid 0. The plain tag
stays for existing pins and for clusters that set `runAsUser` in the pod spec.

Neither one needs root: the binary binds 8080 and the metrics port 9464, both
above 1024, and writes only under its data directory. Give the data volume an `fsGroup` so uid
65532 can write to it — the Deployment example does, and that example runs
either tag as 65532 because it pins `runAsUser` itself.

### Moving an existing node to uid 65532

A volume written by the root image holds root-owned files. `fsGroup` does not
fix that on its own: it re-owns the contents to the **group** and adds group
write, but the user owner stays root. The SQLite database is the file that fails
first. uid 65532 cannot open a root-owned `data/decpm.db` for writing, so the
node cannot run its migrations and the pod exits.

Chown the volume once, with a throwaway root init container ahead of the
existing one:

```yaml
initContainers:
  - name: chown-data
    image: busybox:latest
    command: ["sh", "-c", "chown -R 65532:65532 /app"]
    securityContext:
      runAsUser: 0
      runAsNonRoot: false
    volumeMounts:
      - name: data
        mountPath: /app
```

Drop it again after one successful start — from then on the node owns
everything it writes. A namespace under the `restricted` Pod Security Standard
rejects that container; run the same `chown -R` from a one-off root pod that
mounts the PVC instead. A bind-mounted `docker run` has the same requirement and
the same fix.

> Releases up to and including **v1.6.2** shipped only the root image. To
> harden one of those without upgrading, do the one-time chown above, then set
> `runAsUser: 65532` and the same `fsGroup` on the pod; the binary never needed
> the privileges it had.

To check the version of a running pod:

```bash
kubectl -n <your-namespace> get deploy dec-party-manager -o jsonpath='{.spec.template.spec.containers[0].image}'
```

Bumping the tag in the Deployment manifest and re-applying is how you upgrade going forward. The application runs any required SQL migrations against its SQLite database automatically on startup, so a tag bump is the only operator step between releases.

## 2 — Set up the identity-provider client

The admin UI authenticates users through your identity provider using the **Authorization Code flow with PKCE**, so it needs a dedicated **public** client. Configure either Keycloak **or** Auth0 — the two providers are mutually exclusive at config-load time.

> [!WARNING]
> There is an `--insecure` / `DECPM_INSECURE` mode that bypasses the identity provider entirely (any inbound token is accepted and an unsafe shared-secret token is sent to Canton). It exists for local development against an unsafe-auth Canton and **must never be set in a production deployment.** Leave it unset (the default) for every environment described in this guide.

### Keycloak

You need a **public** client in your realm. The client ID you choose here is what goes into `DECPM_KEYCLOAK_CLIENT_ID` in the Secret below; there is no client secret to copy because public SPA clients don't have one.

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

### Auth0

If your organization already uses Auth0, you can run the admin UI against Auth0 instead of Keycloak. Set the `DECPM_AUTH0_*` trio in the Secret (and leave the `DECPM_KEYCLOAK_*` trio unset). The setup mirrors the Keycloak one: a public SPA application for the browser, plus an API resource whose identifier becomes the audience the SPA requests tokens for.

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
- `DECPM_AUTH0_SCOPE` = extra space-separated scopes the SPA should request, when the admin role is granted as a resource-server scope (see **Admin role** below).
- `DECPM_JWT_ROLE_CLAIM` = a URI-namespaced claim owned by your deployment (for example `https://example.com/decman/roles`) when the admin role arrives as a custom claim.

**Allowed Web Origins** plays the same role as Keycloak's Web Origins — without it the browser blocks the SPA's token requests with CORS errors. Set it to your UI host; do not rely on Auth0 deriving it from callback URLs.

**Admin role**: Auth0 does not embed roles in access tokens by default, so with
`DECPM_ADMIN_ROLE` set you must pick one of the three carriers below. Create the
role under **User Management → Roles** and assign it to the appropriate users
first; the name must match `DECPM_ADMIN_ROLE` exactly.

*Option 1 — RBAC permissions (no Action needed, recommended).* On the API from
step 1, enable **RBAC** and **Add Permissions in the Access Token**, add a
permission named after your `DECPM_ADMIN_ROLE`, and grant it to the role. Auth0
then emits a `permissions` array that DecMan reads directly. Auth0 does not
filter that array by the scopes the SPA requested, so no further configuration
is required.

*Option 2 — RBAC scope.* Same API settings, but set
`DECPM_AUTH0_SCOPE=<your-admin-role>`. Auth0 returns a permission in `scope`
only when the client asked for it, and the SPA cannot ask for what the backend
never named. The scopes you list here are added to Auth0's default
`openid profile email`, not substituted for it.

*Option 3 — namespaced custom claim.* Add an Action under **Actions → Library →
Build Custom**, attached to the Login flow:

```js
exports.onExecutePostLogin = async (event, api) => {
  const roles = event.authorization?.roles || [];
  api.accessToken.setCustomClaim("https://example.com/decman/roles", roles);
};
```

Then set `DECPM_JWT_ROLE_CLAIM=https://example.com/decman/roles`, replacing the
example namespace with one controlled by your deployment. Auth0 silently drops
custom claims that are not URI-namespaced, so the namespace is not optional.

DecMan also reads the standard flat `roles`, `realm_access.roles`, and `scope`
carriers with no configuration at all.

## 3 — Apply the manifests

Below is the core single-participant manifest set. Replace every `<...>` placeholder with values for your environment, save as a file, and apply with `kubectl apply -f <file>.yaml`. Only port 8080 needs to reach the operator, and the Ingress carries it.

### 3a. Namespace (skip if it already exists)

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: <your-namespace>
```

### 3b. Secret (configuration)

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: dec-party-manager-secrets
  namespace: <your-namespace>
type: Opaque
stringData:
  # Canton participant node connection (Admin + Ledger gRPC APIs).
  # The node reaches its own participant only. It never dials another node.
  DECPM_CANTON_ADMIN_HOST: "<canton-admin-host>"
  DECPM_CANTON_ADMIN_PORT: "5002"
  DECPM_CANTON_LEDGER_HOST: "<canton-ledger-host>"
  DECPM_CANTON_LEDGER_PORT: "5001"
  DECPM_CANTON_NETWORK: "mainnet"          # mainnet | testnet | devnet
  DECPM_CANTON_SYNCHRONIZER: "global"

  # Set when the participant serves its APIs over TLS. Point the CA vars at
  # the issuing CA if it is a private one; omit them to use the platform
  # trust store. See the environment table below for mTLS and SNI overrides.
  # DECPM_CANTON_ADMIN_TLS: "true"
  # DECPM_CANTON_ADMIN_TLS_CA_CERT: "/etc/decman/tls/canton-ca.pem"
  # DECPM_CANTON_LEDGER_TLS: "true"
  # DECPM_CANTON_LEDGER_TLS_CA_CERT: "/etc/decman/tls/canton-ca.pem"

  # Keycloak (gates the admin UI).
  DECPM_KEYCLOAK_URL: "https://<your-keycloak-host>"
  DECPM_KEYCLOAK_REALM: "<your-realm>"
  DECPM_KEYCLOAK_CLIENT_ID: "<frontend-client-id>"

  # Optional: require a specific Keycloak role on every authenticated caller
  # before they can hit privileged endpoints (PUT /party-config, /kick, etc.).
  # If unset, every authenticated user is treated as admin — fine for a
  # single-operator deployment, dangerous for shared environments.
  # DECPM_ADMIN_ROLE: "decman-admin"

  # Optional: encryption key for secrets stored in the SQLite database
  # (Keycloak client secrets per party). Any sufficiently long random
  # passphrase — it is hashed with SHA-256 to derive the actual 32-byte
  # key. If unset, secrets are stored in plaintext in the DB.
  # DECPM_DB_ENCRYPTION_KEY: "<long-random-passphrase>"

  # Optional: tighten CORS to a specific origin if the UI is served from a
  # different host than the API (reverse-proxy / dev server). Defaults to
  # same-origin only, which is correct for the Ingress setup below.
  # DECPM_ALLOWED_ORIGIN: "https://<your-ui-host>"

  # Coordination settings. Each one has a working default, so set one only
  # when you need a different value. See the configuration reference below.
  # DECPM_OBSERVER_POLL_SECS: "10"          # mainnet guidance; default 3
  # DECPM_HEARTBEAT_INTERVAL_SECS: "3600"
  # DECPM_HEARTBEAT_MIN_INTERVAL_SECS: "60"
  # DECPM_PEER_STALE_FACTOR: "3"
  # DECPM_PROPOSAL_TTL_SECS: "604800"
  # Upload and vet the embedded coordination DAR at startup. Leave it on
  # unless your participant refuses uploads from decman.
  # DECPM_AUTO_UPLOAD_COORDINATION_DAR: "true"
  # Where add-party ACS snapshots are spooled. Default: {DECPM_DIR}/data/acs.
  # Keep it on the PersistentVolume; see "Sizing the volume" below.
  # DECPM_ACS_SPOOL_DIR: "/app/data/acs"
```

### 3c. Deployment + PersistentVolumeClaim

The PVC holds the SQLite database and the ACS spool directory — keep it for the lifetime of the node. Deleting it wipes the node identity, the peer list, the party credentials, and every workflow run.

**Sizing the volume.** The database stays small. It holds peers, credentials,
and run rows, not ledger data. The ACS spool decides the size.

An add-party run writes one gzip snapshot of the party's active contract set per
joining participant. The node deletes that file once it observes the joiner as
`onboarded == true`, or once the operator dismisses the run. A party with no
contracts writes an empty snapshot, and the run then skips the transfer.

Both sides of an add-party run use the spool. A current host writes the export
there, and the joining node streams the upload into it before it imports.

Size the volume for the largest snapshot you expect, times the number of
add-party runs you expect at the same time, plus 1 GiB of headroom. The example
below requests 5 GiB, which suits a party of a few hundred thousand contracts.

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
      storage: 5Gi
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
        # A Prometheus-style collector reads `app` as the service name.
        app: dec-party-manager
      annotations:
        prometheus.io/scrape: "true"
        prometheus.io/path: "/metrics"
        prometheus.io/port: "9464"
    spec:
      # The app needs no root. Pinning runAsUser here means this works with
      # either published tag; with `-nonroot` it matches the image default.
      # fsGroup makes the mounted PVC group-writable by that uid, which is what
      # lets both the init container and the app create files under /app.
      securityContext:
        runAsNonRoot: true
        runAsUser: 65532
        runAsGroup: 65532
        fsGroup: 65532
      initContainers:
        - name: init-data
          image: busybox:latest
          command: ["sh", "-c", "mkdir -p /app/data"]
          # Inherits the pod securityContext above, so it runs as 65532 too.
          # busybox defaults to root, and with runAsNonRoot set the pod would
          # refuse to start if this container were left to that default.
          volumeMounts:
            - name: data
              mountPath: /app
      containers:
        - name: dec-party-manager
          # Append `-nonroot` for the image that runs as 65532 by itself.
          image: public.ecr.aws/dlc-link/decentralization-manager:<tag>
          imagePullPolicy: Always
          securityContext:
            allowPrivilegeEscalation: false
            capabilities:
              drop: ["ALL"]
            # The server writes only under its data directory, so a read-only
            # root filesystem should hold. Left commented because it has not
            # been proven against a live deploy: a dependency writing to /tmp
            # would surface only at runtime. Enable it, then watch one restart.
            # readOnlyRootFilesystem: true
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
            - name: metrics
              containerPort: 9464
              protocol: TCP
          volumeMounts:
            - name: data
              mountPath: /app
          resources:
            requests: { memory: "128Mi", cpu: "100m" }
            limits:   { memory: "1Gi", cpu: "500m" }
          env:
            - name: RUST_LOG
              value: dec_party_manager=info
            # Logs are JSON on stdout by default. The collector indexes the
            # fields as attributes only where a log pipeline parses the line,
            # so this environment needs its own decman pipeline in dlc-infra.
            # Set DECPM_LOG_FORMAT=text for the console format instead.
            # Prometheus metrics are served on this port, separate from the API
            # port so the Ingress never exposes them. The listener answers any
            # caller that can reach it, so the port must stay inside the cluster.
            # Set 0 to disable.
            - name: DECPM_METRICS_PORT
              value: "9464"
          envFrom:
            - secretRef:
                name: dec-party-manager-secrets
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: dec-party-manager-data
```

### 3d. Service

The deployment needs **one** Service: a `ClusterIP` that backs the Ingress for
the HTTP admin UI. Nodes never dial each other, so nothing has to reach this
pod from outside the cluster except your own operators.

The Service does not expose the metrics port. The metrics listener has no
authentication, and it answers any caller that can reach it. Keep port 9464
inside the cluster: bind it to a private interface, or block it in the host
firewall or security group. An operator with no metrics collector should set
`DECPM_METRICS_PORT=0` instead, which serves nothing.

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
  selector:
    app.kubernetes.io/name: dec-party-manager
```

> [!NOTE]
> A node upgraded from 1.8.x still carries a `dec-party-manager-noise`
> LoadBalancer Service and a second port on its ClusterIP Service. Delete both
> after the cutover. The 2.0 binary binds no port 9000, so that load balancer
> forwards to a closed port and still costs money.

### 3e. Ingress (Traefik example)

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

Adapt the `ingressClassName` and TLS configuration to your cluster's Ingress controller (nginx, Contour, etc.). Make sure `<your-ui-host>` resolves to the cluster and is registered as a valid redirect URI on your IdP client.

### Apply

```bash
kubectl apply -f <each-of-the-above>.yaml
kubectl -n <your-namespace> rollout status deploy/dec-party-manager
```

Once the pod is `Running`, hit `https://<your-ui-host>` in a browser. Your identity provider should challenge for login.

## 4 — Set the node identity

Each node has one **node party**. The node party signs everything this node
writes on the ledger: its registry entry, its workflow proposals, its
acceptances, its submission signatures, its ACS manifests, and its heartbeats.
A node coordinates nothing until you set its node party.

The node party is a normal Canton party with three requirements:

- Your participant hosts it with **Submission** permission. A Confirmation-only
  host cannot submit Daml commands, so `PUT /node-identity` rejects it.
- A Ledger API user holds `CanActAs` and `CanReadAs` on it.
- It exists before any decentralized party exists.

### 4a. Allocate the party on your participant

Allocate it through your participant's own admin path — the Canton console, the
JSON Ledger API, or your platform's tooling. Use a stable party-id hint such as
`decman-node-1`. Canton appends your participant's namespace, so the full id
looks like `decman-node-1::1220abc...`.

Then grant the Ledger API user both rights on that party. Substitute your own
user id for `<ledger-api-user>`:

```json
{
  "userId": "<ledger-api-user>",
  "rights": [
    { "kind": { "CanActAs":  { "value": { "party": "decman-node-1::1220abc..." } } } },
    { "kind": { "CanReadAs": { "value": { "party": "decman-node-1::1220abc..." } } } }
  ],
  "identityProviderId": ""
}
```

Post that body to `/v2/users/<ledger-api-user>/rights` on the JSON Ledger API,
or grant the same two rights from the Canton console.

### 4b. Tell decman about it

`PUT /node-identity` stores the party and the credentials decman uses to mint
its ledger tokens. The endpoint requires the admin role. It is exempt from
authentication only while the credentials table is entirely empty, which is the
case on a node you have just deployed:

```bash
curl -X PUT https://<your-ui-host>/node-identity \
  -H 'Authorization: Bearer <admin-jwt>' \
  -H 'Content-Type: application/json' \
  -d '{
    "node_party_id":          "decman-node-1::1220abc...",
    "user_id":                "<ledger-api-user>",
    "keycloak_url":           "https://<your-keycloak-host>",
    "keycloak_realm":         "<your-realm>",
    "keycloak_client_id":     "<node-party-client-id>",
    "keycloak_client_secret": "<node-party-client-secret>"
  }'
```

The client here is a **confidential** client that mints Canton ledger tokens for
the node party, not the public SPA client from Step 2. Auth0 deployments send
`auth0_domain`, `auth0_audience`, `auth0_client_id`, and `auth0_client_secret`
instead.

Before it stores anything, the handler reads the head `PartyToParticipant` of
the node party. It returns 400 unless this participant hosts the party with
Submission permission, and the message names what it found instead.

Check the result:

```bash
curl -H 'Authorization: Bearer <admin-jwt>' https://<your-ui-host>/node-identity
```

A configured node answers `configured: true` with its `node_party_id`, its
`participant_id`, and its `hosting_permission`.

### 4c. The coordination DAR

Every node runs the same Daml package, `decman-coordination-v1`. Its templates
carry the registry entries, the workflow proposals, the submission rounds, and
the ACS manifests.

The binary embeds the DAR. At startup a background task uploads it to your
participant and vets it. The task retries with a growing backoff until the
package is vetted, so a participant that is still starting costs you nothing.
`DECPM_AUTO_UPLOAD_COORDINATION_DAR` controls the task and defaults to `true`.

`GET /node-health` reports the task under `coordination_dar`. Its `phase` is
`pending`, `disabled`, `uploading`, or `ready`, and `last_error` names the last
failure.

Wait for `ready` before you go on:

```bash
curl -H 'Authorization: Bearer <admin-jwt>' https://<your-ui-host>/node-health \
  | jq '.coordination_dar'
```

Set `DECPM_AUTO_UPLOAD_COORDINATION_DAR=false` when your policy forbids an
automatic upload. You then upload the DAR yourself through `POST /dars/upload`,
and `phase` reports `disabled` until you do.

This package is a prerequisite for coordination, not a convenience. Canton
rejects a contract whose observer's participant has not vetted the package. A
node therefore leaves an unvetted peer out of its registry entry, and retries on
the next observer tick.

## 5 — Exchange peer identities

Operators exchange **one string per peer**, out of band:

```
participant_id,node_party_id,name
```

That is the whole exchange. There is no address, no port, and no key, because
nodes never open a connection to each other.

For each participant in your network:

1. Open the **Network** panel.
2. Click **Share my identity**. The button copies your own
   `participant_id,node_party_id,name` string to the clipboard. It stays
   disabled until you set the node identity in Step 4.
3. Send that string to the other operator through whatever out-of-band channel
   you use (chat, email, ticket).
4. When an operator sends you theirs, click **Paste from Clipboard** in your
   **Network** panel and save.

The peers table then holds a participant id, a name, and a node party per peer.
A peer without a node party cannot be invited to a workflow.

### How to read the peer status

Each node publishes one `DecmanNode` contract that names its peers as observers,
and re-creates it on a heartbeat timer. Your node reads the entries its peers
published and reports one status per peer:

| Status | Meaning |
|---|---|
| `Active` | The peer's entry is visible and its last heartbeat is recent. |
| `Stale` | The entry is visible, but the last heartbeat is older than `DECPM_PEER_STALE_FACTOR` × the peer's heartbeat interval. |
| `Unknown` | The peer has vetted the package, but no entry of theirs is visible. They have not added you yet. |
| `Unvetted` | The peer's participant has not vetted the coordination package. |

`GET /registry` returns the entries themselves, in three buckets. `self_entry`
holds your own. `peers` holds the entries a configured peer signed. `inbound`
holds entries whose signatory you have not added as a peer yet.

Each entry carries the publisher's version, its heartbeat interval, its
`last_active_at`, and its `hosting_verified` flag.

The status describes the age of a heartbeat, not a live connection. A peer turns
`Active` only after **both** sides add each other. You see a peer's entry only
when that peer named your node party as an observer of it.

Vetting comes before the mutual add. A node names a peer as observer only after
that peer's participant has vetted the coordination package, so an `Unvetted`
peer never reaches `Active`.

A proposer checks every invitee before it starts a workflow. It refuses with a
409 unless the invitee has vetted the package, has published a visible registry
entry, and reports a coordination version it understands. The 409 names each
participant that is not ready.

## 6 — Enter party credentials

For each decentralized party your node manages, open the "Party Config" dialog and enter the Keycloak settings (URL, realm, client ID, client secret). The application uses these to obtain Canton ledger tokens on behalf of each party.

## Configuration reference

Most variables have a default that's only useful for local development (loopback Canton, devnet, etc.). For a Kubernetes deployment you should set every variable in the "Set for K8s" column even when there is a code default — the defaults shown are what the binary falls back to if the variable is unset, not what your cluster wants.

| Variable | Code default | Set for K8s? | Notes |
|---|---|---|---|
| `DECPM_HOST` | `0.0.0.0` | optional | HTTP bind address for the admin UI and API |
| `DECPM_PORT` | `8080` | optional | HTTP port for the admin UI and API |
| `DECPM_METRICS_PORT` | `9464` | optional | Prometheus metrics port, separate from the API port. `0` serves no metrics. The listener is unauthenticated, so keep the port off the public internet (see [Service](#3d-service)) |
| `DECPM_REWARD_AUTOMATION_INTERVAL_SECS` | `300` | optional | How often the CIP-104 reward automation sweeps each decparty for unassigned coupons |
| `DECPM_REWARD_EXPIRY_READ_INTERVAL_SECS` | `3600` | optional | How often the automation re-reads the backlog purely to refresh `decman_reward_oldest_unassigned_expires_in_seconds`. Both expiry alert rules read that gauge, so this bounds how stale their input can get. A sweep reads the ledger too, so the gauge refreshes at whichever interval is shorter |
| `DECPM_REWARD_MAX_CREATES` | `100` | optional | Output contracts one `Delegation_Assign` may create, bounding the coupons per transaction. Lower it if assigns start failing |
| `DECPM_REWARD_MIN_EXPIRY_MARGIN_SECS` | `120` | optional | Time a coupon must have left before expiry to be assigned |
| `DECPM_CANTON_ADMIN_HOST` | `127.0.0.1` | **yes** | Canton Admin API host |
| `DECPM_CANTON_ADMIN_PORT` | `5002` | optional | Canton Admin API port |
| `DECPM_CANTON_LEDGER_HOST` | `127.0.0.1` | **yes** | Canton Ledger API host |
| `DECPM_CANTON_LEDGER_PORT` | `5001` | optional | Canton Ledger API port |
| `DECPM_CANTON_SYNCHRONIZER` | `global` | optional | Synchronizer name |
| `DECPM_CANTON_ADMIN_TLS` | `false` | optional | Speak TLS to the Admin API. Leave off for a participant reachable only over a trusted private network |
| `DECPM_CANTON_ADMIN_TLS_CA_CERT` | unset | optional | PEM of the CA that issued the Admin API certificate. Required when that CA is private; the platform trust store is used when unset |
| `DECPM_CANTON_ADMIN_TLS_CLIENT_CERT` | unset | optional | PEM client certificate, for an Admin API requiring mTLS. Set with the key |
| `DECPM_CANTON_ADMIN_TLS_CLIENT_KEY` | unset | optional | PEM client key matching the certificate |
| `DECPM_CANTON_ADMIN_TLS_DOMAIN` | unset | optional | Name to validate the Admin API certificate against, when it differs from the host — e.g. a certificate issued for a service DNS name while DecMan connects by IP |
| `DECPM_CANTON_LEDGER_TLS` | `false` | optional | As above, for the Ledger API |
| `DECPM_CANTON_LEDGER_TLS_CA_CERT` | unset | optional | |
| `DECPM_CANTON_LEDGER_TLS_CLIENT_CERT` | unset | optional | |
| `DECPM_CANTON_LEDGER_TLS_CLIENT_KEY` | unset | optional | |
| `DECPM_CANTON_LEDGER_TLS_DOMAIN` | unset | optional | |
| `DECPM_CANTON_NETWORK` | `devnet` | **yes** | `mainnet`, `testnet`, or `devnet` |
| `DECPM_KEYCLOAK_URL` | unset | **yes**¹ | Keycloak server URL for frontend auth |
| `DECPM_KEYCLOAK_REALM` | unset | **yes**¹ | Keycloak realm |
| `DECPM_KEYCLOAK_CLIENT_ID` | unset | **yes**¹ | Keycloak client used by the SPA |
| `DECPM_AUTH0_DOMAIN` | unset | **yes**¹ | Auth0 tenant domain for frontend auth (mutually exclusive with `DECPM_KEYCLOAK_*`) |
| `DECPM_AUTH0_CLIENT_ID` | unset | **yes**¹ | Auth0 SPA client ID |
| `DECPM_AUTH0_AUDIENCE` | unset | **yes**¹ | Auth0 API audience targeted by SPA tokens |
| `DECPM_AUTH0_SCOPE` | unset | optional | Extra space-separated scopes the SPA requests, added to Auth0's default `openid profile email`. Needed only when the admin role is granted as an RBAC scope |
| `DECPM_JWT_ROLE_CLAIM` | unset | optional | Provider-specific JWT claim containing a role-name array; needed only when the admin role arrives as a namespaced custom claim |
| `DECPM_ADMIN_ROLE` | unset | recommended | IdP role required for privileged endpoints. If unset, every authenticated caller is treated as admin. |
| `DECPM_ALLOWED_ORIGIN` | same-origin | optional | CORS origin if UI host ≠ API host |
| `DECPM_DB_ENCRYPTION_KEY` | unset | recommended | Random passphrase (hashed via SHA-256) protecting party secrets at rest. If unset, secrets are stored in plaintext in the SQLite DB. |
| `DECPM_LOG_FORMAT` | `json` | optional | Leave it unset in a cluster, because the log pipeline parses JSON only. `text` gives the console format for local work |

¹ Required only for the chosen provider. Set the `DECPM_KEYCLOAK_*` trio **or** the `DECPM_AUTH0_*` trio, not both.

### Coordination settings

These control how the node coordinates through Canton. Every one has a working
default; set one only to change that default.

| Variable | Code default | Set for K8s? | Notes |
|---|---|---|---|
| `DECPM_OBSERVER_POLL_SECS` | `3` | recommended | How often the observer loop polls the ledger and the topology store. Raise it to `10` on mainnet, where a shorter poll buys little and costs Admin API calls. Minimum 1 |
| `DECPM_HEARTBEAT_INTERVAL_SECS` | `3600` | optional | How often this node re-creates its `DecmanNode` registry contract. Every heartbeat is a Daml transaction, so a short interval costs traffic. Minimum 1 |
| `DECPM_HEARTBEAT_MIN_INTERVAL_SECS` | `60` | optional | Floor the template enforces between two heartbeats, written into the contract. Clamped to `[1, DECPM_HEARTBEAT_INTERVAL_SECS]` |
| `DECPM_PEER_STALE_FACTOR` | `3` | optional | A peer reads as `Stale` once its last heartbeat is older than this multiple of its own heartbeat interval. Minimum 1 |
| `DECPM_PROPOSAL_TTL_SECS` | `604800` | optional | Lifetime of a `WorkflowProposal`, seven days by default. An invitee cannot accept a proposal past its `expiresAt`. Minimum 1 |
| `DECPM_AUTO_UPLOAD_COORDINATION_DAR` | `true` | optional | Upload and vet the embedded `decman-coordination-v1` DAR at startup. Set `false` when your policy requires a manual upload through `POST /dars/upload` |
| `DECPM_COORDINATION_PACKAGE_REF` | `#decman-coordination-v1` | optional | Package reference the node resolves the coordination templates through. Change it only to run a forked package |
| `DECPM_ACS_SPOOL_DIR` | `{DECPM_DIR}/data/acs` | optional | Directory for add-party ACS snapshots. Keep it on the PersistentVolume; see "Sizing the volume" above |

### Removed in 2.0

The 2.0 binary rejects an unknown CLI argument, and it ignores these variables.
Delete them from your Secret and your Deployment during the cutover:

`DECPM_LISTEN_ADDRESS`, `DECPM_NOISE_PORT`, `DECPM_PUBLIC_ADDRESS`,
`DECPM_TIMEOUT_HANDSHAKE`, `DECPM_TIMEOUT_MESSAGE`,
`DECPM_TIMEOUT_RETRY_ATTEMPTS`, `DECPM_TIMEOUT_RETRY_DELAY`,
every `DECPM_NOISE_RETRY_*`, `DECPM_PEER_WAIT_POLL_DELAY_MS`, and
`DECPM_ACS_BLOCK_BYTES`.

## Cutover from 1.8.x to 2.0

1.8.x nodes coordinate over a direct connection between nodes. 2.0 nodes
coordinate through Canton. The two builds share no coordination path, so the
network cuts over as a group.

A mixed window is nonetheless safe, because both builds refuse to start a
workflow the other side cannot finish. An old build refuses once an invitee's
listener is gone. A new build refuses (409) until every invitee has vetted the
coordination package and published a registry entry. Nothing half-runs.

Migration `000021` marks every `inprogress` run failed, with a message that
tells the operator to dismiss the card and start the operation again.

### Before the window, while every node still runs 1.8.x

1. Call `GET /decentralized-parties?refresh=true` on every node. Treat a NULL
   `dec_party_participant.signing_key` for any member as a blocker: that node
   cannot attribute its own Daml key after the cutover. Fix it before you
   upgrade.
2. Drain the network. `GET /workflows` and `GET /invitations` must come back
   empty on every node. Finish, cancel, or dismiss whatever is left.

### Inside the window, on every node

3. Delete the removed environment variables and the removed CLI arguments from
   your manifests. The 2.0 binary rejects an unknown argument and does not
   start.
4. Stop the node, retag the image to 2.0.0, and start it. Migrations `000020`
   and `000021` run on boot.
5. Set the node identity with an admin JWT (see Step 4 above). The bootstrap
   exemption does not apply to an upgraded node, which already holds credential
   rows.
6. Wait until `GET /node-health` reports `coordination_dar.phase == "ready"`.
7. Exchange `participant_id,node_party_id,name` with every operator and re-add
   every peer (see Step 5 above). The upgrade drops the address, port, and key
   columns, so every peer row needs a node party.

### After the window

8. Verify on every node that `GET /registry` lists every peer under `peers` and
   that `inbound` is empty. An entry under `inbound` means that operator added
   you before you added them.
9. Verify that `GET /packages/vetted` reports `decman-coordination-v1` under
   `package_name` on every node.
10. Delete the `dec-party-manager-noise` LoadBalancer Service, the second port
    on the ClusterIP Service, and any firewall rule for port 9000.
11. Start workflows. A 409 names the members that still run 1.8.x.

### Rollback

Rollback is manual and lossy. The 2.0 build never reads or writes
`data/noise.key`. A node that kept its volume therefore still holds its own
transport key. The addresses and keys of its **peers** are gone, because
migration `000021` drops those three columns.

1. Stop the node.
2. Apply `crates/decman/migrations/000021_noise_sunset.down.sql` with
   `sqlite3` against `data/decpm.db`.
3. Delete the version-21 row from `_sqlx_migrations`. Leave migration `000020`
   in place: it only adds a column and a table that 1.8.x ignores.
4. Restart 1.8.x and re-enter every peer by hand: address, port, and public
   key come back empty.

The down migration restores the three peer columns with empty defaults and
renames `coordinator_participant` back to `coordinator_pubkey`. It does not
restore the values those columns held, and it does not un-fail the runs that
migration `000021` marked failed.

## Troubleshooting

- **Pod is `CrashLoopBackOff`**: `kubectl logs` will usually show a missing required env var. Compare against the configuration reference above.
- **UI loads but login fails**: confirm `<your-ui-host>` is registered as a valid redirect URI on your IdP client, and that the `DECPM_KEYCLOAK_*` (or `DECPM_AUTH0_*`) env vars match the IdP. For Auth0, the SPA application must also have the configured audience listed in its Allowed Callback / API Authorization.
- **A peer reads as `Unvetted`**: that peer's participant has not vetted `decman-coordination-v1`. Ask the operator to check `GET /node-health` on their node. Your node leaves an unvetted peer out of its own registry entry until they vet it.
- **A peer reads as `Unknown`**: the peer has vetted the package, but no registry entry of theirs is visible to you. They have not added you as a peer yet. Send them your identity string again, and confirm the `node_party_id` in it matches what they saved.
- **A peer reads as `Stale`**: the peer's last heartbeat is older than `DECPM_PEER_STALE_FACTOR` × its heartbeat interval. The peer's node is down, or its observer loop is stuck. Check that node's logs. A stale peer is not a network fault: nodes hold no connection to each other.
- **Every peer reads as `Unknown` and the UI shows a "configure node identity" banner**: this node has no node identity. Run Step 4.
- **`PUT /node-identity` returns 400**: this participant does not host the party with Submission permission. The message names the permission it found. Re-allocate the party on this participant, or raise its permission.
- **Starting a workflow returns 409**: an invitee is not ready. The message names each participant and why — no vetted package, no visible registry entry, or a coordination version that is too old.
- **`coordination_dar.phase` stays `uploading`**: read `last_error` in the same object. The two usual causes are a participant that is not connected to the synchronizer, and an Admin API that refuses the upload. The task keeps retrying, so fix the cause and wait.
- **`POST /acs-import` returns 404**: no `AcsManifest` from that exporter names this participant at that activation serial. Check the `exporter` and `serial` query parameters against `GET /acs-manifests/{party}`.
- **`POST /acs-import` returns 400**: the uploaded file does not match the manifest. The snapshot was re-exported, or the download truncated. Download it again with `GET /acs-export/{party}/{target}` and retry.
- **Every Canton call fails with `transport error` / `BrokenPipe`, immediately and permanently**: the channel's TLS setting does not match what the endpoint speaks. A plaintext client against a TLS listener has its connection closed on the first bytes, which looks identical to the participant being down. Confirm with `grpcurl -plaintext <host>:<port> list` — if that fails but `grpcurl <host>:<port> list` succeeds, the endpoint is TLS: set `DECPM_CANTON_ADMIN_TLS=true` (and `DECPM_CANTON_LEDGER_TLS=true` for the ledger API), plus `..._TLS_CA_CERT` when a private CA issued the certificate. The connect error message names the variable to change in either direction.
- **Privileged endpoints return 403**: you have `DECPM_ADMIN_ROLE` set but the calling user doesn't have that role assigned in the IdP, or the role never reaches the token. On Auth0, decode the access token and check for the role in `permissions`, `scope`, or your `DECPM_JWT_ROLE_CLAIM`; a token carrying only `openid profile email` means no carrier is configured (see **Admin role**). Correct the carrier, grant the role, or unset the admin-role gate.
