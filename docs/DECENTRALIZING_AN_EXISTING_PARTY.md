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

| The namespace is | The party is | Path |
|---|---|---|
| Its own key, held by a wallet | External | [Add hosts](#adding-hosts-to-an-external-party) |
| A participant's root key | Local | [Convert first](#converting-a-local-party), then add hosts |
| A `DecentralizedNamespaceDefinition` | A decparty | Use the add-party workflow, not this |

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

`GET /v0/tenant/{party}/acs/{target}` on a host that already holds the party,
then `POST /v0/tenant/add-hosts/import` on the joiner. The wallet carries the
snapshot: there is no host-to-host channel, because a partner's node is
generally not in anyone else's mesh and the wallet already talks to all of them.

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
| Export refused as too large | The snapshot exceeds this path's cap | The wallet-relayed path is bounded by `DECPM_TENANT_ACS_MAX_BYTES` (512 MiB by default), not by the 16 MiB Noise limit. Raise it, remembering the snapshot is held in memory on both ends |

### Commitment mismatches after an import

If the joiner reports commitment mismatches once it starts confirming, its ACS
and its peers' view of it disagree. Canton's `RepairCommitmentsUsingAcs` takes
the imported ACS as the truth and recomputes from it. Run it on the joiner only,
and only after confirming the import itself completed — using it to paper over a
partial import replaces one inconsistency with a different one.

## What this does not do

- **Replicate a party onto a node outside the mesh without the wallet.** The
  wallet is the transport by design.
- **Stream an ACS.** The snapshot is assembled whole in memory on the exporting
  and importing nodes, so `DECPM_TENANT_ACS_MAX_BYTES` is a memory commitment
  rather than a transport limit. A genuinely streaming relay is still unbuilt.
- **Prove a converted party can submit.** #388 proved Canton accepts the key.
  Whether the party then transacts with it is a different runtime path and is
  not yet covered by a test.
- **Undo a conversion.** Removing a signing key is a topology write nobody has
  built or tried here.
