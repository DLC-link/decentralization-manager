# Canton Decentralized Party Manager

A web application for managing decentralized parties in Canton blockchain networks. Provides a user interface for onboarding new parties, deploying governance contracts, and managing participant membership.

## Features

- **Web-Based Management UI**: React frontend for managing decentralized parties
- **Multi-Party Onboarding**: Coordinated workflow for creating decentralized party namespaces
- **Contract Deployment**: Upload DAR files and deploy governance contracts with multi-party signing
- **Governance Actions**: View and manage governance confirmations with threshold-based execution
- **Participant Management**: View party membership, kick participants with threshold-based voting
- **Keycloak Authentication**: Supports M2M (client_credentials) and password flows for Ledger API access
- **Secure P2P Communication**: Noise Protocol Framework for encrypted coordinator-attestor communication
- **Real-time Status**: Live peer connectivity monitoring and workflow progress tracking
- **Canton Integration**: Native gRPC integration with Canton Admin and Ledger APIs

## Documentation

- [Architecture Overview](docs/ARCHITECTURE.md) -- System architecture, core concepts, communication protocol, and technical constraints
- [Integration Guide](docs/INTEGRATION_GUIDE.md) -- Deployment, configuration, authentication setup, and full API reference
- [Use Cases](docs/USE_CASES.md) -- Vault governance, FAR rewards, multi-sig wallet, and utility service walkthroughs

## Architecture

The application runs as an HTTP server with an embedded React frontend. Multiple instances coordinate via the Noise Protocol:

- **Coordinator**: Initiates workflows and orchestrates multi-party operations
- **Attestors**: Respond to coordinator commands, sign proposals, and execute local operations
- **Automatic Key Management**: Noise keypairs are generated automatically on first run

```
┌─────────────────┐     Noise Protocol      ┌─────────────────┐
│  Participant 1  │◄───────────────────────►│  Participant 2  │
│   (Coordinator) │                         │    (Attestor)   │
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
DECPM_CANTON_ADMIN_HOST=localhost \
DECPM_CANTON_ADMIN_PORT=5002 \
DECPM_CANTON_LEDGER_HOST=localhost \
DECPM_CANTON_LEDGER_PORT=5001 \
DECPM_NOISE_PORT=9001 \
cargo run -- -d ./development/participant-1 serve --port 8081

# Or with a .env file in the data directory
cargo run -- -d ./development/participant-1 serve --port 8081

# Or with release build
cargo build --release
./target/release/dec-party-manager -d ./development/participant-1 serve --port 8081
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
    ├── dars/          # DAR files for contract deployment
    └── workflow-data/ # Workflow data (proposals, signatures, etc.)
```

The database file path can be overridden with the `--db` CLI flag.

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
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
| `DECPM_TIMEOUT_HANDSHAKE` | Noise handshake timeout in seconds | `30` |
| `DECPM_TIMEOUT_MESSAGE` | Noise message timeout in seconds | `120` |
| `DECPM_TIMEOUT_RETRY_ATTEMPTS` | Connection retry attempts | `3` |
| `DECPM_TIMEOUT_RETRY_DELAY` | Connection retry delay in seconds | `5` |

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

Party credentials (Keycloak auth, package IDs) are stored in the SQLite database and managed via the `/party-config` API endpoint:

```bash
# Configure party credentials
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

# Retrieve party config (secrets masked)
curl http://localhost:8081/party-config/decparty::1220abc...
```

## Workflows

### Creating a Decentralized Party (Onboarding)

1. Configure all participant nodes with each other's connection details via the `/network-config` API
2. Start all participant servers
3. On the coordinator's UI, click **Create Party** and enter a party ID prefix
4. The coordinator invites attestors and orchestrates:
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

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Serves the React frontend |
| `/node-config` | GET | Returns node configuration |
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
| `/invitations` | GET | Returns pending workflow invitations |
| `/invitations/accept` | POST | Accepts a pending invitation |
| `/invitations/decline` | POST | Declines a pending invitation |
| `/auth/status` | GET | Returns authentication status for configured parties |
| `/auth/test` | POST | Tests Keycloak authentication |
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
| `/amulet-rules` | GET | Returns AmuletRules contract |
| `/token-standard-contracts` | POST | Queries token standard contracts |
| `/dars/upload` | POST | Uploads DARs to the current node only |
| `/dars/distribute` | POST | Distributes DARs across all participants |
| `/dars/distribute/status` | GET | Returns DARs distribution workflow progress |
| `/packages/vetted` | GET | Returns packages uploaded on this node |

## Development

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run unit tests — includes the integration-test binary's helpers
# (Fixture, poll_until, Scenario DSL); the end-to-end test itself is
# gated by `#[ignore]` and is invoked separately via run.sh below.
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
Sample of a passing run:

```
==========================================
Running governance workflow e2e (Rust)
==========================================
running 1 test

INFO Phase: create_dec_party
INFO Using prefix: test-network-1
INFO Scenario "create decentralized party test-network-1"
INFO   GIVEN no party at this prefix yet
INFO   WHEN  P1 starts onboarding and P2/P3 accept invitations
INFO   THEN  eventually party visible in /decentralized-parties
INFO     ✓ (took 18.4s)
INFO Scenario "create decentralized party test-network-1" complete (18.7s)

INFO Phase: distribute_dars
INFO Scenario "distribute DARs"
INFO   GIVEN 3 DAR files on disk
INFO   WHEN  P1 uploads + distributes DARs, P2/P3 accept, status reaches completed
INFO Scenario "distribute DARs" complete (11.4s)

... (14 scenarios total) ...

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;
==========================================
Integration tests completed successfully!
==========================================
```

Each scenario follows the Given-When-Then DSL: `Given` is a precondition,
`When` is the test action, `Then` (or `Then eventually`) is the
postcondition assertion. A failure renders as
`ERROR Scenario "<name>" failed at <KIND> "<step>"` with a chained
`anyhow` cause trail pinpointing the failing HTTP call.

The exact `RUST_LOG` quiet preset is:
```
warn,hyper_noise::server=error,
governance_workflows::common::scenario=info,
governance_workflows::common::phases=info
```

#### Verbose mode

Use `--verbose` when diagnosing a stuck or failing run. Sets:
```
dec_party_manager=info,tokio_noise=error,hyper_noise=error,
governance_workflows=info
```

Surfaces all dec-party-manager INFO output (peer connections, Noise
handshakes, workflow internals) plus the test crate's full helper
chatter (`Waiting for invitation on …`, `Accepting invitation …`).
The cargo test runner is also INFO, so individual test cases narrate.

#### Prerequisites

`docker`, `docker compose v2`, `jq`, `curl`, `lsof`. The script
verifies these up front and bails with a clear message if any are
missing or if a previous run leaked a manager process holding one of
the HTTP/Noise ports (8081–8083, 9001–9003).

### Frontend Development

```bash
cd frontend
npm install
npm run dev     # Development server with hot reload
npm run build   # Production build (output to dist/)
```

The frontend is embedded into the Rust binary at build time via `build.rs`.

## Docker Image

Build and push to ECR:

```bash
# Build
docker build -t dec-party-manager .

# Tag for ECR
docker tag dec-party-manager:latest public.ecr.aws/your-repo/dec-party-manager:v1.0.0

# Push
docker push public.ecr.aws/your-repo/dec-party-manager:v1.0.0
```

## License

Proprietary - All rights reserved.
