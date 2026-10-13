# Purging the leftover contracts of a former party

This guide removes stale contracts from a participant that no longer hosts a
decentralized party. Run it on the node that left the party, not on the
members that stay.

DecMan uses three terms here:

- **Former party:** a decentralized party that this participant hosted once
  and no longer hosts. The participant was kicked, or it removed its own
  hosting entry.
- **Leftover contract:** a contract that the participant still holds for a
  former party.
- **Purge:** the operation that deletes the leftover contracts from this
  participant.

The purge deletes ledger data, and nothing can bring a deleted contract back.
Read the whole guide before you start.

## Why the contracts go stale

A participant keeps every contract it held for a party when it stops hosting
that party. From then on, it no longer sees the party's transactions. The
remaining members archive contracts, but the archives never reach this
participant. Its copies stay active on it for good.

The stale copies cause two problems:

- **The ledger is wrong.** Queries on this participant return contracts that
  no longer exist on the remaining members.
- **ACS commitments disagree.** This participant and the remaining members
  exchange ACS commitments, which are hashes of the contracts they share. The
  stale copies keep those hashes different.

## What the purge deletes and keeps

The purge deletes a leftover contract only when no party that the participant
still hosts is a stakeholder of it. The participant's own admin party counts
as hosted.

The purge keeps every other contract. A contract that another local party sees
is still current, because the participant still receives its archive through
that party. Canton's party replication does not restore such a contract, so a
contract deleted by mistake does not come back.

The purge touches only the former party you choose. Contracts of other former
parties stay until you purge those parties too.

## Before you start

Check every item. DecMan refuses the purge when one of the first five fails.

1. **The removal is at least one minute old.** The participant must process
   every event up to its removal before it leaves the synchronizer.
2. **No workflow runs on the node.** The purge takes the participant off the
   synchronizer, so it waits until every workflow run has ended.
3. **The participant is connected and healthy.** The purge disconnects and
   reconnects it, so it starts only from a healthy connection.
4. **This node holds ledger credentials for the party.** DecMan reads the
   party's contracts with them. They stay stored after the removal.
5. **You have the admin role.** The preview, the purge and the reconnect all
   require it.
6. **The participant has caught up.** See [After downtime](#after-downtime).
7. **You can take the node offline.** See [Downtime](#downtime).

### After downtime

Do not purge right after the participant recovers from downtime. The one-minute
check counts wall-clock time, not how far the participant has processed. A
participant that is still catching up can have the archive of a stale contract
in its backlog.

If the purge deletes that contract, the participant later replays the archive.
The replay finds no contract and crashes the participant with exit code 117.

Wait until the participant has caught up with its synchronizer. Its ACS
commitments with the remaining members then cover recent periods again.

### Downtime

The participant leaves the synchronizer while the purge runs. **Every party on
that participant pauses**, not only the former party. A purge usually takes
about a minute. A party with many contracts takes longer.

DecMan reconnects the participant when the purge ends. It also does this when
the purge fails. A purge with nothing to delete never disconnects.

No workflow can start on the node while the purge runs:

- the operator cannot start a run or accept an invitation;
- a coordinator's retry of a failed run is refused.

Each of these succeeds again once the purge has ended.

## Run it in the UI

1. Open **Parties**. The former parties are at the bottom, under **No longer
   hosted by this node**. Each one has an orange border and a **Removed** chip.
2. Click the former party.
3. Open **Advanced**. The **Danger zone** card holds the **Purge** button.
4. Click **Purge**. The dialog counts the contracts that the purge deletes and
   the contracts that it keeps.
5. Read both counts. If the counts do not match what you expect, click
   **Cancel**.
6. Tick the box under the counts.
7. Type the party's name, for example `joint-custody`, into the text field.
8. Click **Purge N contracts**.

The page shows **Purging…** while the purge runs. It then shows **Purged**,
with how many contracts it deleted and how many it kept.

If the dialog says **Nothing to purge**, every leftover contract is shared with
a party that this node still hosts. Click **Mark as purged**. The participant
does not go offline.

After a successful purge, click **Dismiss** to hide the party from the list.

## Run it with the API

The operator API takes an operator token from your identity provider. The
caller needs the admin role when the node sets `DECPM_ADMIN_ROLE`. Run every
call against the node that left the party.

List the former parties:

```bash
curl -sS "$NODE/former-parties" -H "Authorization: Bearer $TOKEN" \
  | jq '.parties[] | {party_id, purge_status, purged_count, kept_count, purge_error}'
```

Count what a purge would delete and keep:

```bash
curl -sS "$NODE/former-parties/purge-preview?party_id=$PARTY" \
  -H "Authorization: Bearer $TOKEN"
```

Start the purge:

```bash
curl -sS -X POST "$NODE/former-parties/purge" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"party_id\": \"$PARTY\"}"
```

The answer is `202 Accepted`. The purge runs in the background. Poll
`GET /former-parties` until `purge_status` is `succeeded` or `failed`.

The API has no confirm step. Run the preview first, and check its counts.

Dismiss the party after a successful purge:

```bash
curl -sS -X DELETE "$NODE/former-parties?party_id=$PARTY" \
  -H "Authorization: Bearer $TOKEN"
```

## What the purge does

| # | Step | What happens |
|---|---|---|
| 1 | Check | Refuses unless every check in [Before you start](#before-you-start) passes |
| 2 | Plan | Reads the party's contracts and checks which stakeholders this participant still hosts |
| 3 | Disconnect | Takes the participant off the synchronizer. Its topology no longer changes |
| 4 | Check hosting | Reads the party's hosting again, and stops if this participant hosts it again |
| 5 | Purge | Deletes the planned contracts in batches of 500, with up to 3 attempts per batch |
| 6 | Reconnect | Reconnects the participant and waits until its connection is healthy |
| 7 | Verify | Reads the contracts again and confirms that none without a hosted stakeholder remain |

Step 6 runs on every outcome of steps 3 to 5. DecMan records the outcome after
step 7, or after the first step that fails.

## A party that comes back

DecMan forgets a former party when the participant hosts it again. If the
participant is then removed again, the party shows up again with a new
**Removed** chip. The participant holds new leftover contracts from its second
hosting period, so purge the party again.

## If something goes wrong

### DecMan refuses to start the purge

| Error says | Do this |
|---|---|
| "… is not a party this participant was removed from" | Nothing. The node hosts the party, or you dismissed it |
| "Workflows are running on this node (…)" | Wait for those runs to end, then purge |
| "The participant is not connected and healthy …" | Fix the connection first. After a failed purge, see [Reconnect the participant](#reconnect-the-participant) |
| "This participant hosts the party again …" | Nothing. The contracts are current again |
| "The removal took effect Ns ago …" | Wait until the removal is one minute old |
| "This node has no ledger credentials for …" | Store the party's credentials with `PUT /party-config`. The user needs read rights on the party |
| "A purge of this party is already running" | Wait for that purge to end |
| "Another purge or a workflow started on this node …" | Wait for it to end, then purge again |

### The purge fails

The page shows "The last purge failed: …" with the cause. DecMan tried to
reconnect the participant before it recorded the failure. Open **Advanced** and click
**Retry purge** once you have fixed the cause.

| Error says | Cause | Do this |
|---|---|---|
| "the participant did not come back connected and healthy …" | The reconnect failed | See [Reconnect the participant](#reconnect-the-participant) |
| "this participant hosts the party again, so the purge stopped …" | The party was hosted again during the purge | Nothing. The purge deleted nothing |
| "PurgeContracts failed on batch …" | Canton refused a batch three times | Click **Retry purge**. Contracts already deleted are skipped |
| "… contract(s) no hosted party sees are still active after the purge" | The check after the purge found leftovers | Click **Retry purge** |

### DecMan stopped during a purge

The purge runs inside the DecMan process. If DecMan stops during the purge, the
participant can stay off the synchronizer.

On its next start, DecMan marks that purge as failed. The error says "decman
restarted while this purge was running". DecMan does not start at all if it
cannot record this. A purge left as running would block every later purge and
every workflow.

DecMan does not reconnect the participant by itself. Check the participant,
then reconnect it as described below.

### Reconnect the participant

The page of a failed purge shows **Reconnect** while the participant is off the
synchronizer. Click it. DecMan reconnects the participant and waits until its
connection is healthy. Then retry the purge.

With the API:

```bash
curl -sS -X POST "$NODE/former-parties/reconnect" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"party_id\": \"$PARTY\"}"
```

DecMan refuses the reconnect in these cases:

| Error says | Do this |
|---|---|
| "Reconnect is for a purge that failed …" | Nothing. The reconnect applies only to a failed purge |
| "Workflows are running on this node (…)" | Wait for those runs to end |
| "A purge is running on this node …" | Wait. The purge reconnects the participant when it ends |
| "The participant is already connected and healthy …" | Nothing |
| "Synchronizer '…' is set to manual connection …" | **Do not reconnect.** See below |

**Manual connection means that DecMan keeps the participant off on purpose.** A
failed add-party run sets it, for example. A participant in that state can lack
a party's contracts, and a reconnect can crash it. Follow
[Adding a member](ADDING_A_MEMBER_TO_A_DECENTRALIZED_PARTY.md#the-run-fails)
instead. The purge itself never sets manual connection.

Without DecMan, two other ways reconnect the participant. Use them only after a
purge, and only when manual connection is off:

- Restart the participant. It reconnects to every synchronizer that has manual
  connection off.
- Call the participant's Admin API. Use the alias from
  `DECPM_CANTON_SYNCHRONIZER`, and add TLS flags if your Admin API uses TLS:

  ```bash
  grpcurl -plaintext -d '{"synchronizer_alias": "global", "retry": true}' \
    "$CANTON_ADMIN_HOST:$CANTON_ADMIN_PORT" \
    com.digitalasset.canton.admin.participant.v30.SynchronizerConnectivityService/ReconnectSynchronizer
  ```

If the participant crashes with exit code 117 after it reconnects, it replayed
the archive of a contract that the purge deleted. See
[After downtime](#after-downtime). Contact the BitSafe team before you restart
it again.
