# Canton Decentralized Party Manager

A web application for managing decentralized parties in Canton blockchain networks. Provides a user interface for onboarding new parties, deploying governance contracts, and managing participant membership.

## Features

- **Web-Based Management UI**: React frontend for managing decentralized parties
- **Multi-Party Onboarding**: Coordinated workflow for creating decentralized party namespaces
- **Contract Deployment**: Upload DAR files and deploy governance contracts with multi-party signing
- **Governance Actions**: View and manage governance confirmations with threshold-based execution
- **Participant Management**: View party membership, kick participants with threshold-based voting
- **OAuth Authentication (Keycloak or Auth0)**: Supports M2M (client_credentials) and password flows for Ledger API access; per-node choice of provider for both frontend gating and outbound Canton tokens
- **Secure P2P Communication**: Noise Protocol Framework for encrypted coordinator-peer communication
- **Real-time Status**: Live peer connectivity monitoring and workflow progress tracking
- **Canton Integration**: Native gRPC integration with Canton Admin and Ledger APIs

## Documentation

- [Architecture Overview](docs/ARCHITECTURE.md) -- System architecture, core concepts, communication protocol, and technical constraints
- [User Guide](USER_GUIDE.md) -- Walkthrough of the web UI for day-to-day party and governance operations
- [Custom DAML Templates](docs/CUSTOM_DAML_TEMPLATES.md) -- Authoring and deploying your own DAML governance templates
- [Deployment Guide](docs/DEPLOYMENT_GUIDE.md) -- Deploying a node to Kubernetes from scratch: manifests, identity-provider setup, and configuration reference
- [Use Cases](docs/USE_CASES.md) -- Vault governance, FAR rewards, multi-sig wallet, and utility service walkthroughs
- [Contributing Guide](docs/CONTRIBUTING.md) -- Development setup, coding standards, commit conventions, and the PR process

## Architecture

The application runs as an HTTP server with an embedded React frontend. Multiple instances coordinate via the Noise Protocol:

- **Coordinator**: Initiates workflows and orchestrates multi-party operations
- **Peers**: Respond to coordinator commands, sign proposals, and execute local operations
- **Automatic Key Management**: Noise keypairs are generated automatically on first run

```
┌─────────────────┐     Noise Protocol      ┌─────────────────┐
│  Participant 1  │◄───────────────────────►│  Participant 2  │
│   (Coordinator) │                         │    (Peer)   │
│   HTTP :8081    │                         │   HTTP :8082    │
│   Noise :9001   │                         │   Noise :9002   │
└────────┬────────┘                         └────────┬────────┘
         │                                           │
         │              Canton Network               │
         └───────────────────┬───────────────────────┘
                             │
                    ┌────────▼────────┐
                    │  Canton Nodes   │
                    │  (Admin/Ledger  │
                    │      APIs)      │
                    └─────────────────┘
```

## Quick Start

### Prerequisites

- Rust toolchain (for building from source)
- Access to Canton participant nodes (Admin API and Ledger API)
- Docker (optional, for containerized deployment)

### Running Locally

```bash
# Build and run with env vars
DECPM_DIR=./development/participant-1 \
DECPM_PORT=8081 \
DECPM_CANTON_ADMIN_HOST=localhost \
DECPM_CANTON_ADMIN_PORT=5002 \
DECPM_CANTON_LEDGER_HOST=localhost \
DECPM_CANTON_LEDGER_PORT=5001 \
DECPM_NOISE_PORT=9001 \
cargo run -p decman -- serve

# Or with a .env file in the data directory
cargo run -p decman -- -d ./development/participant-1 serve

# Or with release build
cargo build --release -p decman
DECPM_PORT=8081 ./target/release/dec-party-manager -d ./development/participant-1 serve
```

Open http://localhost:8081 in your browser.

### Running with Docker

```bash
# Build the image
docker build -t dec-party-manager .

# Run a single instance
docker run -p 8080:8080 -v ./data:/data \
  -e DECPM_CANTON_ADMIN_HOST=canton-node \
  -e DECPM_CANTON_ADMIN_PORT=5002 \
  -e DECPM_CANTON_LEDGER_HOST=canton-node \
  -e DECPM_CANTON_LEDGER_PORT=5001 \
  -e DECPM_NOISE_PORT=9001 \
  -e DECPM_CANTON_SYNCHRONIZER=global \
  -e DECPM_CANTON_NETWORK=devnet \
  dec-party-manager
```

### Running Multiple Participants (Development)

```bash
cd development
docker compose up
```

This starts three participant instances on ports 8081, 8082, and 8083.

## Configuration

All node configuration is done via environment variables (prefixed `DECPM_*`) or CLI arguments. The `--dir` (`-d`) flag points to a directory for persistent data. If a `.env` file exists in that directory, it is loaded automatically before parsing CLI arguments.

### Directory Structure

```
participant-dir/
├── .env               # Optional environment file (loaded automatically)
└── data/
    ├── noise.key      # Auto-generated Noise keypair
    ├── decpm.db       # SQLite database (peers, party credentials)
    └── dars/          # DAR files for contract deployment
```

The database file path can be overridden with the `--db` CLI flag.

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `DECPM_DIR` | Root directory for persistent data (`--dir`/`-d`) | `.` |
| `DECPM_HOST` | Host address to bind the HTTP/UI server to | `0.0.0.0` |
| `DECPM_PORT` | Port for the HTTP/UI server | `8080` |
| `DECPM_DB_PATH` | SQLite database path override (CLI flag `--db`) | _(defaults to `{dir}/data/decpm.db`)_ |
| `DECPM_DB_ENCRYPTION_KEY` | Encryption key for secrets stored in the database | _(none)_ |
| `DECPM_ADMIN_ROLE` | Role name that gates sensitive endpoints (unset skips the role check) | _(none)_ |
| `DECPM_ALLOWED_ORIGIN` | Origin permitted by CORS (e.g. `https://dpm.example.com`) | _(none, same-origin only)_ |
| `DECPM_LISTEN_ADDRESS` | Address to listen on for Noise protocol connections | `0.0.0.0` |
| `DECPM_NOISE_PORT` | Port for Noise protocol connections | `9000` |
| `DECPM_PUBLIC_ADDRESS` | Public address that peers use to connect to this node | _(falls back to listen address)_ |
| `DECPM_CANTON_ADMIN_HOST` | Canton Admin API host | `127.0.0.1` |
| `DECPM_CANTON_ADMIN_PORT` | Canton Admin API port | `5002` |
| `DECPM_CANTON_LEDGER_HOST` | Canton Ledger API host | `127.0.0.1` |
| `DECPM_CANTON_LEDGER_PORT` | Canton Ledger API port | `5001` |
| `DECPM_CANTON_SYNCHRONIZER` | Canton synchronizer name | `global` |
| `DECPM_CANTON_NETWORK` | Canton network environment (`devnet`, `testnet`, `mainnet`) | `devnet` |
| `DECPM_KEYCLOAK_URL` | Keycloak server URL for frontend auth | _(none)_ |
| `DECPM_KEYCLOAK_REALM` | Keycloak realm name for frontend auth | _(none)_ |
| `DECPM_KEYCLOAK_CLIENT_ID` | Keycloak client ID for frontend auth | _(none)_ |
| `DECPM_KEYCLOAK_INTERNAL_URL` | Internal/backchannel Keycloak URL the server uses for OIDC discovery, JWKS, and introspection when it cannot reach `DECPM_KEYCLOAK_URL` directly (e.g. that is a tailnet host but the pod is in-cluster) | `DECPM_KEYCLOAK_URL` |
| `DECPM_AUTH0_DOMAIN` | Auth0 tenant domain for frontend auth (mutually exclusive with `DECPM_KEYCLOAK_*`) | _(none)_ |
| `DECPM_AUTH0_CLIENT_ID` | Auth0 SPA client ID for frontend auth | _(none)_ |
| `DECPM_AUTH0_AUDIENCE` | Auth0 API audience the SPA's access tokens target | _(none)_ |
| `DECPM_TIMEOUT_HANDSHAKE` | Noise handshake timeout in seconds | `30` |
| `DECPM_TIMEOUT_MESSAGE` | Noise message timeout in seconds | `120` |
| `DECPM_TIMEOUT_RETRY_ATTEMPTS` | Connection retry attempts | `3` |
| `DECPM_TIMEOUT_RETRY_DELAY` | Connection retry delay in seconds | `5` |
| `DECPM_NOISE_RETRY_TIMEOUT_SEC` | Per-attempt timeout for the bounded peer-Noise retry wrapper, in seconds | `5` |
| `DECPM_NOISE_RETRY_MAX_ATTEMPTS` | Total attempts (initial + retries) for the bounded peer-Noise retry wrapper | `2` |
| `DECPM_NOISE_RETRY_BACKOFF_MS` | Backoff between attempts of the bounded peer-Noise retry wrapper, in milliseconds | `250` |

All environment variables can also be passed as CLI arguments (e.g., `--canton-admin-host`).

### Example `.env` File

```env
DECPM_NOISE_PORT=9001
DECPM_PUBLIC_ADDRESS=10.0.0.1
DECPM_CANTON_ADMIN_HOST=localhost
DECPM_CANTON_ADMIN_PORT=5002
DECPM_CANTON_LEDGER_HOST=localhost
DECPM_CANTON_LEDGER_PORT=5001
DECPM_CANTON_SYNCHRONIZER=global
DECPM_CANTON_NETWORK=devnet
```

### Network Peers

Peers are stored in the SQLite database and managed via the `/network-config` API endpoint:

```bash
# Configure peers
curl -X POST http://localhost:8081/network-config \
  -H "Content-Type: application/json" \
  -d '[
    {
      "participant_id": "participant1::1220abc...",
      "name": "Participant 1",
      "address": "10.0.0.1",
      "port": 9001,
      "public_key": "03ab12cd...",
      "party": null
    },
    {
      "participant_id": "participant2::1220def...",
      "name": "Participant 2",
      "address": "10.0.0.2",
      "port": 9002,
      "public_key": "02ef34ab...",
      "party": null
    }
  ]'

# Retrieve current peers
curl http://localhost:8081/network-config
```

- `participant_id`: Canton participant UID (e.g., `participant::1220...`)
- `name`: Display name
- `address`: Hostname or IP address for Noise connections
- `port`: Noise protocol port
- `public_key`: Hex-encoded secp256k1 public key (auto-populated from `/keys/status` endpoint)
- `party`: Canton party ID (populated after onboarding)

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

### Creating a Decentralized Party (Onboarding)

1. Configure all participant nodes with each other's connection details via the `/network-config` API
2. Start all participant servers
3. On the coordinator's UI, click **Create Party** and enter a party ID prefix
4. The coordinator invites peers and orchestrates:
   - Cryptographic key generation (namespace + DAML signing keys)
   - Topology proposal creation (DNS and P2P mappings)
   - Multi-party signing
   - Proposal submission to Canton

### Deploying Contracts

1. From a party card in the UI, click **Deploy Contracts**
2. Upload DAR files via the file picker
3. Configure contract definitions (operator party, templates, fields)
4. The coordinator orchestrates:
   - DAR distribution and upload to all participants
   - Ledger submission preparation
   - Multi-party signing of submissions
   - Execution on the Canton ledger

### Removing a Participant (Kick)

1. From a party card, click **Kick Participant**
2. Select the participant to remove
3. The coordinator orchestrates:
   - Export current namespace state
   - Create updated topology proposals (reduced threshold, removed P2P mapping)
   - Multi-party signing by remaining members
   - Proposal submission

## API Endpoints

The table below is a curated subset. A complete, interactive API reference is available via the **Swagger UI at `/swagger-ui/`** (OpenAPI document at `/api-docs/openapi.json`) — but note these endpoints are only mounted in development/test builds (`--features test-mode`); the shipped release image does not expose them.

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
| `/decentralized-parties` | GET | Lists decentralized parties (filtered by `prefix` query param) |
| `/participants-status` | GET | Returns peer connectivity status |
| `/keys/status` | GET | Returns Noise keypair status |
| `/onboarding` | POST | Starts onboarding workflow |
| `/onboarding/status` | GET | Returns onboarding progress |
| `/contracts` | POST | Starts contracts workflow |
| `/contracts/status` | GET | Returns contracts progress |
| `/kick` | POST | Starts kick workflow |
| `/kick/status` | GET | Returns kick progress |
| `/workflows` | GET | Lists workflow instances and their lifecycle state |
| `/workflows/{instance_name}/dismiss` | POST | Dismisses a workflow instance |
| `/workflows/{instance_name}/retry` | POST | Retries a failed workflow instance |
| `/onboarding/cancel` | POST | Cancels the onboarding workflow |
| `/contracts/cancel` | POST | Cancels the contracts workflow |
| `/kick/cancel` | POST | Cancels the kick workflow |
| `/dars/cancel` | POST | Cancels the DARs distribution workflow |
| `/invitations` | GET | Returns pending workflow invitations |
| `/invitations/accept` | POST | Accepts a pending invitation |
| `/invitations/decline` | POST | Declines a pending invitation |
| `/auth/status` | GET | Returns authentication status for configured parties |
| `/auth/test` | POST | Tests outbound IdP authentication (Keycloak or Auth0, per party) |
| `/governance/confirmations` | GET | Returns governance confirmations grouped by action |
| `/governance/state` | GET | Returns governance state (VaultGovernanceRules) |
| `/governance/confirm` | POST | Submits a governance confirmation |
| `/governance/execute` | POST | Executes a confirmed governance action |
| `/governance/expire` | POST | Expires a stale governance confirmation |
| `/governance/cancel` | POST | Cancels a governance confirmation |
| `/vaults` | GET | Returns deployed Vault contracts |
| `/services/provider` | GET | Returns ProviderService contracts |
| `/services/user` | GET | Returns UserService contracts |
| `/services/registrar` | GET | Returns RegistrarService contracts |
| `/contracts/query` | GET | Queries active contracts by template |
| `/packages` | GET | Returns configured package IDs for a party |
| `/token-standard-contracts` | POST | Queries token standard contracts |
| `/dars/upload` | POST | Uploads DARs to the current node only |
| `/dars/distribute` | POST | Distributes DARs across all participants |
| `/dars/distribute/status` | GET | Returns DARs distribution workflow progress |
| `/packages/vetted` | GET | Returns packages uploaded on this node |

## Development

This repository is a Cargo workspace with three crates under `crates/`:

- **`decman`** — the server (HTTP API, Noise P2P, Canton gRPC, workflows) and
  the embedded React frontend. Its binary is `dec-party-manager`.
- **`common`** — shared wire DTOs and the Canton-ID helpers, consumed by both
  `decman` and `decman-cli`. Kept dependency-light; OpenAPI (`utoipa`) schema
  derives are behind its `openapi` feature.
- **`decman-cli`** — a terminal UI client for the server.

Workspace-wide `cargo` commands build all three; pass `-p decman` to act on
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

# Verbose mode — full INFO from dec-party-manager + Canton/Noise libs
./integration-tests/run.sh --verbose

# Custom RUST_LOG (overrides both presets)
RUST_LOG=debug ./integration-tests/run.sh

# Help
./integration-tests/run.sh --help
```

#### Quiet mode (default)

Quiet mode is the recommended way to run the suite — it surfaces only
what a tester needs to verify a passing run, suppressing the
dec-party-manager INFO chatter and Canton/Noise convergence warnings.

The suite is organised into two layers:

- **Phases** — top-level workflow chunks, one file in `tests/common/phases/`
  per phase (`create_dec_party`, `distribute_dars`, `deploy_gov_core`,
  `token_custody`, `utility_onboarding`, `generic_vote`, `kick`). Each
  phase corresponds 1:1 to one of the original bash scripts and is logged
  as `INFO Phase: <name>`.
- **Scenarios** — Given-When-Then story arcs built with the
  [`Scenario`](tests/common/scenario.rs) DSL. Each scenario has its own
  header, indented step trace, and completion line. A phase runs **one or
  more scenarios**: six of the seven phases run a single scenario;
  `utility_onboarding` runs eight (four propose-confirm-execute cycles —
  ProvisionProviderService, SetupUtility, Mint, Burn — plus four
  side-effect assertion scenarios), for **14 scenarios total**.

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
warn,hyper_noise::server=error,
governance_workflows::common::scenario=info,
governance_workflows::common::phases=info
```

The trace itself is rendered with a minimal format locally — just the message text, no timestamps, targets, levels, or structured fields. CI runs (auto-detected via the `CI` env var that GitHub Actions sets) get the full structured format with timestamps + structured fields for log archives and JSON parsing. To force the full format locally, set `INTEGRATION_TEST_FULL_LOG=1`.

#### Verbose mode

Use `--verbose` when diagnosing a stuck or failing run. Sets:
```
dec_party_manager=info,tokio_noise=error,hyper_noise=error,
governance_workflows=info
```

Surfaces all dec-party-manager INFO output (peer connections, Noise
handshakes, workflow internals). The cargo test runner is also INFO,
so individual test cases narrate.

#### Prerequisites

`docker`, `docker compose v2`, `jq`, `curl`, `lsof`. The script
verifies these up front and bails with a clear message if any are
missing or if a previous run leaked a manager process holding one of
the HTTP/Noise ports (8081–8083, 9001–9003).

### Integration tests on devnet

The same suite can also run against a real Canton devnet cluster, manually
triggered from a developer laptop. Useful for catching divergences between
localnet's docker-compose Canton and the clustered Canton that production
faces — auth shape, topology propagation, namespace ownership, etc.

```bash
./integration-tests/run.sh --target devnet
./integration-tests/run.sh --target devnet --verbose   # see DPM INFO trace
```

The bringup is structurally identical to localnet (three bare-process DPM
instances, same `wait_for_server` and `configure_peers` flow), except:
- No Docker localnet — Canton is the production-shaped cluster in `ieu-devnet`.
- Canton gRPC admin (5002/5012/5022) and ledger (5001/5011/5021) ports are
  tunneled to localhost via `kubectl port-forward` (managed by
  `devnet.env.sh`'s `start_canton_tunnels`).
- DPM auth uses real Keycloak (the `JwtValidator`), not the test-mode
  `MockValidator` localnet uses. The test runner mints its own bearer token
  via password grant; per-party workflows use M2M `client_credentials`.
- Member parties (`P{N}_MEMBER_PARTY_ID`) are pre-provisioned, not allocated
  during the test. CanActAs grants on the freshly-created dec party are
  issued via DPM's `POST /auth/grant-rights` (Canton's gRPC
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
     required by DPM's `POST /auth/grant-rights`).

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

Build and push to ECR:

```bash
# Build
docker build -t dec-party-manager .

# Tag for ECR
docker tag dec-party-manager:latest public.ecr.aws/dlc-link/canton-decparty-manager:<version>

# Push
docker push public.ecr.aws/dlc-link/canton-decparty-manager:<version>
```

Replace the registry/org (`public.ecr.aws/dlc-link`) and `<version>` with your own.

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
