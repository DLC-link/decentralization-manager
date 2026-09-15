# Canton Decentralized Party Manager

A web application for managing decentralized parties in Canton blockchain networks. Provides a user interface for onboarding new parties, deploying governance contracts, and managing participant membership.

## Features

- **Web-Based Management UI**: React frontend for managing decentralized parties
- **Canton-Native Coordination**: nodes never connect to each other. The synchronizer topology store holds the signatures, and Daml contracts hold the workflow intent
- **Multi-Party Onboarding**: invitation-based workflow that creates a decentralized party namespace
- **Contract Deployment**: hash-pinned DAR files and governance contracts that the party's members sign together
- **Governance Actions**: View and manage governance confirmations with threshold-based execution
- **Participant Management**: view party membership, add a member, remove a member, and change the signing threshold
- **Node Registry**: every node publishes a `DecmanNode` contract and heartbeats on it, so operators see each peer as Active, Stale, Unknown, or Unvetted
- **OAuth Authentication (Keycloak or Auth0)**: Supports M2M (client_credentials) and password flows for Ledger API access; per-node choice of provider for both frontend gating and outbound Canton tokens
- **Canton Integration**: Native gRPC integration with Canton Admin and Ledger APIs

## Documentation

- [Architecture Overview](docs/ARCHITECTURE.md) -- System architecture, core concepts, the coordination model, and technical constraints
- [User Guide](USER_GUIDE.md) -- Walkthrough of the web UI for day-to-day party and governance operations
- [Decentralizing an Existing Party](docs/DECENTRALIZING_AN_EXISTING_PARTY.md) -- Adding hosts to a party that already exists, and converting a local party
- [Custom Daml Templates](docs/CUSTOM_DAML_TEMPLATES.md) -- Authoring and deploying your own Daml governance templates
- [Deployment Guide](docs/DEPLOYMENT_GUIDE.md) -- Deploying a node to Kubernetes from scratch: manifests, identity-provider setup, and configuration reference
- [Use Cases](docs/USE_CASES.md) -- Joint custody governance, FAR rewards, multi-sig wallet, and utility service walkthroughs
- [Contributing Guide](docs/CONTRIBUTING.md) -- Development setup, coding standards, commit conventions, and the PR process

## Architecture

The application runs as an HTTP server with an embedded React frontend. Each
node talks only to its own Canton participant, over the Admin API and the
Ledger API. No node ever opens a connection to another node. Canton holds the
coordination in two places:

- **Synchronizer topology store**: the proposer writes a topology change as a
  partially signed proposal. Every member co-signs it by transaction hash.
  Canton merges the signatures by fingerprint, and the change becomes
  effective.
- **Daml contracts**: the `decman-coordination-v1` package holds the workflow
  intent. A run starts as a `WorkflowProposal` whose invitees are observers.
  The invitees accept or decline it on the ledger.

Two roles exist per run:

- **Coordinator (proposer)**: creates the proposal, writes the topology
  change, and executes the prepared submission.
- **Member (invitee)**: accepts the proposal, validates each change against
  it, and co-signs the change.

```
┌──────────────────┐   ┌──────────────────┐   ┌──────────────────┐
│  decman node 1   │   │  decman node 2   │   │  decman node 3   │
│    HTTP :8081    │   │    HTTP :8082    │   │    HTTP :8083    │
└────────┬─────────┘   └────────┬─────────┘   └────────┬─────────┘
         │ Admin +              │ Admin +              │ Admin +
         │ Ledger API           │ Ledger API           │ Ledger API
┌────────▼─────────┐   ┌────────▼─────────┐   ┌────────▼─────────┐
│  participant 1   │   │  participant 2   │   │  participant 3   │
└────────┬─────────┘   └────────┬─────────┘   └────────┬─────────┘
         │                      │                      │
         └──────────────────────┼──────────────────────┘
                                │
                ┌───────────────▼────────────────┐
                │     Canton synchronizer        │
                │  topology store + Daml         │
                │  transactions                  │
                └────────────────────────────────┘
```

## Quick Start

### Prerequisites

- Rust toolchain (for building from source)
- Access to Canton participant nodes (Admin API and Ledger API)
- Docker (optional, for containerized deployment). Building the image also
  requires an SSH key registered on a GitHub account — see
  [Running with Docker](#running-with-docker)

### Running Locally

```bash
# Build and run with env vars
DECPM_DIR=./development/participant-1 \
DECPM_PORT=8081 \
DECPM_CANTON_ADMIN_HOST=localhost \
DECPM_CANTON_ADMIN_PORT=5002 \
DECPM_CANTON_LEDGER_HOST=localhost \
DECPM_CANTON_LEDGER_PORT=5001 \
cargo run -p decman -- serve

# Or with a .env file in the data directory
cargo run -p decman -- -d ./development/participant-1 serve

# Or with release build
cargo build --release -p decman
DECPM_PORT=8081 ./target/release/dec-party-manager -d ./development/participant-1 serve
```

Open http://localhost:8081 in your browser.

### Setting the node identity

A fresh node coordinates nothing until it has a **node party**. The node party
is a normal Canton party. This node's participant hosts it with Submission
permission. The node signs every coordination contract with it: the registry
entry, the proposals, the acceptances, the submission signatures, and the ACS
manifests.

1. Allocate the node party on your participant with Submission permission.
2. Grant the Ledger API user `CanActAs` and `CanReadAs` on that party.
3. Save the identity:

```bash
curl -X PUT http://localhost:8081/node-identity \
  -H "Content-Type: application/json" \
  -d '{
    "node_party_id": "node1::1220abc...",
    "user_id": "ledger-api-user",
    "keycloak_url": "https://keycloak.example.com",
    "keycloak_realm": "my-realm",
    "keycloak_client_id": "my-client",
    "keycloak_client_secret": "secret-value"
  }'

# Read it back, with the hosting permission this participant reports
curl http://localhost:8081/node-identity
```

The handler reads the head `PartyToParticipant` before it saves anything, and
refuses a party this participant does not host with Submission permission.
`PUT /node-identity` needs the admin role. It is exempt from authentication
only while `party_credentials` is entirely empty, which is the state of a fresh
node.

The node stores the identity as a `party_credentials` row with `kind = 'node'`.
That row mints outbound Canton tokens only. The JWT validator leaves it out of
the trusted issuers, so a node identity never widens who may log in.

### Running with Docker

> **An SSH key is required to build the image.** The build compiles the
> `canton-lib` Rust dependency, which Cargo fetches from GitHub over SSH, so
> BuildKit needs an SSH key forwarded into the build via `--ssh`. `canton-lib`
> is a **public** repository, so any SSH key registered on any GitHub account
> works — no special repository access is required. Point `--ssh default=` at
> your private key file, or pass just `--ssh default` to forward your running
> `ssh-agent`.

```bash
# Build the image (forward an SSH key registered on a GitHub account;
# replace the key path with your own)
docker build --ssh default=$HOME/.ssh/id_ed25519 -f development/Dockerfile -t dec-party-manager .

# Run a single instance
docker run -p 8080:8080 -v ./data:/data \
  -e DECPM_CANTON_ADMIN_HOST=canton-node \
  -e DECPM_CANTON_ADMIN_PORT=5002 \
  -e DECPM_CANTON_LEDGER_HOST=canton-node \
  -e DECPM_CANTON_LEDGER_PORT=5001 \
  -e DECPM_CANTON_SYNCHRONIZER=global \
  -e DECPM_CANTON_NETWORK=devnet \
  dec-party-manager
```

Published releases ship that same binary as two images, `…:<tag>` and
`…:<tag>-nonroot`; the second runs as uid 65532 and defaults `DECPM_DIR` to
`/home/nonroot`, so its mount goes at `/home/nonroot/data` and the host
directory has to belong to that uid. The `chown` is recursive because a `./data`
left behind by the root image holds root-owned files — the SQLite database, the
DAR directory, and the ACS spool — that uid 65532 could otherwise not open:

```bash
mkdir -p ./data && sudo chown -R 65532:65532 ./data
docker run -p 8080:8080 -v ./data:/home/nonroot/data \
  ... public.ecr.aws/dlc-link/decentralization-manager:<tag>-nonroot
```

### Running Multiple Participants (Development)

The Compose services build from `development/Dockerfile` and forward your
`ssh-agent` (`ssh: default`), so add your GitHub-registered SSH key to the
agent before bringing them up:

```bash
ssh-add ~/.ssh/id_ed25519   # your GitHub-registered key
cd development
docker compose up
```

This starts three participant instances on ports 8081, 8082, and 8083.

The compose stack uses bridge networking and reaches Canton through `host.docker.internal`, so it runs the same on Docker Desktop, OrbStack, and Linux. Each participant expects its Canton Ledger/Admin APIs reachable on the host (the standard layout forwards them to `localhost:5001/5002`, `5011/5012`, `5021/5022` — e.g. via `just port-forward`). See the header of [`development/docker-compose.yml`](development/docker-compose.yml) for the full prerequisites.

## Configuration

All node configuration is done via environment variables (prefixed `DECPM_*`) or CLI arguments. The `--dir` (`-d`) flag points to a directory for persistent data. If a `.env` file exists in that directory, it is loaded automatically before parsing CLI arguments.

### Directory Structure

```
participant-dir/
├── .env               # Optional environment file (loaded automatically)
└── data/
    ├── decpm.db       # SQLite database (peers, credentials, workflow runs)
    ├── dars/          # DAR files for contract deployment
    └── acs/           # ACS snapshots spooled for an add-party handoff
```

The database file path can be overridden with the `--db` CLI flag. The spool
directory can be moved with `DECPM_ACS_SPOOL_DIR`.

### Ports

| Port | Purpose | Variable |
|------|---------|----------|
| 8080 | HTTP API and web UI | `DECPM_PORT` |
| 9464 | Prometheus metrics at `/metrics`, on its own listener | `DECPM_METRICS_PORT` (`0` disables it) |

A node opens no other listener. No peer connects to it, so neither port has to
be reachable from another operator's network. The node dials out to its own
Canton participant and to its identity provider.

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `DECPM_DIR` | Root directory for persistent data (`--dir`/`-d`) | `.` |
| `DECPM_HOST` | Host address to bind the HTTP/UI server to | `0.0.0.0` |
| `DECPM_PORT` | Port for the HTTP/UI server | `8080` |
| `DECPM_METRICS_PORT` | Port serving Prometheus metrics at `/metrics`, separate from the HTTP/UI port (`0` disables it) | `9464` |
| `DECPM_DB_PATH` | SQLite database path override (CLI flag `--db`) | _(defaults to `{dir}/data/decpm.db`)_ |
| `DECPM_DB_ENCRYPTION_KEY` | Encryption key for secrets stored in the database | _(none)_ |
| `DECPM_ADMIN_ROLE` | Role name that gates sensitive endpoints (unset skips the role check) | _(none)_ |
| `DECPM_ALLOWED_ORIGIN` | Origin permitted by CORS (e.g. `https://decman.example.com`) | _(none, same-origin only)_ |
| `DECPM_LOG_FORMAT` | Log format. `text` gives the readable console format for local work; any other value gives one JSON object per line | `json` |
| `DECPM_JWT_ROLE_CLAIM` | JWT claim holding an array of role names, for a provider that needs a namespaced custom claim | _(standard role carriers)_ |
| `DECPM_TENANT_API_KEYS` | Comma-separated bearer keys for the wallet-facing `/v0/tenant/*` endpoints. Empty disables the tenant API outside insecure mode | _(none)_ |
| `DECPM_CANTON_ADMIN_HOST` | Canton Admin API host | `127.0.0.1` |
| `DECPM_CANTON_ADMIN_PORT` | Canton Admin API port | `5002` |
| `DECPM_CANTON_LEDGER_HOST` | Canton Ledger API host | `127.0.0.1` |
| `DECPM_CANTON_LEDGER_PORT` | Canton Ledger API port | `5001` |
| `DECPM_CANTON_SYNCHRONIZER` | Canton synchronizer name | `global` |
| `DECPM_CANTON_NETWORK` | Canton network environment (`devnet`, `testnet`, `mainnet`) | `devnet` |
| `DECPM_CANTON_ADMIN_TLS` | Speak TLS to the Canton Admin API (see [TLS to the participant](#tls-to-the-participant)) | `false` |
| `DECPM_CANTON_ADMIN_TLS_CA_CERT` | PEM of the CA that issued the Admin API certificate. Needed when that CA is private | _(platform trust store)_ |
| `DECPM_CANTON_ADMIN_TLS_CLIENT_CERT` | PEM client certificate, for an Admin API that requires mTLS | _(none)_ |
| `DECPM_CANTON_ADMIN_TLS_CLIENT_KEY` | PEM client key matching `DECPM_CANTON_ADMIN_TLS_CLIENT_CERT` | _(none)_ |
| `DECPM_CANTON_ADMIN_TLS_DOMAIN` | Name to validate the Admin API certificate against, when it differs from the host being dialed | _(the configured host)_ |
| `DECPM_CANTON_LEDGER_TLS` | Speak TLS to the Canton Ledger API | `false` |
| `DECPM_CANTON_LEDGER_TLS_CA_CERT` | Ledger API equivalent of `DECPM_CANTON_ADMIN_TLS_CA_CERT` | _(platform trust store)_ |
| `DECPM_CANTON_LEDGER_TLS_CLIENT_CERT` | Ledger API equivalent of `DECPM_CANTON_ADMIN_TLS_CLIENT_CERT` | _(none)_ |
| `DECPM_CANTON_LEDGER_TLS_CLIENT_KEY` | Ledger API equivalent of `DECPM_CANTON_ADMIN_TLS_CLIENT_KEY` | _(none)_ |
| `DECPM_CANTON_LEDGER_TLS_DOMAIN` | Ledger API equivalent of `DECPM_CANTON_ADMIN_TLS_DOMAIN` | _(the configured host)_ |
| `DECPM_KEYCLOAK_URL` | Keycloak server URL for frontend auth | _(none)_ |
| `DECPM_KEYCLOAK_REALM` | Keycloak realm name for frontend auth | _(none)_ |
| `DECPM_KEYCLOAK_CLIENT_ID` | Keycloak client ID for frontend auth | _(none)_ |
| `DECPM_KEYCLOAK_INTERNAL_URL` | Internal/backchannel Keycloak URL the server uses for OIDC discovery, JWKS, and introspection when it cannot reach `DECPM_KEYCLOAK_URL` directly (e.g. that is a tailnet host but the pod is in-cluster) | `DECPM_KEYCLOAK_URL` |
| `DECPM_AUTH0_DOMAIN` | Auth0 tenant domain for frontend auth (mutually exclusive with `DECPM_KEYCLOAK_*`) | _(none)_ |
| `DECPM_AUTH0_CLIENT_ID` | Auth0 SPA client ID for frontend auth | _(none)_ |
| `DECPM_AUTH0_AUDIENCE` | Auth0 API audience the SPA's access tokens target | _(none)_ |
| `DECPM_AUTH0_SCOPE` | Extra space-separated scopes the SPA requests. Auth0 RBAC returns a permission in `scope` only when the client asks for it | _(none)_ |
| `DECPM_INSECURE` | Run without an IdP: accept any inbound token and present an unsafe HS256 token to Canton (CLI flag `--insecure`). **Never use in production.** See [Insecure mode](#insecure-mode-local-development-without-an-idp) | `false` |
| `DECPM_CANTON_HMAC_SECRET` | HS256 secret decman signs the unsafe Canton token with, in insecure mode. Must match Canton's unsafe auth-service secret | `unsafe` |
| `DECPM_CANTON_HMAC_AUDIENCE` | `aud` claim for the unsafe Canton token, in insecure mode. Must match Canton's `target-audience` | `https://canton.network.global` |
| `DECPM_CANTON_HMAC_SUBJECT` | `sub` claim / ledger user for the unsafe Canton token, in insecure mode | `ledger-api-user` |
| `DECPM_OBSERVER_POLL_SECS` | Seconds between observer ticks. A tick reads the ledger, co-signs pending topology proposals, and drives every run. Mainnet guidance is `10` | `3` |
| `DECPM_HEARTBEAT_INTERVAL_SECS` | Seconds between heartbeats on this node's `DecmanNode` registry contract | `3600` |
| `DECPM_HEARTBEAT_MIN_INTERVAL_SECS` | Floor the `DecmanNode` template enforces between two heartbeats. Clamped to `[1, DECPM_HEARTBEAT_INTERVAL_SECS]` | `60` |
| `DECPM_PEER_STALE_FACTOR` | A peer reads as `Stale` once its last heartbeat is older than this many of its own heartbeat intervals | `3` |
| `DECPM_PROPOSAL_TTL_SECS` | Lifetime of a `WorkflowProposal`. An invitee cannot accept it after it expires | `604800` |
| `DECPM_AUTO_UPLOAD_COORDINATION_DAR` | Upload and vet the embedded coordination DAR in a startup task. `/node-health` reports the result | `true` |
| `DECPM_ACS_SPOOL_DIR` | Directory that holds exported ACS snapshots for an add-party handoff | _(defaults to `{dir}/data/acs`)_ |
| `DECPM_COORDINATION_PACKAGE_REF` | Package reference the coordination templates resolve against | `#decman-coordination-v1` |
| `DECPM_TOPOLOGY_RETRY_MAX_ATTEMPTS` | Attempts a topology propagation check makes before it gives up | `30` |
| `DECPM_TOPOLOGY_RETRY_DELAY_SECS` | Delay between two topology propagation attempts, in seconds | `2` |
| `DECPM_TOPOLOGY_PROPAGATION_DELAY_SECS` | Wait after a topology change becomes effective, in seconds, so the sequencer's topology state settles | `30` |
| `DECPM_REWARD_AUTOMATION_INTERVAL_SECS` | How often the CIP-104 reward automation sweeps each decparty for unassigned coupons, in seconds. Enablement is on-ledger, so this sets cadence only | `300` |
| `DECPM_REWARD_EXPIRY_READ_INTERVAL_SECS` | How often the automation re-reads the backlog purely to refresh `decman_reward_oldest_unassigned_expires_in_seconds`, in seconds, when no sweep is due. Both expiry alert rules read that gauge, so this bounds how stale their input can get. A sweep reads the ledger too, so the gauge refreshes at whichever interval is shorter | `3600` |
| `DECPM_REWARD_MAX_CREATES` | Output contracts one `Delegation_Assign` may create, which bounds the coupons per transaction. Lower it if assigns start failing | `100` |
| `DECPM_REWARD_MIN_EXPIRY_MARGIN_SECS` | Time a coupon must have left before expiry to be assigned, in seconds. Guards against a coupon expiring mid-submission | `120` |

All environment variables can also be passed as CLI arguments (e.g., `--canton-admin-host`).

### TLS to the participant

DecMan talks to the participant over two gRPC channels, the Admin API and the
Ledger API, and each is configured independently. Both default to plaintext
h2c, which is the right setting when the participant is only reachable over a
trusted private network — loopback, a Docker network, or a pod.

Turn TLS on per endpoint when the participant serves it:

```bash
# Participant behind a private CA (the usual case).
DECPM_CANTON_ADMIN_TLS=true
DECPM_CANTON_ADMIN_TLS_CA_CERT=/etc/decman/tls/canton-ca.pem
DECPM_CANTON_LEDGER_TLS=true
DECPM_CANTON_LEDGER_TLS_CA_CERT=/etc/decman/tls/canton-ca.pem
```

- **Publicly-issued certificate**: set only the `_TLS` flag. With no CA file
  the platform trust store is used.
- **mTLS**: add `_TLS_CLIENT_CERT` and `_TLS_CLIENT_KEY`. Setting one without
  the other is rejected at startup rather than silently ignored.
- **Certificate issued to a name you are not dialing** — e.g. a cert for a
  Kubernetes service DNS name while DecMan connects by IP: set `_TLS_DOMAIN`
  to the name on the certificate.

A mismatch between these settings and what the endpoint actually speaks
surfaces on the first call, because a plaintext client against a TLS listener
gets its connection closed on the first bytes. The connect error names the
variable to change in either direction, so `transport error` /
`BrokenPipe` on every Canton call is worth reading in full before digging
further.

### Insecure mode (local development without an IdP)

For local development against a Canton configured with **unsafe (shared-secret) auth**, you can run without setting up Keycloak or Auth0 at all. Start the node with `--insecure` (or `DECPM_INSECURE=true`) and it will:

- accept **any** inbound token, so the admin UI needs no login, and
- mint an unsafe **HS256** token for Canton instead of fetching one from an IdP.

> [!WARNING]
> Insecure mode disables authentication entirely. It is for local development only — **never enable `--insecure` / `DECPM_INSECURE` in a shared or production deployment.** The node logs a warning at startup whenever it is on.

The token is signed and stamped from the `DECPM_CANTON_HMAC_*` variables above; point Canton's unsafe auth service at the same secret and audience so it accepts the token. With the defaults (`unsafe` / `https://canton.network.global`), `--insecure` alone is enough:

```bash
dec-party-manager -d ./development/participant-1 serve --insecure
```

This replaces the older `--features test-mode` build — no special build is required; the flag is honored by the standard release binary.

### Example `.env` File

```env
DECPM_PORT=8081
DECPM_CANTON_ADMIN_HOST=localhost
DECPM_CANTON_ADMIN_PORT=5002
DECPM_CANTON_LEDGER_HOST=localhost
DECPM_CANTON_LEDGER_PORT=5001
DECPM_CANTON_SYNCHRONIZER=global
DECPM_CANTON_NETWORK=devnet
```

### Network Peers

A peer is three values: the participant id, the node party, and a display name.
There is no address, no port, and no key, because no node dials another node.

Operators exchange one string per peer out of band:

```
participant_id,node_party_id,name
```

The **Share my identity** button in the UI copies that string. **Add peer**
pastes it. Both operators do this, because each side names the other's node
party as an observer of its own coordination contracts.

Peers are stored in the SQLite database and managed via the `/network-config`
API endpoint:

```bash
# Configure peers
curl -X POST http://localhost:8081/network-config \
  -H "Content-Type: application/json" \
  -d '[
    {
      "participant_id": "participant1::1220abc...",
      "name": "Participant 1",
      "party": "node1::1220abc..."
    },
    {
      "participant_id": "participant2::1220def...",
      "name": "Participant 2",
      "party": "node2::1220def..."
    }
  ]'

# Retrieve current peers
curl http://localhost:8081/network-config
```

- `participant_id`: Canton participant UID (e.g., `participant::1220...`)
- `name`: Display name
- `party`: the peer's node party. A peer without one cannot be invited to a
  workflow.

Before this node names a peer's node party as an observer, it reads the head
`PartyToParticipant` of that party. It refuses the peer unless the claimed
participant hosts the party with Submission permission.

`GET /registry` shows what the peers publish. It groups the `DecmanNode`
entries three ways: this node's own entry, the entries of configured peers, and
inbound entries. An inbound entry has a signatory that is not a configured peer
yet.

### Party Credentials

Per-party credentials (outbound OAuth for Canton, package IDs) are stored in the SQLite database and managed via the `/party-config` API endpoint. Either the Keycloak fields or the Auth0 fields are supplied — whichever matches the node's top-level provider:

```bash
# Keycloak (client_credentials)
curl -X PUT http://localhost:8081/party-config \
  -H "Content-Type: application/json" \
  -d '{
    "dec_party_id": "decparty::1220abc...",
    "member_party_id": "participant1::1220abc...",
    "user_id": "CoordinatorUser",
    "keycloak_url": "https://keycloak.example.com",
    "keycloak_realm": "my-realm",
    "keycloak_client_id": "my-client",
    "keycloak_client_secret": "secret-value"
  }'

# Auth0 M2M (client_credentials)
curl -X PUT http://localhost:8081/party-config \
  -H "Content-Type: application/json" \
  -d '{
    "dec_party_id": "decparty::1220abc...",
    "member_party_id": "participant1::1220abc...",
    "user_id": "CoordinatorUser",
    "auth0_domain": "tenant.us.auth0.com",
    "auth0_audience": "https://your-canton-api",
    "auth0_client_id": "m2m-client-id",
    "auth0_client_secret": "m2m-client-secret"
  }'

# Retrieve party config (secrets masked)
curl http://localhost:8081/party-config/decparty::1220abc...
```

## Workflows

Every workflow follows the same shape. The proposer creates a
`WorkflowProposal` with the invitees as observers. Each invitee accepts or
declines it on the ledger. The proposer then writes the topology change into
the synchronizer store as a partially signed proposal, and every member
co-signs it by transaction hash.

The proposer checks every invitee before it starts a run. It answers HTTP 409
when an invitee has not vetted the coordination package, has no visible
registry entry, or reports an older coordination version. The error names the
participants that still have to upgrade.

How many acceptances a run needs:

- Onboarding, add-party and DAR runs need every invitee.
- Kick and change-threshold need `max(previous threshold, new threshold)` owner
  signatures, and the proposer counts as one of them.

### Creating a Decentralized Party (Onboarding)

1. Set each node's identity, and add every peer on every node
2. Start all participant servers
3. On the coordinator's UI, click **Create Party**, enter a party ID prefix, and
   select the peers to invite
4. Optionally adjust the **Threshold** — how many of the party's owners must sign
   topology changes. It defaults to a majority (`ceil(owners / 2)`, shown and
   editable in the dialog)
5. Each invitee accepts the proposal from its notifications view
6. Every member generates one dual-usage key and publishes its root namespace
   delegation. The proposer then writes the decentralized namespace change,
   and the members co-sign it. The proposer writes the party mapping last.

### Deploying Contracts

1. From a party card in the UI, click **Deploy Contracts**
2. Upload DAR files via the file picker
3. Configure contract definitions (operator party, templates, fields)
4. Each invitee accepts, then uploads the same pinned DAR files locally
5. The proposer prepares each transaction and opens a `SubmissionRound`. Each
   member verifies the hash and signs. The proposer executes once the party's
   signing threshold is met.

### Distributing DARs

1. From the packages panel, click **Distribute DARs** and pick the files
2. The proposer pins every file by its sha256 and its main package id, and
   uploads it to its own participant
3. Each invitee accepts and uploads the same files through `POST /dars/upload`
   with `pin_instance` set to the run id
4. The run completes when the topology store shows every pinned package vetted
   on every participant. No DAR bytes travel between nodes.

### Removing a Participant (Kick)

1. From a party card, click **Kick Participant**
2. Select the participant to remove
3. The proposer writes the namespace change without that owner, and then the
   party mapping without that host. The remaining members co-sign both. The
   kicked node sees the change as an unsolicited proposal.

### Adding a Participant (Add Party)

1. From a party card, click **Add member** and choose the participant to add
2. The joining node generates its key and publishes its root namespace
   delegation
3. Every member co-signs the namespace and party changes, which mark the
   joining participant `Onboarding`
4. Each current host exports an ACS snapshot and publishes an `AcsManifest`.
   The joining operator downloads the snapshot with `GET /acs-export` and
   uploads it with `POST /acs-import`. An empty snapshot skips the transfer.

### Changing the Threshold (Change Threshold)

1. From a party card, click **Change Threshold** and enter the new value
2. The proposer re-issues the namespace and party mappings with the new
   threshold and the same members
3. A quorum of the current owners co-signs both mappings

## API Endpoints

The table below is a curated subset. A complete, interactive API reference is available via the **Swagger UI at `/swagger-ui/`** (OpenAPI document at `/api-docs/openapi.json`) — but note this is only mounted when the node runs in [insecure mode](#insecure-mode-local-development-without-an-idp) (`--insecure`); a normal (secure) deployment does not expose it.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Serves the React frontend |
| `/auth-config` | GET | Returns frontend auth configuration (Keycloak or Auth0) |
| `/node-config` | GET | Returns node configuration |
| `/network-info` | GET | Returns network info (DSO party, AmuletRules contract) |
| `/operator-info` | GET | Returns DA Utility operator info |
| `/network-config` | GET | Returns network peer list (from SQLite) |
| `/network-config` | POST | Updates network peer list (saved to SQLite) |
| `/party-config/{dec_party_id}` | GET | Returns party credentials (secrets masked) |
| `/party-config` | PUT | Saves or updates party credentials (to SQLite) |
| `/node-identity` | GET | Returns this node's node party and how the participant hosts it (admin role) |
| `/node-identity` | PUT | Sets or replaces the node identity (admin role) |
| `/registry` | GET | Returns the `DecmanNode` entries this node sees: its own, its peers', and inbound (admin role) |
| `/decentralized-parties` | GET | Lists decentralized parties (filtered by `prefix` query param) |
| `/participants-status` | GET | Returns each peer's registry status and heartbeat age |
| `/node-health` | GET | Returns node and participant health, including the startup coordination-DAR upload |
| `/proposals/unsolicited` | GET | Returns pending topology proposals that no accepted workflow explains |
| `/acs-manifests/{party}` | GET | Returns the `AcsManifest` records published for a party (admin role) |
| `/acs-export/{party}/{target}` | GET | Streams the party's ACS snapshot for a joining participant (admin role) |
| `/acs-import/{party}` | POST | Imports a relayed ACS snapshot and clears the onboarding flag (admin role) |
| `/onboarding` | POST | Starts onboarding workflow |
| `/onboarding/status` | GET | Returns onboarding progress |
| `/contracts` | POST | Starts contracts workflow |
| `/contracts/status` | GET | Returns contracts progress |
| `/kick` | POST | Starts kick workflow |
| `/kick/status` | GET | Returns kick progress |
| `/add-party` | POST | Starts add-party workflow |
| `/add-party/status` | GET | Returns add-party progress |
| `/change-threshold` | POST | Starts change-threshold workflow |
| `/change-threshold/status` | GET | Returns change-threshold progress |
| `/workflows` | GET | Lists workflow instances and their lifecycle state |
| `/workflows/{instance_name}/dismiss` | POST | Dismisses a workflow instance |
| `/workflows/{instance_name}/retry` | POST | Retries a failed workflow instance |
| `/onboarding/cancel` | POST | Cancels the onboarding workflow |
| `/contracts/cancel` | POST | Cancels the contracts workflow |
| `/kick/cancel` | POST | Cancels the kick workflow |
| `/add-party/cancel` | POST | Cancels the add-party workflow |
| `/change-threshold/cancel` | POST | Cancels the change-threshold workflow |
| `/dars/cancel` | POST | Cancels the DARs distribution workflow |
| `/invitations` | GET | Returns pending workflow invitations |
| `/invitations/accept` | POST | Accepts a pending invitation |
| `/invitations/decline` | POST | Declines a pending invitation |
| `/auth/status` | GET | Returns authentication status for configured parties |
| `/auth/test` | POST | Tests outbound IdP authentication (Keycloak or Auth0, per party) |
| `/governance/confirmations` | GET | Returns governance confirmations grouped by action |
| `/governance/state` | GET | Returns governance state (GovernanceRules) |
| `/governance/confirm` | POST | Submits a governance confirmation |
| `/governance/execute` | POST | Executes a confirmed governance action |
| `/governance/expire` | POST | Expires a stale governance confirmation |
| `/governance/cancel` | POST | Cancels a governance confirmation |
| `/services/provider` | GET | Returns ProviderService contracts |
| `/services/user` | GET | Returns UserService contracts |
| `/services/registrar` | GET | Returns RegistrarService contracts |
| `/contracts/query` | GET | Queries active contracts by template |
| `/packages` | GET | Returns configured package IDs for a party |
| `/token-standard-contracts` | POST | Queries token standard contracts |
| `/dars/upload` | POST | Uploads DARs to this node. With `pin_instance` the files must match that run's pins |
| `/dars/distribute` | POST | Starts a DARs run: pins each file and invites the peers to upload it |
| `/dars/distribute/status` | GET | Returns DARs distribution workflow progress |
| `/packages/vetted` | GET | Returns packages uploaded on this node |
| `/packages/compare-peers` | GET | Compares vetted packages per participant, read from the topology store |
| `/external-parties` | GET | Lists the external (co-validated) parties this node hosts |
| `/v0/tenant/prepare` | POST | Wallet-facing: builds an external party's onboarding topology and returns the hash to sign |
| `/v0/tenant/onboard` | POST | Wallet-facing: validates the wallet's signed topology, co-signs, and submits it |
| `/v0/tenant/{party}/status` | GET | Wallet-facing: reports whether this host has the party hosted yet |
| `/v0/tenant/{party}/state` | GET | Wallet-facing: the party's current serial, threshold and host count, so a caller can pin `base_serial` on the next write |
| `/v0/tenant/add-hosts/prepare` | POST | Wallet-facing: builds the serial-N+1 topology that adds hosts to an existing external party |
| `/v0/tenant/add-hosts/onboard` | POST | Wallet-facing: validates the wallet-signed add-hosts topology, co-signs, and submits it |
| `/v0/tenant/{party}/acs/{target}` | GET | Wallet-facing: serves one block of the party's ACS scoped to a joining host (`?seq=`), for the wallet to relay. The host keeps the Canton export stream open between blocks |
| `/v0/tenant/add-hosts/import` | POST | Wallet-facing: feeds one relayed block straight into this host's open Canton import, clearing the onboarding marker when the final block lands |
| `/v0/tenant/threshold/prepare` | POST | Wallet-facing: builds a confirmation-threshold change |
| `/v0/tenant/threshold/onboard` | POST | Wallet-facing: submits the wallet-signed threshold change |
| `/v0/tenant/local-party/adopt-key/prepare` | POST | Wallet-facing: builds the conversion that gives a local party an owner-held signing key |
| `/v0/tenant/local-party/adopt-key/onboard` | POST | Wallet-facing: co-signs and submits the owner-signed conversion |

The `/v0/tenant/*` endpoints are the tenant API. They authenticate with a
separate tenant API key rather than the operator JWT, and are driven by
[`decman-wallet`](crates/decman-wallet/README.md).

## Development

This repository is a Cargo workspace with four crates under `crates/`:

- **`decman`** — the server (HTTP API, Canton gRPC, on-ledger coordination,
  workflows) and the embedded React frontend. Its binary is
  `dec-party-manager`. The coordination module is documented in
  [crates/decman/src/onledger/README.md](crates/decman/src/onledger/README.md).
- **`common`** — shared wire DTOs, the Canton-ID helpers, and the external-party
  fingerprint derivation, consumed by the other crates. Kept dependency-light;
  OpenAPI (`utoipa`) schema derives are behind its `openapi` feature.
- **`decman-cli`** — a terminal UI client for the server.
- **`decman-wallet`** — the client side of the tenant API (`/v0/tenant/*`): the
  library a wallet provider embeds to run a co-validated party from a key it holds
  itself, and to transact as it. See
  [crates/decman-wallet/README.md](crates/decman-wallet/README.md).

Workspace-wide `cargo` commands build all four; pass `-p decman` to act on
just the server (e.g. `cargo run -p decman -- serve`).

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run unit tests — includes the integration-test binary's helpers
# (Fixture, Scenario DSL); the end-to-end test itself is gated by
# `#[ignore]` and is invoked separately via run.sh below.
cargo test

# Run linter
cargo clippy --all-targets --all-features -- -D warnings

# Format code
cargo fmt
```

### Integration tests

The full integration test boots a Splice localnet (Docker), spawns 3
`dec-party-manager` instances, configures peers, and runs an end-to-end
governance workflow exercising onboarding, DAR distribution, governance
contract deployment, the token-custody / utility-onboarding / generic-vote
plugins, and the kick workflow.

```bash
# Quiet mode (default) — focused Given-When-Then trace
./integration-tests/run.sh

# Verbose mode — full INFO from dec-party-manager and the test crate
./integration-tests/run.sh --verbose

# Custom RUST_LOG (overrides both presets)
RUST_LOG=debug ./integration-tests/run.sh

# Help
./integration-tests/run.sh --help
```

#### Quiet mode (default)

Quiet mode is the recommended way to run the suite. It prints only what a
tester needs to verify a passing run, and it suppresses the dec-party-manager
INFO chatter and the Canton convergence warnings.

The suite is organised into two layers:

- **Phases** — top-level workflow chunks, one file per phase in
  [`crates/decman/tests/common/phases/`](crates/decman/tests/common/phases/).
  [`crates/decman/tests/governance_workflows.rs`](crates/decman/tests/governance_workflows.rs)
  runs them in order, and each is logged as `INFO Phase: <name>`. The set
  covers the governance arc (`create_dec_party`, `distribute_dars`,
  `deploy_gov_core`, `token_custody`, `utility_onboarding`, `generic_vote`,
  `kick`), the add-party and external-party flows, and the chaos phases
  (restart / resume, cancel cascades, concurrent workflows).
- **Scenarios** — Given-When-Then story arcs built with the
  [`Scenario`](crates/decman/tests/common/scenario.rs) DSL. Each scenario has
  its own header, indented step trace, and completion line. A phase runs one
  or more scenarios: most run a single one, while `utility_onboarding` runs
  eight (four propose-confirm-execute cycles — ProvisionProviderService,
  SetupUtility, Mint, Burn — plus four side-effect assertion scenarios).

A scenario may omit `Given` and/or `When` and contain only `Then`s.
That happens when the action has already been taken by an earlier
scenario in the same phase, and this scenario only needs to observe its
after-state — the four "side-effect assertion scenarios" in
`utility_onboarding` (`ProviderService visible`, `SetupUtility side
effects`, `Mint side effects`, `Burn side effects`) follow exactly this
pattern. The runner does **not** carry steps between scenarios; cross-
scenario state flows through the **`Fixture`**, which `Scenario::run`
borrows as `&mut Fixture`. An action-side scenario mutates the SUT and
records captured ids on the fixture (`f.provider_service_cid`,
`f.allocation_factory_cid`, etc.); a follow-up observation-side
scenario reads them back via `f.get_json(...)` and stores anything new
it captures on the same fixture for later scenarios to use.

Sample of a passing run:

```
==========================================
Running governance workflow e2e (Rust)
==========================================
running 1 test

INFO Phase: create_dec_party
INFO Using prefix: test-network-1
INFO   Scenario "create decentralized party test-network-1"
INFO     GIVEN no party at this prefix yet
INFO     WHEN  P1 posts /onboarding
INFO     THEN  Onboarding invitation visible on P2
INFO       ✓ (took 2.1s)
INFO     THEN  Onboarding invitation visible on P3
INFO       ✓ (took 0.0s)
INFO     WHEN  P2 + P3 accept Onboarding invitations
INFO     THEN  onboarding workflow reaches completed
INFO       ✓ (took 8.4s)
INFO     THEN  party visible in /decentralized-parties
INFO       ✓ (took 1.9s)
INFO   Scenario "create decentralized party test-network-1" complete (18.7s)

INFO Phase: distribute_dars
INFO   Scenario "distribute DARs"
INFO     GIVEN 3 DAR files on disk
INFO     WHEN  P1 uploads and distributes DARs
INFO     THEN  Dars invitation visible on P2
INFO       ✓ (took 1.4s)
INFO     THEN  Dars invitation visible on P3
INFO       ✓ (took 0.0s)
INFO     WHEN  P2 + P3 accept Dars invitations
INFO     THEN  dars/distribute workflow reaches completed
INFO       ✓ (took 5.6s)
INFO   Scenario "distribute DARs" complete (11.4s)

... (14 scenarios total) ...

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;
==========================================
Integration tests completed successfully!
==========================================
```

Each scenario follows the Given-When-Then DSL: `Given` is a precondition,
`When` is the test action, `Then` is the postcondition assertion (its
probe is polled until it observes the expected state or the per-step
deadline elapses). A failure renders as
`ERROR Scenario "<name>" failed at <KIND> "<step>"` with a chained
`anyhow` cause trail pinpointing the failing HTTP call.

The exact `RUST_LOG` quiet preset is:
```
warn,
governance_workflows::common::scenario=info,
governance_workflows::common::phases=info,
governance_workflows::common::chaos=info
```

The trace itself is rendered with a minimal format locally — just the message text, no timestamps, targets, levels, or structured fields. CI runs (auto-detected via the `CI` env var that GitHub Actions sets) get the full structured format with timestamps + structured fields for log archives and JSON parsing. To force the full format locally, set `INTEGRATION_TEST_FULL_LOG=1`.

#### Verbose mode

Use `--verbose` when diagnosing a stuck or failing run. Sets:
```
dec_party_manager=info,governance_workflows=info
```

This prints all dec-party-manager INFO output: the observer ticks, the
proposal and co-signature traffic, and the workflow internals. The cargo test
runner is also INFO, so individual test cases narrate.

#### Expected WARN output during chaos phases

The chaos phases (`restart_coordinator_resume`, `restart_peer_resume`,
`invite_survives_peer_restart`, `restart_with_concurrent_kinds`,
`concurrent_sibling_cancel`, `concurrent_sibling_decline`) kill and restart
nodes on purpose. A killed node stops answering its own participant, so its
observer loop warns until it comes back. Read those lines as part of the
scenario rather than as findings.

Grep the message text to find them; the emitting module is named so the
reference survives the code moving.

| Line | Emitted by | Why it fires |
|---|---|---|
| `observer stage failed` | `onledger::observer` | One stage of a tick could not read or write the ledger while the node was down or still starting. |
| `driving a run failed; retry next tick` | `onledger::observer` | A run's driver hit a transient error. The next tick drives the same run again. |
| `run failed` | `onledger::engine` | The step-failure budget ran out, which is the outcome a "leave the peer offline" phase asserts. |

Each is a real condition the code detects correctly, so none is downgraded. In
production every one of them warrants a WARN, and the code that emits it
cannot know that a test killed the node.

A chaos restart also changes what the HTTP API answers. A restarted node needs
a few observer ticks to republish its registry entry, so `POST /onboarding` can
answer 409 in that window. `chaos::post_onboarding` retries for exactly that
reason.

When triaging a chaos-phase failure the signal is the scenario trace
(`ERROR Scenario "<name>" failed at <KIND> "<step>"`) and its `anyhow` cause
trail, not the WARN cluster around it.

#### Prerequisites

`docker`, `docker compose v2`, `jq`, `curl`, `lsof`. The script
verifies these up front and bails with a clear message if any are
missing or if a previous run leaked a manager process holding one of
the HTTP ports (8081–8083) or metrics ports (9464–9466).

### Integration tests on devnet

The same suite can also run against a real Canton devnet cluster, manually
triggered from a developer laptop. Useful for catching divergences between
localnet's docker-compose Canton and the clustered Canton that production
faces — auth shape, topology propagation, namespace ownership, etc.

```bash
./integration-tests/run.sh --target devnet
./integration-tests/run.sh --target devnet --verbose   # see DecMan INFO trace
```

The bringup is structurally identical to localnet (three bare-process DecMan
instances, same `wait_for_server` and `configure_peers` flow), except:
- No Docker localnet — Canton is the production-shaped cluster in `ieu-devnet`.
- Canton gRPC admin (5002/5012/5022) and ledger (5001/5011/5021) ports are
  tunneled to localhost via `kubectl port-forward` (managed by
  `devnet.env.sh`'s `start_canton_tunnels`).
- DecMan auth uses real Keycloak (the `JwtValidator`), not the insecure-mode
  `MockValidator` localnet uses. The test runner mints its own bearer token
  via password grant; per-party workflows use M2M `client_credentials`.
- Member parties (`P{N}_MEMBER_PARTY_ID`) are pre-provisioned, not allocated
  during the test. CanActAs grants on the freshly-created dec party are
  issued via DecMan's `POST /auth/grant-rights` (Canton's gRPC
  `UserManagementService.GrantUserRights`).

#### Prerequisites

Beyond the localnet prerequisites listed above, you'll need:

1. **AWS SSO authenticated** against the account that owns the `ieu-devnet`
   cluster:
   ```bash
   aws sso login --profile bs-np   # or whichever profile your org uses
   ```
   Refresh before each run if your SSO session is past its TTL — symptoms
   are kubectl probes that hang or return "Token has expired"; the
   `start_canton_tunnels` step prints a clear error in that case.

2. **kubectl configured** with the `ieu-devnet` context:
   ```bash
   aws eks update-kubeconfig --name devnet-cluster --region us-east-1        --profile bs-np
   ```
   The expected context name (`ieu-devnet`) and namespace (`catalyst-canton`)
   are overridable via `KUBE_CONTEXT_DEVNET` / `KUBE_NS_CANTON` env vars.

3. **`kubectl` and `nc` on `$PATH`** (in addition to `jq`/`curl`/`lsof`).
   Docker is **not** required for the devnet path even though the current
   `check_prerequisites` still asks for it — see [#148][i148] /
   [Copilot review #6][cprev6] for the cleanup.

4. **Per-participant `.env` files** populated at
   `development/remote/participant-{1,2,3}/.env`. Templates with the full
   key shape, inline documentation, and sensible defaults for the
   deployment-config keys are checked in alongside as
   `participant-{1,2,3}/.env.example`. Copy and fill:
   ```bash
   for n in 1 2 3; do
     cp development/remote/participant-$n/.env{.example,}
   done
   # then edit each .env with the real Keycloak URL/realm/credentials and
   # the per-participant party IDs + M2M client secrets
   ```
   The real `.env` files are gitignored; only `.env.example` is tracked.
   Keys required by the integration test:
   - **Shared** (identical across all three): `DECPM_KEYCLOAK_URL`
     (with or without `/auth` — both forms tolerated by `token_url`),
     `DECPM_KEYCLOAK_REALM`, `DECPM_KEYCLOAK_CLIENT_ID`,
     `DECPM_KEYCLOAK_USERNAME`, `DECPM_KEYCLOAK_PASSWORD`.
   - **Per-participant** (`P{N}_*`): `MEMBER_PARTY_ID`, `MEMBER_USER_ID`,
     `MEMBER_KEYCLOAK_CLIENT_ID/SECRET` (workflow M2M client),
     `PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID/SECRET` (admin M2M client,
     required by DecMan's `POST /auth/grant-rights`).

The bringup performs a Keycloak password-grant smoke check before spending
time on `cargo build`, so misconfigured credentials fail fast with a
human-readable error.

#### Known issues

- **Canton-side `TOPOLOGY_NO_APPROPRIATE_SIGNING_KEY_IN_STORE` transient**
  fires intermittently on devnet — most reliably during chaos-phase
  restart-resume windows, when the kubectl-tunneled Canton synchronizer
  hasn't fully reconciled a just-restarted participant's signing keys.
  Transparently absorbed by the workflow step-retry budget
  ([`MAX_CONSECUTIVE_STEP_FAILURES`][step-retry] = 6 attempts × 2s = 12s);
  a single devnet IT run typically sees 4–6 such errors across 3 nodes
  and still passes end-to-end. If you raise the chaos phase count or
  see more than ~10 of these per run, consider bumping the const
  further or filing as a Canton-side performance regression.

[step-retry]: https://github.com/DLC-link/dec-party-manager/blob/main/crates/decman/src/consts.rs

[i148]: https://github.com/DLC-link/dec-party-manager/issues/148
[cprev6]: https://github.com/DLC-link/dec-party-manager/pull/142#discussion_r3241693561

### Frontend Development

```bash
cd frontend
npm install
npm run dev     # Development server with hot reload
npm run build   # Production build (output to dist/)
```

The frontend is embedded into the Rust binary at build time via `build.rs`, which
runs the Vite build. The wire types in `frontend/src/types.generated.ts` are
generated separately from the Rust DTOs (in `common` and the `decman` server) by
the `gen-types` binary (ts-rs). That file is gitignored, so on a fresh checkout
run `just gen-types` once before frontend-only work, otherwise the generated
TypeScript imports won't resolve.

## Docker Image

Release images are built and published by CI: pushing a `v<version>` tag (which
must match the crate version in `crates/decman/Cargo.toml`) runs the release
workflow, which builds the binary and pushes two images:
`public.ecr.aws/dlc-link/decentralization-manager:v<version>` and
`…:v<version>-nonroot`. They hold the same binary and differ only in runtime
identity — uid 0 versus uid 65532, with `DECPM_DIR` defaulting to `/` and
`/home/nonroot` respectively. One `Dockerfile` builds both; the nonroot job
overrides `BASE_TAG`, `RUNTIME_UID` and `DECPM_DIR_DEFAULT`. See the
[Deployment Guide](docs/DEPLOYMENT_GUIDE.md) for which to pick. The root
`Dockerfile` is that workflow's runtime wrapper — it copies in the CI-built
binary and does no compilation, so it is not useful for a local build.

To build an image from source locally, use the full-source build instead:

```bash
docker build --ssh default=$HOME/.ssh/id_ed25519 -f development/Dockerfile -t dec-party-manager .
```

## Deployment

The container image built above is self-contained. For a from-scratch
deployment walkthrough — Secret, Deployment + PVC, Service, and Ingress manifests
with all required configuration — see the [Deployment Guide](docs/DEPLOYMENT_GUIDE.md).

## Contributing

Contributions are welcome! See the [Contributing Guide](docs/CONTRIBUTING.md) for
development setup, coding standards, commit conventions, and the pull request
process. Please also review our [Code of Conduct](docs/CODE_OF_CONDUCT.md) and,
for vulnerabilities, our [Security Policy](docs/SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).

## Feedback

Have feedback on the Decentralization Manager? Let us know via our
[feedback form](https://bitsafe.typeform.com/decman-feedback).
