# decman-wallet

The wallet side of DecMan's tenant API (`/v0/tenant/*`) — the library a wallet
provider embeds to run a **co-validated** Canton party, plus a demo wallet that
drives it end to end.

A co-validated party is hosted on several participants at once but controlled by a
single key its owner holds. One key, one signature, N hosts: the owner keeps sole
control and gains uptime, because any one host can be down without the party going
with it.

The property this crate exists to preserve: **the private key never leaves the
process using it.** DecMan only ever receives a public key and signatures over
hashes it computed itself. Nothing here transmits key material, and the node-side
code cannot generate a party key at all.

## The library

```rust
use common::canton_id::CantonId;
use decman_wallet::{ExternalKeyPair, TenantClient, WalletHost, onboard_co_validated, statuses};

// The hosting set: one DecMan endpoint + participant id per host.
let hosts = vec![
    WalletHost::new(TenantClient::new("https://node1.example.com", api_key)?, node1_id),
    WalletHost::new(TenantClient::new("https://node2.example.com", api_key)?, node2_id),
    WalletHost::new(TenantClient::new("https://node3.example.com", api_key)?, node3_id),
];

// Generated here, held here, never transmitted.
let key = ExternalKeyPair::generate();

// Prepare on the first host, sign locally, onboard on every host.
let party = onboard_co_validated(&hosts, &key, "alice", Some(2)).await?;

// Authorization is not instant — poll until every host reports the party hosted.
let reports = statuses(&hosts, &party.party_id).await;
```

The onboarding shape is load-bearing: **one** host builds the topology, the wallet
signs it **locally**, and the wallet submits that same signed bundle to **every**
host itself. No host relays to another. Canton keeps the topology a proposal until
the last host signs, so the party is only live once all of them report it —
`onboard_co_validated` records a failed host in its report rather than aborting,
and onboarding is idempotent, so stragglers can simply be retried.

The wallet signs one hash per topology transaction, each hash computed by Canton and
returned by the prepare step. Nothing on either side re-derives a Canton hash.

Onboarding rides Canton's **admin** API rather than the Ledger API's
`AllocateExternalParty`. That RPC is a convenience wrapper around the same topology
write, and the wrapper is where the authorization check and the party-allocation
quota live — it wants either `ParticipantAdmin` or a `user_id` matching the caller,
and naming a user turns on a quota that defaults to zero. Writing topology directly
needs no ledger credential, so onboarding does not depend on how a node's ledger
users happen to be provisioned.

`confirmation_threshold` is how many hosts must confirm a transaction. Passing
`None` lets DecMan default it to `N-1`, which is what keeps a host able to exit
later; a threshold of `N` is rejected.

Transacting goes through `create_contract`, which prepares a CREATE on a host,
signs the returned transaction hash locally, and executes it. The API supports
CREATE only today — no Exercise, no Archive.

## The demo wallet

Behind the `demo` feature, so depending on this crate for the client does not drag
in an HTTP server and a UI bundle.

```sh
just demo-wallet "http://localhost:8080=participant::1220aa… http://localhost:8081=participant::1220bb…"
```

or directly:

```sh
cargo run -p decman-wallet --features demo --bin decman-wallet-demo -- \
  --host http://localhost:8080=participant::1220aa… \
  --host http://localhost:8081=participant::1220bb… \
  --host http://localhost:8082=participant::1220cc… \
  --api-key "$DECMAN_TENANT_API_KEY"
```

Then open <http://127.0.0.1:7878>: name a party, watch it come up on every host,
and transact as it.

Hosts and the API key are process configuration, not UI state — this process holds
the party's private key and the provider's API key, which is why it binds to
loopback and why the browser never receives either. `--state-file` persists the
party's key (mode 0600) so a restart keeps the same party; without it each run is
a fresh demo.

The UI is its own Vite app under `frontend/`, separate from the DecMan server's UI,
and is embedded into the binary by `build.rs`. `DECMAN_SKIP_FRONTEND=1` skips that
npm build while iterating on the Rust side; `just demo-wallet-ui` runs the UI with
hot reload against a demo wallet already running on :7878.

## Why the key is in this process and not the browser

Three reasons, in order of weight:

1. A wallet holds two secrets — the party's signing key and the provider's tenant
   API key. Neither belongs in a page.
2. One implementation of the crypto. Signing in TypeScript would mean a second
   copy of the fingerprint derivation and signature encoding, free to drift from
   the one the node validates against.
3. CORS. DecMan is same-origin-only unless an operator sets `--allowed-origin`, and
   it accepts a single origin — so a page cannot call three hosts directly anyway.

A real wallet app is a local process holding a key, which is exactly what this is.
