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
use decman_wallet::{ExternalKeyPair, TenantClient, WalletHost, execute_co_validated, statuses};

// The hosting set: one DecMan endpoint + participant id per host.
let hosts = vec![
    WalletHost::new(TenantClient::new("https://node1.example.com", api_key)?, node1_id),
    WalletHost::new(TenantClient::new("https://node2.example.com", api_key)?, node2_id),
    WalletHost::new(TenantClient::new("https://node3.example.com", api_key)?, node3_id),
];

// Generated here, held here, never transmitted.
let key = ExternalKeyPair::generate();

// Every host prepares, the wallet signs locally, and onboards on every host.
let party = execute_co_validated(&hosts, &key, "alice", Some(2)).await?;

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
report it. `execute_co_validated` records a failed host in its report rather than
aborting, and onboarding is idempotent, so stragglers can simply be retried.

The wallet signs one hash per topology transaction, each hash computed by Canton
and returned by the prepare step. The wallet does not re-derive topology hashes.
For a transaction, the wallet does recompute the hash before it signs. See
[the check before signing](#the-check-before-signing).

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

Transacting as the party is **not** part of DecMan's tenant API. DecMan holds no
Ledger API credential for the party. Once onboarded, the party is an ordinary
Canton external party.

The wallet talks to a participant's JSON Ledger API itself. This crate gives it
two Token Standard flows:

- `send_transfer` sends an asset from the party.
- `accept_transfer` accepts a transfer that another party offered to it.

```rust
use std::time::Duration;

use decman_wallet::{
    AMULET_TRANSFER_INSTRUCTION_TEMPLATE, LedgerClient, RegistryClient, TransferArgs,
    accept_transfer, send_transfer,
};

// One participant that hosts the party, and the instrument's registry.
let ledger = LedgerClient::new(
    "http://localhost:7575/api/json-api",
    Some(ledger_token),
    "wallet-user",
)?;
let registry = RegistryClient::new("https://scan.example.com", None)?;

// Send 1.25 Canton Coin. The offer expires after 24 hours.
let args = TransferArgs::new(
    &party_id,
    &receiver,
    "1.25",
    &dso_party,
    "Amulet",
    Duration::from_secs(24 * 3600),
);
let sent = send_transfer(&ledger, &registry, &key, &args).await?;
tracing::info!(kind = %sent.transfer_kind, update_id = %sent.update_id, "sent");

// Accept every Canton Coin offer the party can accept now. The listed
// template comes from the participant and only picks the offers to try.
// The check binds the template the wallet passes.
for offer in ledger.transfer_instructions(&party_id).await? {
    if offer.template != AMULET_TRANSFER_INSTRUCTION_TEMPLATE {
        continue;
    }
    accept_transfer(
        &ledger,
        &registry,
        &key,
        &party_id,
        &offer.contract_id,
        AMULET_TRANSFER_INSTRUCTION_TEMPLATE,
    )
    .await?;
}
```

### The steps, next to the SDK

The flows follow the splice-wallet-kernel SDK step for step.

| Step | This crate | splice-wallet-kernel SDK | Endpoint |
|---|---|---|---|
| Read holdings | `LedgerClient::holdings`, `unlocked_holdings` | `token.utxos.list` | `GET /v2/state/ledger-end`, `POST /v2/state/active-contracts` |
| Fix the arguments | `TransferArgs::new` | `buildTransferChoiceArgs` | none |
| Get the factory | `RegistryClient::transfer_factory` | `fetchTransferFactoryChoiceContext` | `POST {registry}/registry/transfer-instruction/v1/transfer-factory` |
| Build the command | `transfer_choice_argument` | `createTransferFromContext` | none |
| Prepare | `LedgerClient::prepare` | `ledger.prepare` | `POST /v2/interactive-submission/prepare` |
| Check | the check below | `computePreparedTransactionHash`, optional | none |
| Sign | `Signer::sign` | `sign` | none |
| Execute | `LedgerClient::execute` | `execute` | `POST /v2/interactive-submission/executeAndWait` |
| List offers | `LedgerClient::transfer_instructions` | `token.transfer.pending` | `POST /v2/state/active-contracts` |
| Accept | `accept_transfer` | `token.transfer.accept` | `POST {registry}/registry/transfer-instruction/v1/{cid}/choice-contexts/accept`, then prepare to execute |

The crate differs from the SDK in four ways:

- It sends the hashing scheme version that the prepare returned. The SDK always sends V2.
- It spends every unlocked holding of the instrument, up to 100. The SDK picks
  holdings by amount. With more than 100 unlocked holdings, the send takes the
  first 100 that the ledger lists. If those hold less than the amount, the send
  fails at the prepare step, although the party holds enough. The caller can
  then name the holdings to spend in `TransferArgs::input_holding_cids`.
- It skips every holding with a lock. The SDK also spends a holding whose lock has expired.
- It lists only offers that wait for the receiver and have not expired. The SDK's `pending` lists every instruction.

### The check before signing

The wallet does not sign a hash until it knows what the hash covers. A
participant could return one transaction with the hash of another. It could also return a
different transaction with that transaction's correct hash.

So the wallet checks each prepared transaction before the key signs:

1. It decodes the `PreparedTransaction` protobuf.
2. It recomputes the hash with `decman_lib::canton_hash`. The hash must equal
   the one the participant returned.
3. It reads the root of the transaction. There must be exactly one root, and it
   must be an exercise.
4. The exercise must use the expected interface, choice and contract. Its only
   acting party must be the wallet's party.
5. The transaction's metadata must carry the command id the wallet sent to the
   prepare. The hash covers this id. The completion lookup after
   `Error::ExecuteOutcomeUnknown` uses it.
6. The root must act on the template the caller names. For an accept, that is
   the template passed to `accept_transfer`. For a send, it is
   `TransferArgs::factory_template`, when set. The check compares the module
   and the entity. It also compares the package name, or the package id when
   the caller gave one.
7. For a send, the choice argument must match the request. The check compares
   the instrument admin, sender, receiver, amount and instrument. It also
   compares both times, and the input holdings in order.
8. The choice argument's `extraArgs` must equal the ones the wallet sent. That
   is the registry's context and an empty `meta`. For a send, `transfer.meta`
   must be empty too.

A transaction that fails returns `Error::TransactionRefused`. The `Refusal` in
it names what did not match. The wallet signs nothing and executes nothing.

The check compares `extraArgs` and `meta` in the JSON Ledger API's encoding.
It ignores spellings that do not change a value. An Int64 may be a number or a
string. A decimal may carry trailing zeros. A time may carry fractional
seconds or an offset. The check refuses a value it cannot read, such as a
generic map.

The accept binds the template because the instruction's contract id usually
comes from the participant's own listing. Any template can implement the
`TransferInstruction` interface, and its accept can do anything. So the caller
names the template it trusts:

| Instrument | Template |
|---|---|
| Canton Coin | `AMULET_TRANSFER_INSTRUCTION_TEMPLATE`, `#splice-amulet:Splice.AmuletTransferInstruction:AmuletTransferInstruction` |
| CBTC on DA's Utility registry | `CBTC_TRANSFER_INSTRUCTION_TEMPLATE`, `#utility-registry-app-v0:Utility.Registry.App.V0.Model.Transfer:TransferOffer` |

`TransferInstruction::template` shows each listed offer's template. The
participant supplies it, so it is for display only.

The `#` form names the package by name. Any package with that name passes,
including a look-alike package with another package id. To pin one package,
pass the template as `<package-id>:Module:Entity`, without the `#`. The check
then compares the package id too. The prepare also names that id in its
`packageIdSelectionPreference`, so the participant interprets the choice with
that package.

`TransferInstruction::package_id` holds each listed offer's package id. The
example's `pending` mode prints it. The participant supplies this id, so check
it against the package you trust before you pin it.

A send pins the factory the same way, with `TransferArgs::factory_template`.
`TransferReceipt::factory_template` shows the factory's template as the
registry names it, with its package id. Check that id the same way before you
pin it.

Every package upgrade changes the package id. A pinned wallet refuses accepts
after an upgrade until the operator updates the id.

The check covers hashing schemes V2 and V3. Canton 3.6 prepares with V2 unless
the request asks for another version, and the wallet never asks. The check
refuses V4 and any unknown version.

The check does not compare these:

- Anything below the root exercise. That part is the instrument admin's code,
  such as the factory's transfer logic.
- The template's package id, unless the expected template names one. The `#`
  form compares the package name, so any package with that name passes.
- The factory's template on a send, unless `TransferArgs::factory_template`
  names one.
- The rest of the transaction's metadata, such as its submitting parties, its
  synchronizer and its preparation time.

### The registry

The registry is the instrument admin's service. The caller passes its root URL,
and the client adds the `/registry/...` paths.

| Instrument | Registry root |
|---|---|
| Canton Coin (`Amulet`) | The base URL of a Splice scan |
| CBTC and other utility instruments | `{utility}/api/token-standard/v0/registrars/{admin}` |

Canton Coin and CBTC use the same code. Only the registry root differs.

No scan URL is fixed. Read the scans from the DSO endpoint, then keep the first
one that answers. Many advertised scans do not answer.

```sh
dso=https://docs.dev.global.canton.network.sync.global/dso
for scan in $(curl -s "$dso" \
    | jq -r '.sv_node_states[].contract.payload.state.synchronizerNodes[][1].scan.publicUrl // empty' \
    | sed 's:/*$::' | sort -u); do
  curl -sf -o /dev/null "$scan/registry/metadata/v1/instruments" && echo "$scan"
done
```

### What the wallet needs from the participant

The ledger user needs `CanReadAs` and `CanExecuteAs` for the party. Never grant
it `CanActAs`. The participant relays what the key signed and must not submit
on its own.

### Two-step transfers

A receiver without a `TransferPreapproval` must accept each transfer. The
registry then answers `transfer_kind: "offer"`. The asset moves only when the
receiver calls `accept_transfer`.

A new party holds nothing, so it cannot send. It can still receive, and it can
accept an offer.

### Amounts

Amounts are decimal strings, such as `"1.25"`. The crate never parses them as
numbers, so it never rounds them. The ledger writes amounts with ten decimals,
such as `"1.2500000000"`. The check ignores trailing zeros when it compares
amounts.

### Timeouts and retries

Each call times out after 60 seconds. Pass another `ClientOptions::timeout` to
`LedgerClient::with_options` or `RegistryClient::with_options`.

The execute step commits the transaction. The flow returns
`Error::ExecuteOutcomeUnknown` when the request left the wallet but its answer
does not tell the outcome. These cases return it:

- The call timed out or the connection broke before the answer arrived.
- The answer's body ended early or did not finish before the timeout.
- The answer's body is not the JSON the client expects.
- The answer's body is larger than `ClientOptions::max_response_bytes`.
- The participant's gateway answered 502, 503 or 504.

The error's `source` holds the error behind it. The transaction may have
committed. A new send builds a new transaction, so a blind retry can pay twice.
Before you try again, check one of these:

- The party's holdings. A committed send has consumed its input holdings.
- The party's pending offers. A committed accept has removed its offer.
- The completion for the error's `command_id`.

A connection that never opened returns `Error::Transport`. Nothing reached the
participant, so a retry is safe.

### What the clients refuse

- Plain `http` to a host that is not loopback returns `Error::InsecureUrl`.
  Loopback is `localhost`, `127.0.0.0/8` and `::1`. Set
  `ClientOptions::allow_plain_http` to allow another host, such as a service
  inside a private cluster network.
- A response body over 64 MiB returns `Error::ResponseTooLarge`. Set
  `ClientOptions::max_response_bytes` to change the cap.
- A redirect returns `Error::Api` with its 3xx status. The clients follow no
  redirect, because it could resend the request to a URL they never checked.
- An instruction id that is not a Canton contract id returns
  `Error::InvalidContractId`, before any call. A contract id is lowercase hex
  of even length. It starts with `00` and has at least 66 digits. The registry
  call puts the id in its URL path, so the wallet checks it first.

### The example

`examples/token_transfer.rs` runs each flow from the command line. Set `MODE` to
`list`, `pending`, `send` or `accept`.

| Variable | Modes | Meaning |
|---|---|---|
| `PARTY_ID` | all | The party's id |
| `LEDGER_JSON_API_URL` | all | The JSON Ledger API URL, with its path prefix |
| `LEDGER_USER_ID` | all | The ledger user the wallet submits as |
| `LEDGER_TOKEN` | all, optional | A bearer token for the Ledger API |
| `DECMAN_WALLET_SEED` | `send`, `accept` | The party's base64 Ed25519 seed |
| `REGISTRY_URL` | `send`, `accept` | The registry root |
| `REGISTRY_TOKEN` | `send`, `accept`, optional | A bearer token for the registry |
| `RECEIVER`, `AMOUNT` | `send` | Who gets how much |
| `INSTRUMENT_ADMIN`, `INSTRUMENT_ID` | `send` | The instrument, such as the DSO party and `Amulet` |
| `VALIDITY_HOURS` | `send`, optional | How long the offer stays open; 24 by default |
| `FACTORY_TEMPLATE` | `send`, optional | The template the factory must have. Write `<package-id>:Module:Entity` to pin one package. |
| `INSTRUCTION_CID` | `accept` | The transfer instruction to accept |
| `INSTRUCTION_TEMPLATE` | `accept` | The template the instruction must have, such as `#splice-amulet:Splice.AmuletTransferInstruction:AmuletTransferInstruction`. Write `<package-id>:Module:Entity` to pin one package. |

The example never prints the seed. It refuses a seed whose fingerprint is not
the party's.

```sh
MODE=pending PARTY_ID=bob::1220… LEDGER_USER_ID=wallet \
  LEDGER_JSON_API_URL=http://localhost:7575/api/json-api \
  cargo run -p decman-wallet --example token_transfer
```

## Why the key belongs in the embedding process

A wallet holds two secrets: the party's signing key and the provider's tenant API
key. Neither belongs in a browser page. Keeping them in the process that embeds
this library also keeps one implementation of the crypto — signing in the browser
would mean a second copy of the fingerprint derivation and signature encoding, free
to drift from the one the node validates against.

## Adding a host to a party that already exists

Onboarding creates a party. `add_hosts` grows one, in the order Canton forces:
topology, then state, then activation.

```rust
use common::api::HostPermission;
use decman_wallet::{add_hosts, raise_threshold};

// Read the serial rather than remembering one: any PartyToParticipant write
// moves it.
let base_serial = current_hosts[0].client.party_state(&party_id).await?.serial;

// The party gained a host, but a host with no contracts confirms nothing.
let added = add_hosts(
    &current_hosts,
    &new_hosts,
    &key,
    &party_id,
    HostPermission::Confirmation,
    base_serial,
)
.await?;
assert!(added.replicated, "a joiner is hosted but still suspended");

// Not `added.serial`. Clearing each joiner's onboarding marker is itself a
// PartyToParticipant write, so by now the party has moved past the serial the
// add-hosts write carried and every host would answer 409.
let state = all_hosts[0].client.party_state(&party_id).await?;

// Only once the markers have cleared, because a marked host cannot confirm and
// so cannot count toward the threshold.
raise_threshold(&all_hosts, &key, &party_id, 2, state.serial).await?;
```

`base_serial` is the serial the wallet last saw for the party. Every host is
required to prepare the same bytes for it, which is what makes a single lying
host detectable. A host whose own view has moved on answers 409 rather than
preparing something different.

The wallet carries the ACS snapshot from a current host to each joiner itself.
There is no host-to-host channel: a partner's node is generally not in anyone
else's mesh, and the wallet already talks to all of them.

A joiner's import returns as soon as the node has asked Canton to clear the
marker; `add_hosts` then polls that joiner's status until the clear is
authorized. `replicated: false` means a joiner ran out that wait, not that the
import failed — the party is hosted there and holds no contracts.

`AddedHosts::without_package_preflight` names joiners whose source could not
check their vetted packages up front, so their import validated after
disconnecting rather than before. Empty is the good case.

### Failover

A party this crate drives signs its own transactions, so it gets failover from
`HostPermission::Confirmation` alone: the wallet can submit through any host.
Raising the threshold above 1 then adds co-validation.

`HostPermission::Submission` is for a party with no key of its own, whose host
submits on its behalf. That is a local party, and its namespace is a
participant's key, so no wallet signature can authorize its topology. Its hosts
are added with `/v0/tenant/add-hosts/authorize` instead. See
[Decentralizing an existing party](../../docs/DECENTRALIZING_AN_EXISTING_PARTY.md).

## Asking a host to leave a party

`TenantClient::unhost` asks one host to stop hosting the party. The host
removes itself with its own key, so the wallet signs nothing. It first archives
the Splice contracts its validator holds for the party.

```rust
use common::api::TenantUnhostRequest;

let host = &all_hosts[2];
let base_serial = host.client.party_state(&party_id).await?.serial;
let left = host
    .client
    .unhost(&TenantUnhostRequest {
        party_id: party_id.clone(),
        // A host removes only itself, so this must be its own id.
        participant_id: host.participant_id.clone(),
        base_serial,
        // `None` runs the Splice cleanup. `Some(false)` only for a host that
        // never acted as the party's Splice validator.
        splice_cleanup: None,
    })
    .await?;
```

A `202` means the host's write is not effective yet. Repeat the call later: it
answers `409` while the host waits for the change, and `200` once the host is
gone. A `400` means the hosts that remain could
not meet the threshold. Lower it with `raise_threshold` first, which moves the
threshold either way.

The Splice cleanup can take longer than the client's 60-second timeout. The
host keeps working after the client gives up, so repeat the call. A repeat
answers `409` while the first call still runs, and `200` once the host is gone.

## Rate limits

Each host limits the calls per minute that one tenant key may make. The
default is 120. A call past the limit returns `Error::Api` with `status: 429`.
The client does not retry it. The message gives the seconds to wait. In the
ACS copy that `add_hosts` runs, only the import of block 1 counts toward the
limit, so a copy that started is never refused partway.

## Keys the wallet does not hold

`ExternalKeyPair` keeps the seed in this process. A provider whose keys live in
a KMS implements [`Signer`] instead: the flows only ever need a public key and a
signature over a hash the node computed, so they ask for those rather than for
the key.

```rust
#[async_trait::async_trait]
impl decman_wallet::Signer for MyKmsKey {
    fn public_key_bytes(&self) -> [u8; 32] { self.cached_public_key }
    async fn sign(&self, message: &[u8]) -> decman_wallet::Result<[u8; 64]> {
        self.kms.sign(message).await
    }
}
```

Signing is async and fallible because a KMS signs over the network.
