# decman-wallet

The wallet side of DecMan's tenant API (`/v0/tenant/*`) — the library a wallet
provider embeds to run a **co-validated** Canton party.

A co-validated party is hosted on several participants at once but controlled by a
single key its owner holds. One key, one signature, N hosts: the owner keeps sole
control and gains uptime, because any one host can be down without the party going
with it.

The property this crate exists to preserve: **the private key never leaves the
process using it.** DecMan only ever receives a public key and signatures over
hashes it computed itself. Nothing here transmits key material, and the node-side
code cannot generate a party key at all.

## Onboarding

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

// Every host prepares, the wallet signs locally, and onboards on every host.
let party = onboard_co_validated(&hosts, &key, "alice", Some(2)).await?;

// Authorization is not instant — poll until every host reports the party hosted.
let reports = statuses(&hosts, &party.party_id).await;
```

The onboarding shape is load-bearing: **every** host builds the topology, the
wallet requires their output to be byte-identical, signs it **locally**, and
submits that same signed bundle to **every** host itself. No host relays to
another. The wallet signs a hash it cannot itself recompute, so agreement between
hosts is what stands between it and a host that returns the hash of a mapping it
never showed — one honest host defeats a lying one. Canton keeps the topology a
proposal until the last host signs, so the party is only live once all of them
report it. `onboard_co_validated` records a failed host in its report rather than
aborting, and onboarding is idempotent, so stragglers can simply be retried.

The wallet signs one hash per topology transaction, each hash computed by Canton
and returned by the prepare step. Nothing on either side re-derives a Canton hash.

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

## Transacting

Transacting as the party is **not** part of DecMan's tenant API, and not part of
this crate's node-facing client. Once a party is onboarded it is an ordinary
Canton external party: a wallet reads its contracts and signs its submissions
directly against Canton's Ledger API, authorizing each with the
[`ExternalKeyPair`] it holds. DecMan's role ends at onboarding — it has no business
holding a Ledger-API credential on the party's behalf.

This crate gives you the two halves a wallet needs for that: [`ExternalKeyPair`]
for the signing, and the onboarding flow above to bring the party up.

## Why the key belongs in the embedding process

A wallet holds two secrets: the party's signing key and the provider's tenant API
key. Neither belongs in a browser page. Keeping them in the process that embeds
this library also keeps one implementation of the crypto — signing in the browser
would mean a second copy of the fingerprint derivation and signature encoding, free
to drift from the one the node validates against.
