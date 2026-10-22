# Decentralizing an existing party

This guide adds hosts to a **local party** that already exists and holds
contracts. A local party lives on one participant, the owner node. Its
namespace is the owner node's key. The owner node signs the party's topology,
and applications submit for the party through that node.

After this guide, other participants host the party too. Two outcomes are
possible:

- **Failover.** Any host can submit for the party. Nothing changes in the
  party's application.
- **Co-validation.** The party gets its own signing key, and several hosts
  must confirm each transaction.

Other kinds of party use a different guide:

- A party with its own key, held by a wallet:
  [Decentralizing an external party](DECENTRALIZING_AN_EXTERNAL_PARTY.md).
- A decentralized party:
  [Adding a member](ADDING_A_MEMBER_TO_A_DECENTRALIZED_PARTY.md).

## Is the party local?

A party ID has two halves, `hint::namespace`. A participant ID has the same
shape. The party is local to a participant when the two namespaces are equal.
The **Configuration** page shows the node's **Participant ID**.

The owner node's `/state` call tells you whether the party has its own key yet:

```bash
curl -sS "$OWNER/v0/tenant/$PARTY/state" -H "Authorization: Bearer $OWNER_KEY"
# {"party_id": "...", "serial": 1, "threshold": 1, "host_count": 1,
#  "onboarding_hosts": 0, "has_signing_key": false}
```

`has_signing_key: false` means a plain local party. `true` means that someone
has already converted it.

## Choose failover or co-validation

| | Failover | Co-validation |
|---|---|---|
| The party's own key | None | A new Ed25519 key that the party owner holds |
| Permission of each host | Submission | Confirmation |
| Threshold | Stays 1. Each host acts alone | Can rise above 1. No single host acts alone |
| Change to the application | None | It must switch to interactive submission and sign with the new key |
| Steps | Add hosts, copy contracts | Convert, add hosts, copy contracts, raise the threshold |

**Choose failover when the application cannot change.**

Choose with care, because DecMan cannot move a party from failover to
co-validation yet. Canton refuses a Submission host above threshold 1, and
DecMan has no call that changes the permission of an existing host. Do not
convert a party that already has failover hosts.

## What never changes

The party ID includes the namespace, and the namespace never changes. So the
party keeps its ID and its contracts in both outcomes.

The owner node also keeps control of the party's topology for good. It can add
or remove hosts and change the threshold, on its own signature. After a
conversion, it can even remove the party's key and act as the party again.
Converting gives the party a signing key, not control of its own topology. Tell
the party owner this before you start.

## Before you start

1. **Every node runs DecMan with the tenant API on.** That covers the owner node
   and each joining node. Set `DECPM_TENANT_API_KEYS` on each one. You call
   every node's tenant API with its key.
2. **The joining nodes have every DAR the party uses.** DecMan cannot check
   this before the copy for a local party. Compare the vetted packages on each
   node's **Packages** page, and distribute what is missing.
3. **The application can pause.** See the warning below.
4. **You can take each joining participant offline for a while.** It leaves the
   synchronizer during its copy. Every party it hosts pauses for that time.
5. **For failover:** each joining participant needs a Ledger API user for the
   application, with the rights to act and read as the party. The joining
   node's operator grants them.
6. **For co-validation:** the party owner is ready to sign, and the application
   is ready for interactive submission.

> [!WARNING]
> **Keep the party idle from the add-hosts call until the last copy ends.** A
> joining node starts to receive the party's traffic as soon as the change takes
> effect. It leaves the synchronizer only when its copy starts. If the party
> archives an older contract in between, the joining node records an archive for
> a contract it never held. A replay of that record crashed a MainNet
> participant on 2026-09-16. So pause every application that acts as the party.

## The calls

All calls go to the tenant API and send the node's key as
`Authorization: Bearer <key>`. Every write answers `202 Accepted`. The change
takes effect a moment later, so poll the owner's `/state` after each one.

Each write pins `base_serial`, the serial you last read from `/state`. A node
whose serial has moved answers `409`. Then read `/state` again and repeat the
call.

| Call | Where | Signed by |
|---|---|---|
| `local-party/adopt-key/prepare` and `/execute` | The owner node | The owner node and the new party key |
| `add-hosts/authorize` | The owner node and each joining node | Each node, with its own key |
| `threshold/authorize` | The owner node only | The owner node |
| `{party}/acs/{joiner}` and `add-hosts/import` | The owner node, then the joining node | Nobody. This copies the contracts |

A local party takes `authorize`, not the wallet's `execute`. Its namespace key
sits in the owner node's Canton vault, and no wallet can sign with it. This
stays true after a conversion.

## Failover

1. **Read the serial** from the owner's `/state`. Call it `$SERIAL`.
2. **Authorize the add on the owner node and on each joining node,** with the
   same body:

   ```bash
   jq -n --arg p "$PARTY" --argjson s "$SERIAL" --argjson n "$NEW_HOSTS" \
     '{party_id:$p, permission:"submission", new_hosts:$n, base_serial:$s}' > add.json
   curl -sS -X POST "$NODE/v0/tenant/add-hosts/authorize" \
     -H "Authorization: Bearer $NODE_KEY" -H 'Content-Type: application/json' \
     -d @add.json
   ```

   `$NEW_HOSTS` is a JSON array of the joining participant IDs. The order of the
   calls does not matter. The change takes effect after the last one.

3. **Wait for the change.** Poll the owner's `/state` until `host_count`
   includes the joining nodes and `onboarding_hosts` counts them.
4. **Copy the contracts** from the owner node to each joining node. See
   [Copy the contracts](#copy-the-contracts).
5. **Wait for the markers to clear.** Poll the owner's `/state` until
   `onboarding_hosts` is 0. The application can run again.

The application now submits through any host, the normal way. Point it at a
second host to use the failover.

## Co-validation

1. **Make the party's key.** The party owner does this and keeps the private
   key. The Python helper in
   [Sign a hash](DECENTRALIZING_AN_EXTERNAL_PARTY.md#sign-a-hash) makes one:

   ```bash
   python3 key.py new party.key
   PUB=$(python3 key.py pub party.key | jq -r .public_key)
   ```

2. **Convert the party** on the owner node. Read `$SERIAL` from `/state`
   first.

   ```bash
   curl -sS -X POST "$OWNER/v0/tenant/local-party/adopt-key/prepare" \
     -H "Authorization: Bearer $OWNER_KEY" -H 'Content-Type: application/json' \
     -d "$(jq -n --arg p "$PARTY" --arg k "$PUB" --argjson s "$SERIAL" \
           '{party_id:$p, public_key:$k, base_serial:$s}')" > adopt.json
   python3 key.py sign party.key adopt.json > signed.json
   jq -n --arg p "$PARTY" --arg k "$PUB" --argjson s "$SERIAL" \
     --slurpfile a adopt.json --slurpfile g signed.json \
     '{party_id:$p, base_serial:$s, public_key:$k,
       topology_transactions:$a[0].topology_transactions,
       signatures:$g[0].signatures, signed_by:$g[0].signed_by}' > adopt-execute.json
   curl -sS -X POST "$OWNER/v0/tenant/local-party/adopt-key/execute" \
     -H "Authorization: Bearer $OWNER_KEY" -H 'Content-Type: application/json' \
     -d @adopt-execute.json
   ```

   Poll `/state` until `has_signing_key` is `true`. **From this moment the party
   must sign every submission with its key.** The owner node moves from
   Submission to Confirmation, so a normal submission for the party now fails.
   Plan the application's cutover for this moment.

3. **Add the hosts** as in [Failover](#failover) steps 1 to 5, with
   `"permission": "confirmation"`. Read the serial again first: the conversion
   moved it.
4. **Raise the threshold** on the owner node, after every marker has cleared.
   Read the serial again first. The add and each marker clear moved it, and
   the old value answers `409`.

   ```bash
   SERIAL=$(curl -sS "$OWNER/v0/tenant/$PARTY/state" \
     -H "Authorization: Bearer $OWNER_KEY" | jq .serial)
   curl -sS -X POST "$OWNER/v0/tenant/threshold/authorize" \
     -H "Authorization: Bearer $OWNER_KEY" -H 'Content-Type: application/json' \
     -d "$(jq -n --arg p "$PARTY" --argjson s "$SERIAL" \
           '{party_id:$p, new_threshold:2, base_serial:$s}')"
   ```

   Poll `/state` until `threshold` shows the new value.

The party now transacts with interactive submission through any host, signing
each transaction with its key. See
[Transact as the party](DECENTRALIZING_AN_EXTERNAL_PARTY.md#transact-as-the-party).

## Copy the contracts

Read block N from the owner node and hand it to the joining node, until the
import answers `"complete": true`. Do this once per joining node, one node at a
time.

```bash
seq=1
while :; do
  curl -sS -H "Authorization: Bearer $OWNER_KEY" \
    "$OWNER/v0/tenant/$PARTY/acs/$JOINER_ID?base_serial=$SERIAL&seq=$seq" > block.json
  jq --arg p "$PARTY" --argjson s "$SERIAL" \
    '{party_id:$p, base_serial:$s, seq, chunk, end, total_len, sha256, package_ids}' \
    block.json > import.json
  curl -sS -X POST "$JOINER/v0/tenant/add-hosts/import" \
    -H "Authorization: Bearer $JOINER_KEY" -H 'Content-Type: application/json' \
    -d @import.json > result.json
  jq -e .complete result.json > /dev/null && break
  seq=$((seq + 1))
done
```

`$SERIAL` is the serial that the add-hosts call pinned, not a later one. The
joining node leaves the synchronizer when block 1 arrives, and it reconnects
after the last block.
[How the copy works](DECENTRALIZING_AN_EXTERNAL_PARTY.md#how-the-copy-works)
explains restarts and time-outs.

## What the owner node has to run

The owner node signs with its participant's namespace key. That key lives in
the participant's Canton vault, and only the Admin API can use it. So the owner
node runs DecMan next to its participant, at least for the duration of this
work. DecMan needs no ledger credential for these calls.

A script cannot replace DecMan here. Each write is a versioned protobuf
transaction that Canton generates, then signs, then accepts. There is also no
`decman-cli` command for it.

## Leaving a party

A host can stop hosting a party on its own. It removes itself from the party's
hosting mapping and signs that change with its own key. The party's key is not
needed. Canton accepts the change when the threshold stays the same, no host
joins, and exactly one host leaves.

You start it in one of two ways:

- **From the UI.** An admin clicks **Stop hosting** on the party's row in the
  external-parties list. The dialog reads
  `GET /external-parties/{party_id}/unhost/preview` first. It shows the
  serial, the hosts that remain, and the Splice contracts that the node will
  archive. The confirm button calls `POST /external-parties/{party_id}/unhost`.
- **From the tenant API.** The wallet provider calls `/v0/tenant/unhost` on the
  host that leaves. `TenantClient::unhost` in `decman-wallet` makes this call.

```bash
curl -X POST "$HOST/v0/tenant/unhost" \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"party_id": "...", "participant_id": "<this host>", "base_serial": 7}'
```

### What the host checks first

The host checks its own head state before it writes anything. A failed check
changes nothing on the ledger or in the topology.

| Check | Answer when the check fails |
|---|---|
| The party's mapping lists this host | `404`. A repeat after a success answers `200` instead |
| `participant_id` is this host. Only the tenant call carries it | `400` |
| `base_serial` is the serial in head state | `409` |
| Another host remains | `400`. Canton refuses a mapping with no hosts |
| The hosts that remain can meet the threshold | `400`. The party's key must lower the threshold first |
| No ACS copy of the party is open on this host | `409` |
| No other unhost of the party runs on this host | `409` |
| The host can run the Splice cleanup, or the request sends `"splice_cleanup": false` | `422` |

A remaining host counts toward the threshold only when it has no onboarding
marker. It must also have Confirmation or Submission permission. On a 2-of-3
party one host may leave. On a 2-of-2 or 3-of-3 party no host may leave.

### What the host does

1. **Splice cleanup.** The host acts as its validator operator party, which
   `DECPM_VALIDATOR_OPERATOR_PARTY` names. It archives each `ValidatorRight`
   whose `user` is the party and whose `validator` is the operator. It cancels
   each `TransferPreapproval` whose `receiver` is the party and whose
   `provider` is the operator. It leaves an expired preapproval alone, because
   the DSO expires it. This step runs first, so a failure leaves the party
   fully hosted.
2. **Topology.** The host writes the mapping without itself at serial
   `base_serial + 1`, signed with its own key. It reads head state until the
   mapping no longer lists it. After about 20 seconds it answers `202` and
   keeps watching. Until the change takes effect, no ACS copy of the party may
   start on this host, and a repeat answers `409`. Once the host is gone, it
   closes the attempt's audit row as `completed`, and a repeat answers `200`.
3. **Audit.** The host writes one row per attempt to its own database.
   `GET /external-parties/{party_id}/unhost-audit` lists the rows, newest
   first. The **History** button on the party's row shows them too.

You can repeat every step safely. Suppose an attempt finished the cleanup and
then failed at the topology step. The next attempt skips the cleanup, as long as
the serial has not moved.

The host keeps working when the caller gives up. On a busy network the Splice
cleanup can take more than a minute, which is longer than the wallet client
waits. Repeat the call: it answers `409` while the first one still runs, and
`200` once the host is gone.

The host keeps the party's contracts in its store. DecMan has no call that
removes them yet.

## If something goes wrong

| Symptom | Cause | Do this |
|---|---|---|
| `409` | The party's serial changed after you read it | Read `/state` again and repeat the call |
| `400` "not local to this participant" from adopt-key | The call went to a node that does not own the party | Call the owner node |
| `400` "already carries party signing keys" | The party is already converted | Skip the conversion |
| `400` "this node neither owns … namespace nor is joining it" | The call went to an unrelated node | Call only the owner node and the joining nodes |
| `400` "a Submission host needs the party's threshold to be 1" | The party's threshold is above 1 | Use `"permission": "confirmation"` |
| `404` from `/status` on a failover host | `/status` checks Confirmation hosts only | Use the owner's `/state` instead |
| `500` "Imported the ACS, but could not request the marker clear" on the last block | Canton already has its own marker clear pending | Nothing. Poll `/state` until `onboarding_hosts` is 0 |
| `"package_preflight": false` in block 1 | The owner node cannot list the party's packages before the copy | Normal for a local party. Check the DARs yourself first |
| A normal submission for the party fails after the conversion | The party now signs for itself | Switch the application to interactive submission |
| `400` "the hosts that would remain" from unhost | The hosts that remain cannot meet the threshold | Lower the threshold with the party's key, then repeat the unhost |
| `400` "the last host" from unhost | This host is the party's only host | Add another host first |
| `409` "an ACS relay … is open" from unhost | A copy of the party's contracts runs on this host | Let the copy finish. An idle copy times out after 5 minutes |
| `409` "an unhost of … is already running" | An earlier unhost call for the party still runs, perhaps for a caller that gave up | Wait, then repeat the call |
| `409` "this host is leaving …" from an ACS block call | An unhost of the party runs on this host | Wait for the unhost to finish. The relay cannot start while the host leaves |
| `422` from unhost | The host has no validator operator party | Set `DECPM_VALIDATOR_OPERATOR_PARTY`. If the host never acted as the party's Splice validator, send `"splice_cleanup": false` |
| `202` from unhost | Canton accepted the topology write, but it is not effective yet | Repeat the call later. It answers `409` while the host waits for the change, and `200` once the host is gone |
| `500` from unhost | A Splice archive or the topology write failed | Read the reason in the unhost history, fix it, and repeat the call |

The copy fails the same way for every kind of party. See
[If something goes wrong](DECENTRALIZING_AN_EXTERNAL_PARTY.md#if-something-goes-wrong)
in the external-party guide for those errors, and for commitment mismatches
after an import.

## Limits

- **No undo of a conversion.** DecMan has no call that removes the party's key.
  The owner node's key could authorize it, but nobody has built or tested it.
- **One conversion per party.** A converted party cannot adopt a second key.
- **No change of namespace.** No sequence of calls turns a local party into a
  decentralized party.
