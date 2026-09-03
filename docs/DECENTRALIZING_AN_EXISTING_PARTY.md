# Decentralizing an existing party

How to give a party that already exists more hosts, and how to give a local
party a key its owner holds.

This is the operator's counterpart to the scoping study. It covers what to run,
in what order, what each step can fail with, and how to recover. For why the
design is shaped this way, read the module docs in
[`crates/decman/src/workflow/external_party/`](../crates/decman/src/workflow/external_party/).

## Which path applies

The party's namespace decides it, and the namespace is the half of the party id
after `::`.

| The namespace is | Signing keys | The party is | Path |
|---|---|---|---|
| Its own key, held by a wallet | present | External | [Add hosts](#adding-hosts-to-an-external-party) |
| A participant's root key | **absent** | Local, unconverted | [Convert first](#converting-a-local-party), then add hosts |
| A participant's root key | **present** | Local, already converted | [Add hosts](#adding-hosts-to-an-external-party) — do **not** convert again |
| A `DecentralizedNamespaceDefinition` | — | A decparty | Use the add-party workflow, not this |

The namespace alone is not enough to pick a row. A converted party keeps its
participant's namespace forever — that is the point of the conversion being
in-place — so the presence of `party_signing_keys` is what distinguishes one
that has already been converted from one that has not. Converting twice is
refused, because it would replace the owner's key with someone else's.

To tell a local party from an external one, compare the party's namespace with
the hosting participant's:

```bash
curl -s "$HOST/external-parties" | jq '.parties[] | {party_id, fingerprint, host_count, threshold}'
```

A party listed there is external. One whose namespace equals a participant's own
namespace is local to that participant.

**A party can never change its namespace.** The party id embeds it. Converting a
local party gives it a signing key; it does not make it a decentralized-namespace
party, and no sequence of operations will.

## Two offers: co-validation or failover

`add-hosts/prepare` takes a `permission`, and it is the choice between the two
products rather than a tuning knob.

| `permission` | What the partner gets | What it costs them |
|---|---|---|
| `confirmation` (default) | Co-validation. Several hosts confirm for the party, and the threshold can rise above 1 afterwards | The party must sign its own submissions, so their application changes |
| `submission` | Failover hosting. Any one host can submit for the party, so uptime improves | Nothing. Their application is untouched |

**Lead with `submission` when the partner cannot change their application.** It
is also a step toward the other one: add failover hosts now, convert and raise
the threshold later.

Its limit is real, not a detail. Threshold 1 means each host acts alone — that
is hosting redundancy, not multi-party validation. And Canton refuses a
Submission host once the threshold is above 1 (`topology.proto`: "if threshold >
1, must be Confirmation or Observation"), so raising the threshold means moving
those hosts to Confirmation first.

## Adding hosts to an external party

Three phases, in the order Canton forces. Do not skip ahead: the export in
phase 2 needs the joiner's activation from phase 1 to exist.

### Phase 1 — topology

The wallet calls `/v0/tenant/add-hosts/prepare` on **every** host, current and
joining, and requires byte-identical answers before it signs. That comparison is
the only thing standing between the wallet and a host that returns the hash of a
mapping it never showed, so a wallet that prepares on one host is not doing this
safely.

`base_serial` is the serial the wallet last read for the party. Pin it: without
it, two hosts reading head state a moment apart build different transactions and
the comparison fails for a reason that is not an attack.

Then `/v0/tenant/add-hosts/onboard` on **each joining host only**. Canton needs
the party namespace plus each new participant; existing hosts are neither and
have nothing to add.

### Phase 2 — state

`GET /v0/tenant/{party}/acs/{target}?offset=N` on a host that already holds the
party, then `POST /v0/tenant/add-hosts/import` on the joiner, in a loop until
the import reports `complete`. The wallet carries the snapshot: there is no
host-to-host channel, because a partner's node is generally not in anyone else's
mesh and the wallet already talks to all of them.

**Relay in ranges, not in one shot.** The source exports once and stages the
snapshot to disk; each `GET` serves a byte range out of that file, and each
`POST` appends one. That is not an optimisation:

- A whole snapshot in one JSON body is refused past ~75 MiB. actix caps JSON at
  100 MiB and base64 inflates by 4/3, so a single-body relay has a ceiling far
  below what the export allows and it arrives as a bare 413.
- The joiner reports `received` after every range, and
  `GET /v0/tenant/{party}/acs-progress` reports it before one, so a wallet
  restarted from scratch resumes rather than beginning again. On a large party
  that is the difference between a retry and a restart.
- An `offset` that does not match what the joiner holds is **refused**, not
  written. Writing it would leave a hole, and Canton only discovers that
  mid-import with the participant already disconnected.

### Phase 3 — activation

The import endpoint clears the onboarding marker itself. Its response says
whether that succeeded. **`marker_cleared: false` means the party is hosted
there and still suspended** — it holds none of the party's contracts and
confirms nothing.

### Phase 4 — threshold, optional

`/v0/tenant/threshold/{prepare,onboard}`, and only after the markers clear. A
marked host cannot confirm, so a threshold raised to count one is a threshold
the party cannot meet.

## Converting a local party

`/v0/tenant/local-party/adopt-key/{prepare,onboard}` on the party's own node.

The owner must sign. Canton requires "party namespace + all the new signing key"
for adding a signing key, so the node's signature alone is not enough — the
owner's signature over the prepared hash is the proof they hold the key. Plan an
interaction with them; this is not an operator-only action.

The conversion also demotes the host from Submission to Confirmation, because
Canton refuses Submission once a party signs its own transactions. **The
partner's application must switch to interactive submission at this point.**
Coordinate the cutover: between the conversion landing and their application
being updated, the party cannot transact.

Afterwards the party is external in every respect except one: its namespace is
still the source participant's key, so **that node can unilaterally change the
party's topology forever**, including removing hosts. Say this to the partner
plainly rather than letting them infer symmetry that is not there.

## What the partner's own node has to run

For an **external** party the partner runs nothing. Their key is in their
wallet, they sign hashes, and the hosting nodes do the topology work.

For a **local** party they cannot avoid it. The conversion is authorized by
their participant's namespace key, that key lives inside their Canton node's
vault, and the only way to use it is `TopologyManagerWriteService.SignTransactions`
over their Admin API. So something with Admin API access has to issue the write.

**That something is DecMan, not a script.** The write is a versioned protobuf
`TopologyTransaction` that Canton itself generates, then co-signs, then accepts —
three round trips carrying binary payloads. `grpcurl` cannot realistically
assemble them, so "Admin API access plus a short script" is not a viable
substitute for running the binary, even briefly.

The minimum is therefore:

1. The partner runs DecMan against their participant's Admin API, long enough to
   perform the conversion. It needs no ledger credential and no IdP — the whole
   tenant path is tokenless on the Admin API.
2. They call `local-party/adopt-key/prepare`, sign the returned hash with the key
   they intend the party to answer to, and call `adopt-key/onboard`.
3. After that they can stop it. Adding hosts and replicating the ACS are driven
   by the wallet against the *hosting* nodes, not theirs.

There is no `decman-cli` path for this. The CLI is an HTTP client for a DecMan
API and holds no Canton client, so giving it one would mean a second
implementation of the topology write. Running DecMan briefly is cheaper and has
one code path.

## What can go wrong

| Symptom | Cause | Do |
|---|---|---|
| `409` from prepare or onboard | The party's serial moved between the wallet's read and the call | Re-read the party and retry with the new `base_serial` |
| `404` from any of these | This host does not hold the party | Check the host set; a joiner cannot serve the ACS export |
| `400` naming a field | The submitted bundle failed validation against the host's own head state | Do not retry as-is. Something built a different mapping than the host would |
| `marker_cleared: false` | The ACS imported but the flag has not cleared | Canton clears it past a safe time. Poll `/v0/tenant/{party}/status`; `InProgress` with the marker reason means wait |
| `package_preflight: false` | The source cannot read the party's contracts over the Ledger API, which is normal for an external party | Confirm the joiner has the party's DARs vetted **before** importing. Without it the import fails after disconnecting |
| Import fails mid-window | The joiner disconnected and the import did not complete | The import reconnects and verifies health on its own. Retry it; the durable marker makes re-entry safe |
| Joiner crash-loops after an import | Orphan ACS rows from an unclean shutdown | Manual repair. See `RepairCommitmentsUsingAcs` below |
| Export refused as too large | The snapshot exceeds this path's cap | Bounded by `DECPM_TENANT_ACS_MAX_BYTES` (512 MiB by default), not by the 16 MiB Noise limit. The export still assembles the whole snapshot in memory once before staging it, so raising this is a memory commitment |
| `400` naming an offset from import | The range's `offset` disagrees with what the joiner holds | Read `/v0/tenant/{party}/acs-progress` and resume from there. Do not retry the same range |
| `400` about `total_size` from import | The declared size exceeds this host's cap, or the range runs past it | The joiner cannot export to check the size itself, so it bounds what it is told. Check the source and the joiner agree on the same snapshot |

### Commitment mismatches after an import

If the joiner reports commitment mismatches once it starts confirming, its ACS
and its peers' view of it disagree. Canton's `RepairCommitmentsUsingAcs` takes
the imported ACS as the truth and recomputes from it. Run it on the joiner only,
and only after confirming the import itself completed — using it to paper over a
partial import replaces one inconsistency with a different one.

## What this does not do

- **Replicate a party onto a node outside the mesh without the wallet.** The
  wallet is the transport by design.
- **Stream the export itself.** The transfer is now ranged and resumable, and
  neither end holds the snapshot in a request body — but the source still
  assembles it whole in memory once before staging, so
  `DECPM_TENANT_ACS_MAX_BYTES` remains a memory commitment on the exporting node.
  Streaming Canton's export straight to disk would remove that.
- **Survive a terabyte-scale ACS.** At that size the wall is the import, not the
  transfer: Canton re-authenticates every contract while the joiner is
  disconnected, which no transport change touches. Sequencer retention and the
  node's volume size become the binding constraints, and shrinking the ACS beats
  moving it faster.
- **Prove a converted party can submit.** #388 proved Canton accepts the key.
  Whether the party then transacts with it is a different runtime path and is
  not yet covered by a test.
- **Undo a conversion.** Removing a signing key is a topology write nobody has
  built or tried here.
