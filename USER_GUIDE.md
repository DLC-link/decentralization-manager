# Canton Decentralized Party Manager - User Guide

This is an operator quick-start. The application is configured entirely through
`DECPM_*` environment variables (or a `.env` file placed in the directory given
by `--dir` / `DECPM_DIR`).
There is **no TOML config file** — every setting is an env var / CLI flag.

For end-to-end workflow walkthroughs (onboarding a party, deploying contracts,
kicking a participant), see the [Use Cases](docs/USE_CASES.md).

## How the nodes coordinate

Decman nodes never connect to each other. Each node talks only to its own
Canton participant, over the Admin API and the Ledger API. Canton holds
everything two operators share:

- The **synchronizer topology store** holds partially signed topology
  proposals. The proposer writes one change. Every member co-signs it by
  transaction hash. Canton merges the signatures, and the change becomes
  effective.
- **Daml contracts** hold the workflow intent. A run starts as a
  `WorkflowProposal`. The invitees accept or decline it on the ledger.

So you never open a port to a peer. You exchange one text string per peer, and
Canton does the rest.

## Quick Start with Docker

Build the image locally, then run a single instance. The build fetches the
`canton-lib` Rust dependency from GitHub over SSH. So forward an SSH key
registered on a GitHub account with BuildKit's `--ssh` flag. `canton-lib` is
public, so no special repository access is needed:

```bash
# Build the image (replace the key path with your own GitHub-registered key)
docker build --ssh default=$HOME/.ssh/id_ed25519 -f development/Dockerfile -t dec-party-manager .

# Run
docker run -p 8080:8080 -v ./data:/data \
  -e DECPM_PORT=8080 \
  -e DECPM_CANTON_ADMIN_HOST=canton-node \
  -e DECPM_CANTON_ADMIN_PORT=5002 \
  -e DECPM_CANTON_LEDGER_HOST=canton-node \
  -e DECPM_CANTON_LEDGER_PORT=5001 \
  -e DECPM_CANTON_SYNCHRONIZER=global \
  -e DECPM_CANTON_NETWORK=devnet \
  dec-party-manager
```

Then open the web UI at `http://localhost:8080`.

The `-v ./data:/data` mount keeps the SQLite database (peers, credentials,
workflow runs), the DAR files, and the ACS spool across restarts.

`/` is the image's default `DECPM_DIR`, and the app writes `$DECPM_DIR/data` —
so that is the directory to mount. Point the mount somewhere else and pass
`-e DECPM_DIR=<parent>` to match, or the container writes inside its own
filesystem and the data disappears with it.

### Nonroot image

Every release is published twice — `…:<tag>` and `…:<tag>-nonroot`. Both hold
the same binary. The second runs as uid 65532 rather than root. Deploy that one
where a policy forbids root containers.

Its `DECPM_DIR` defaults to `/home/nonroot`, because uid 65532 cannot create
`/data`. So the mount moves with it, and the host directory has to belong to
that uid. A bind mount keeps the host's ownership, so the first write fails
without the `chown`.

Make it recursive. A `./data` carried over from the root image holds root-owned
files: the SQLite database, the DAR directory, and the ACS spool. Uid 65532
cannot open those. A non-recursive `chown` fixes the first write, and then
fails on the second start:

```bash
mkdir -p ./data && sudo chown -R 65532:65532 ./data
docker run -p 8080:8080 -v ./data:/home/nonroot/data \
  ... public.ecr.aws/dlc-link/decentralization-manager:<tag>-nonroot
```

Nothing else changes: same ports, same env vars, same entrypoint. For
Kubernetes, see the [Deployment Guide](docs/DEPLOYMENT_GUIDE.md).

## Configuration

All configuration is supplied via `DECPM_*` environment variables. The key ones:

| Variable | Description | Default |
|----------|-------------|---------|
| `DECPM_PORT` | Port for the HTTP / web UI server | `8080` |
| `DECPM_METRICS_PORT` | Port serving Prometheus metrics at `/metrics`, on its own listener (`0` disables it) | `9464` |
| `DECPM_LOG_FORMAT` | Log format. Set `text` for the readable console format while working locally | `json` |
| `DECPM_CANTON_ADMIN_HOST` | Canton Admin API host | `127.0.0.1` |
| `DECPM_CANTON_ADMIN_PORT` | Canton Admin API port | `5002` |
| `DECPM_CANTON_LEDGER_HOST` | Canton Ledger API host | `127.0.0.1` |
| `DECPM_CANTON_LEDGER_PORT` | Canton Ledger API port | `5001` |
| `DECPM_CANTON_SYNCHRONIZER` | Canton synchronizer name | `global` |
| `DECPM_CANTON_NETWORK` | Canton network (`devnet`, `testnet`, `mainnet`) | `devnet` |
| `DECPM_CANTON_ADMIN_TLS` | Speak TLS to the Canton Admin API | `false` |
| `DECPM_CANTON_ADMIN_TLS_CA_CERT` | PEM of the CA that issued the Admin API certificate, when it is a private one | _(platform trust store)_ |
| `DECPM_CANTON_LEDGER_TLS` | Speak TLS to the Canton Ledger API | `false` |
| `DECPM_CANTON_LEDGER_TLS_CA_CERT` | PEM of the CA that issued the Ledger API certificate | _(platform trust store)_ |
| `DECPM_OBSERVER_POLL_SECS` | Seconds between observer ticks. A tick reads the ledger, co-signs, and drives every run. Mainnet guidance is `10` | `3` |
| `DECPM_HEARTBEAT_INTERVAL_SECS` | Seconds between heartbeats on this node's registry contract | `3600` |
| `DECPM_HEARTBEAT_MIN_INTERVAL_SECS` | Floor the registry contract enforces between two heartbeats. Clamped to the interval above | `60` |
| `DECPM_PEER_STALE_FACTOR` | A peer reads as `Stale` after this many of its own heartbeat intervals | `3` |
| `DECPM_PROPOSAL_TTL_SECS` | Lifetime of a `WorkflowProposal`. An invitee cannot accept it after it expires | `604800` |
| `DECPM_AUTO_UPLOAD_COORDINATION_DAR` | Upload and vet the embedded coordination package at startup | `true` |
| `DECPM_ACS_SPOOL_DIR` | Directory that holds exported ACS snapshots for an add-party handoff | _(`{dir}/data/acs`)_ |
| `DECPM_COORDINATION_PACKAGE_REF` | Package reference the coordination templates resolve against | `#decman-coordination-v1` |

Both Canton channels default to plaintext h2c, which is what a participant
reachable only over a trusted private network serves. If yours has TLS
enabled, set the flags above; mTLS and certificate-name overrides are covered
in the [README](README.md#tls-to-the-participant).

The [README](README.md#environment-variables) lists every variable, including
the authentication, database, and reward-automation ones.

Instead of `-e` flags, you can place a `.env` file in the directory given by
`--dir` / `DECPM_DIR` (its root — not the `data/` subfolder). It is loaded
automatically on startup (before CLI parsing), so any `DECPM_*` key set there
takes effect:

```env
DECPM_PORT=8080
DECPM_CANTON_ADMIN_HOST=canton-node
DECPM_CANTON_ADMIN_PORT=5002
DECPM_CANTON_LEDGER_HOST=canton-node
DECPM_CANTON_LEDGER_PORT=5001
DECPM_CANTON_SYNCHRONIZER=global
DECPM_CANTON_NETWORK=devnet
```

## Port Requirements

| Port | Purpose |
|------|---------|
| 8080 | HTTP API and web UI (default, `DECPM_PORT`) |
| 9464 | Prometheus metrics at `/metrics` (default, `DECPM_METRICS_PORT`) |

A node opens no other listener. No peer ever connects to it, so neither port
has to be reachable from another operator's network.

## Operator walkthrough

### 1. Set this node's identity

Each node needs one **node party**. The node party is a normal Canton party.
This node's participant hosts it with Submission permission. The node signs
everything with it: the registry entry, the proposals, the acceptances, the
submission signatures, and the ACS manifests.

Set it once, before anything else:

1. Allocate the node party on your participant with Submission permission.
2. Grant the Ledger API user `CanActAs` and `CanReadAs` on that party.
3. Save the identity with `PUT /node-identity`.

```bash
curl -X PUT http://localhost:8080/node-identity \
  -H "Content-Type: application/json" \
  -d '{
    "node_party_id": "node1::1220abc...",
    "user_id": "ledger-api-user",
    "keycloak_url": "https://keycloak.example.com",
    "keycloak_realm": "my-realm",
    "keycloak_client_id": "my-client",
    "keycloak_client_secret": "secret-value"
  }'
```

The handler reads the head topology state before it saves anything. It refuses
a party that this participant does not host with Submission permission. A party
hosted with Confirmation permission only cannot submit Daml commands, so it
cannot serve as a node party.

`PUT /node-identity` needs the admin role. It is exempt from authentication
only while the credentials table is entirely empty, which is the state of a
fresh node. A node that already holds party credentials needs an admin JWT.

`GET /node-identity` reads the identity back and reports the hosting
permission. It needs the admin role too. The network panel shows the same node
party on this node's own row. The **Share my identity** button stays disabled
until the identity exists.

Without a node identity the peers table reads every peer as `Unknown`, and the
UI asks you to configure the identity.

### 2. Add your peers

A peer is three values: the participant id, the node party, and a display name.
There is no address, no port, and no key.

Press **Share my identity** in the network panel. The button copies one string:

```
participant_id,node_party_id,name
```

Send that string to the other operator over any channel you trust. Paste the
string that operator sends you into **Add peer**. Both operators do this,
because each side names the other's node party as an observer of its own
contracts.

`GET` and `POST /network-config` do the same over HTTP:

```bash
curl -X POST http://localhost:8080/network-config \
  -H "Content-Type: application/json" \
  -d '[
    {
      "participant_id": "participant2::1220def...",
      "name": "Participant 2",
      "party": "node2::1220def..."
    }
  ]'
```

### 3. Read the peers table

Every node publishes one `DecmanNode` registry contract. The contract names
this node's peers as observers, and the node heartbeats on it on a timer. The
peers table reads those contracts.

| Status | What it means |
|--------|---------------|
| `Active` | The peer's registry entry is visible, and its last heartbeat is recent. |
| `Stale` | The entry is visible, but its last heartbeat is older than `DECPM_PEER_STALE_FACTOR` times the peer's own heartbeat interval. |
| `Unknown` | No entry signed by that peer's node party is visible. The peer has not added you yet, or this node has no identity yet. |
| `Unvetted` | The peer's participant has not vetted the coordination package. |

`Active` is not a live connection, because the nodes hold no connection. The
value is the age of the last heartbeat, and the UI labels it that way.

`Unvetted` blocks a peer in both directions. Canton rejects a contract whose
observer's participant has not vetted the package. So this node leaves an
unvetted peer out of its registry entry, and adds it on a later tick once the
vetting appears. Each node uploads and vets the coordination package itself at
startup, and the node health card reports that upload.

`GET /registry` lists the same entries in three groups: this node's own entry,
the entries of configured peers, and inbound entries. An inbound entry is one
whose signatory is not in your peers table yet, which means that operator added
you before you added them.

### 4. Start a workflow

Start a run from the UI as before: **Create Party**, **Deploy Contracts**, **Add
member**, **Kick Participant**, or **Change Threshold**.

The node pushes nothing to the invitees. It creates one `WorkflowProposal` on
the ledger with the invitees as observers. The proposal records the intent. It
also records what every member validates against: the kind, the participants,
the prefix, the threshold, the DAR pins, and the base serials.

The node checks every invitee before it creates the proposal. It answers HTTP
409 when an invitee has not vetted the coordination package, has no visible
registry entry, or reports an older coordination version. The error names the
participants.

### 5. Accept an invitation

The invitee's node finds the proposal on its next observer tick, and writes a
card in the notifications view. The card names the proposer, the kind, the
party, and — for a DAR run — each filename with its pinned sha256.

Press **Accept**. The node exercises `WorkflowProposal_Accept` and publishes
the key material the run needs. Press **Decline** and the node exercises
`WorkflowProposal_Decline` at once.

You accept once per run. After that the node co-signs every topology change
that matches the proposal you accepted. It validates each change against the
proposal, the counted acceptances, and the head topology state before it signs.
A mismatch fails the run, and the card shows the reason.

### 6. Watch the nodes co-sign

The proposer writes the topology change into the synchronizer store as a
partially signed proposal. Each member's observer loop finds that proposal,
validates it, and co-signs it by transaction hash. Canton merges the signatures
by fingerprint. The change becomes effective once the party's threshold is met.

The progress card shows the current step. Onboarding, for example, walks the
proposer through `GenerateKeys`, `WaitingForAcceptances`, `ProposeNamespace`,
`AwaitNamespace`, `ProposeParty`, `AwaitParty`, and `Complete`. A member walks
through `GenerateKeys`, `CoSignNamespace`, `CoSignParty`, and `Complete`.

How many signatures a run needs depends on the kind:

- Onboarding, add-party and DAR runs need every invitee to accept.
- Kick and change-threshold need `max(previous threshold, new threshold)` owner
  signatures, and the proposer counts as one of them.

For every new decentralized party each member generates one vault key named
`{prefix}-key`. That key has both the Namespace usage and the Protocol usage.
Its self-signed root namespace delegation publishes the public key. The
proposer copies that key into the party's topology mapping. A party created by
an older build keeps its two keys and keeps working.

### 7. Deploy contracts

The proposer prepares each transaction and opens one `SubmissionRound` per
prepared transaction. Each member recomputes the hash and decodes the
transaction. The member then checks that every root node creates a template
from a package the proposal names, and signs with its own party key. The
proposer verifies each signature, and executes once the party's signing
threshold is met.

The signing window is bounded. The prepared transaction sets a
`max_record_time`, and the synchronizer's preparation-time tolerance bounds the
window further. The proposer prepares an expired round again, and the members
sign the new one.

### 8. Distribute DARs

DAR bytes never travel between nodes. The proposer pins each file by its sha256
and its main package id, and uploads the file to its own participant. Every
other operator gets the same files out of band, and uploads them locally:

```bash
curl -X POST http://localhost:8080/dars/upload \
  -H "Content-Type: application/json" \
  -d '{
    "pin_instance": "<runId>",
    "dar_files": [{ "filename": "my-app.dar", "data": "<base64>" }]
  }'
```

`pin_instance` is the run id the invitation names. The node refuses any file
whose sha256 matches no pending pin of that run, and it pins the main package
id on the upload. The run completes when the topology store shows every pinned
package vetted on every participant.

The invitation card prints each filename with its pinned hash in full. Compare
that hash against the published release before you accept, because accepting
pins the content and not just the name.

### 9. Add a member and hand over the ACS

Add-party keeps Canton offline party replication. The operator moves the
snapshot file, and no node sends it.

1. Every current host captures its export offset when it co-signs the change.
2. The topology change marks the joining participant `Onboarding`.
3. Each current host exports a snapshot into `DECPM_ACS_SPOOL_DIR` and
   publishes an `AcsManifest`. The manifest pins the party, the target
   participant, the activation serial, the size, the sha256, and the package
   ids.
4. The joining operator reads the manifests, downloads one snapshot, and
   uploads it to the joining node.

```bash
# Read the manifests the current hosts published
curl -H "Authorization: Bearer $ADMIN_JWT" \
  "http://localhost:8080/acs-manifests/<party>"

# Download the snapshot from a current host
curl -H "Authorization: Bearer $ADMIN_JWT" \
  "http://<current-host>:8080/acs-export/<party>/<joining-participant>?serial=<N>" \
  -o party.acs.gz

# Upload it to the joining node
curl -X POST -H "Authorization: Bearer $ADMIN_JWT" \
  --data-binary @party.acs.gz \
  "http://localhost:8080/acs-import/<party>?serial=<N>&exporter=<exporter-participant>"
```

Every ACS call needs the admin role. The joining node checks the size and the
sha256 against the manifest, and checks that it has vetted every package the
manifest names. It then disconnects from the synchronizer, imports the
snapshot, reconnects, and clears its onboarding flag.

A snapshot of zero bytes skips the transfer. The joining node clears the flag
directly.

The joining node's run waits in `SyncAcs` until the import finishes. The
progress card shows the transfer while it runs.

### 10. Cancel, decline, and retry

- **Cancel** archives the `WorkflowProposal`. Members stop co-signing, because
  the proposal they matched is gone. A topology proposal that already reached
  the threshold still becomes effective, and the UI says so.
- **Decline** fails the run for the kinds that need every invitee: onboarding,
  add-party, contracts, and DARs. For a kick or a change-threshold, a decline
  fails the run only when the remaining invitees cannot reach the quorum.
- **Retry** re-runs the same ensure loop. The proposer re-reads the serial, and
  proposes again only when its proposal is gone and the base serial still
  holds. Members issue the co-signature again. Nothing is broadcast.

## Next Steps

- **Walk through onboarding, deploying contracts, and kicking a participant** —
  [Use Cases](docs/USE_CASES.md)
