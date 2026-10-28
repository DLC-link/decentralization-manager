# Adding a member to a decentralized party

This guide adds a new participant to a decentralized party that already exists.
The new participant becomes a namespace owner and a host of the party. DecMan
then copies the party's contracts to it.

DecMan calls this the add-party workflow. The UI calls it **Add Member**.

- **Coordinator:** the existing member that starts the run.
- **Members:** the participants that own and host the party today.
- **New member:** the participant that joins.

Every member and the new member take part in the run. Each of them accepts an
invitation on its own DecMan node.

For a party that is not a decentralized party, use a different guide:

- A party with its own key, held by a wallet:
  [Decentralizing an external party](DECENTRALIZING_AN_EXTERNAL_PARTY.md).
- A party local to one participant:
  [Decentralizing an existing party](DECENTRALIZING_AN_EXISTING_PARTY.md).

## Before you start

Check every item. DecMan refuses the run, or the run fails partway, when one is
missing.

1. **The new member runs DecMan.** It runs the same release as the members, and
   it connects to its own participant. DecMan refuses to invite a node that runs
   an older protocol version.
2. **The nodes know each other as peers.** The coordinator and the new member
   must list each other. Every member must list the coordinator. Set up the full
   mesh anyway: a later run can have a different coordinator. See
   [Exchange peer details](#exchange-peer-details).
3. **The new member has every DAR the party uses.** DecMan checks this before it
   writes any topology. See [Check the new member's DARs](#check-the-new-members-dars).
4. **The coordinator can read the party's contracts.** The DAR check reads them
   over the Ledger API, with the party credentials stored on the coordinator.
   Open the party page and confirm that **Test Auth** succeeds.
5. **No other topology workflow runs for this party.** DecMan runs one kick,
   add-party or change-threshold run per party at a time.
6. **You can take the new member offline for a while.** Its participant leaves
   the synchronizer during the run. See [Downtime](#downtime).

If the new member was a member before, read
[Re-adding a former member](#re-adding-a-former-member) first.

### Exchange peer details

Do this on the coordinator and on the new member.

1. Open **Configuration**.
2. Under **Peers:**, click **Share my data**. DecMan copies one CSV line for
   this node: `participant_id,name,address,port,public_key`.
3. Send that line to the operator of the other node.
4. On the other node, click the pencil icon (**Edit peers**), then
   **Paste from Clipboard**, then **Save**.

To send many peers at once, use **Export CSV** and **Import CSV**.

The status dot of each peer then reads "Connected via Noise protocol". A dot
that reads "Noise handshake failed" means that a public key is wrong.

### Check the new member's DARs

1. Open the party page and click **Check DARs**. The **Packages** page opens
   and compares the party's hosts.
2. Add the new member in the **Participants** box. It is not a host yet, so
   DecMan does not add it for you.
3. Turn on **Differences only**. A red cell with an error icon marks a package
   that the new member lacks.
4. Click the send icon in that cell to distribute the DAR. The new member's
   operator accepts it under **Approvals**. The send icon shows only in a UI
   built with admin access.

### Downtime

The new member's participant disconnects from the synchronizer when it signs
the proposals. It reconnects after the contract import ends. While it is
disconnected, **every party on that participant pauses**, not only this one.

The members are not affected. The party keeps transacting on them for the
whole run.

The time offline depends on two things:

- How fast the other members sign.
- How many contracts the party holds.

So ask the members to accept the invitation before you start.

## Choose the new threshold

The threshold is how many owners must sign a topology change for the party.
The confirmation threshold is how many hosts must confirm a transaction. If the
two are equal, the add moves both to the new value. If they differ, the
confirmation threshold stays as it is.

The new threshold must be between 1 and the **current** number of owners. The
new member does not count yet, because it cannot sign until its import
completes. The dialog suggests a majority of the owners after the add.

To count the new member, run **Change Threshold** after this run completes.

## Run it in the UI

On the coordinator:

1. Open **Parties** and click the party.
2. In the **Participants** section, click the **Add member** icon.
3. In **Participant to Add**, pick the new member. The list holds the peers that
   do not host the party yet.
4. Set **New Threshold**.
5. Click **Add Member**.

DecMan sends an invitation to every member and to the new member.

On every invited node:

1. Open **Approvals**. The invitation is under **Needs your action**.
2. Click **Review ▾**. Check the **Dec party**, the participant under
   **Adding**, and the **Threshold**.
3. Click **Accept**.

The run starts when every invited node has accepted. The coordinator shows the
run as a card under **Approvals**, with one dot per step. During the copy, the
card shows a meter: "Sending ACS" on the coordinator, "Importing ACS" on the
new member.

## Run it with the API

The operator API takes an operator token from your identity provider. The
caller needs the admin role when the node sets `DECPM_ADMIN_ROLE`.

Start the run on the coordinator:

```bash
curl -sS -X POST "$COORDINATOR/add-party" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "decentralized_party_id": "joint-custody::1220...",
    "new_participant_id": "custodian-d::1220...",
    "new_threshold": 2,
    "previous_threshold": 2
  }'
```

`previous_threshold` is for display only. The answer is `202 Accepted` with the
run's `instance_name`.

Accept the invitation on every invited node:

```bash
ID=$(curl -sS "$NODE/invitations" -H "Authorization: Bearer $TOKEN" \
  | jq -r '.invitations[] | select(.invitation_type == "AddParty") | .id')
curl -sS -X POST "$NODE/invitations/accept" \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"id\": \"$ID\"}"
```

Follow the run on the coordinator:

```bash
curl -sS "$COORDINATOR/workflows" -H "Authorization: Bearer $TOKEN" \
  | jq '.runs[] | select(.instance_name | contains("-add-party-"))
        | {instance_name, status, current_step, step_index, step_total, error}'
```

## What the run does

| # | Step | Who acts | What happens |
|---|---|---|---|
| 1 | WaitingForPeers | Coordinator | Waits until every invited node has accepted and connected |
| 2 | GenerateNewMemberKeys | New member | Creates its namespace key and its Daml signing key in its participant |
| 3 | ExportState | Coordinator | Reads the party's topology, checks the threshold, and checks the new member's packages |
| 4 | CreateProposals | Coordinator | Builds the new namespace and hosting mappings. The new member gets Canton's onboarding marker |
| 5 | SignProposals | All | Every node signs. The new member then disconnects from the synchronizer |
| 6 | SubmitProposals | Coordinator | Submits the mappings and opens the export of the party's contracts |
| 7 | SyncAcs | New member | Imports the contracts block by block, then reconnects |
| 8–12 | Clear the onboarding marker | All | The new member proposes to clear the marker, every node signs, and the coordinator submits |
| 13 | RetireLegacyKeys | All | Removes the party's old key mapping, if it has one |
| 14 | Complete | All | Ends the run |

While the onboarding marker is set, the new member holds the party but does not
confirm for it. The marker clears in steps 8 to 12.

The coordinator UI may show these steps under their code names, for example
"Export state". [Architecture](ARCHITECTURE.md#add-party-add-a-host-to-an-existing-decentralized-party)
lists the Canton calls behind each step.

## After the run

1. **Check the party page.** The new member is in **Participants**. **Owners**
   has grown by one.
2. **Give the new member credentials.** On the new member's node, open the
   party, click **Login** under **Authentication**, and enter the identity
   provider fields. Click **Discover Member Party**, then **Save**, then
   **Test Auth**. If rights are missing, click **Grant Rights**.
3. **Add the new member to governance.** The topology change does not touch the
   party's `GovernanceRules` contract. If the party has one, propose a
   governance add-member action. See
   [Use Cases](USE_CASES.md#add-a-new-custodian).
4. **Raise the threshold if you want.** Run **Change Threshold** to count the
   new member.

## Re-adding a former member

A participant that hosted the party before can still own its namespace. For
example, it removed its own hosting entry but stayed an owner. DecMan detects
this case and adds the hosting entry only. The namespace stays as it is.

In this case, the new threshold must equal the current threshold. Change it
afterwards with **Change Threshold**.

Treat a participant that left the party after a failed run as a recovery case.
Two rules apply:

- **Do not reconnect it while it lacks the party's contracts.** On reconnect,
  it replays every event it missed. It judges each event by the topology at
  that event's time, so it acts as a host again and crashes.
- **Purge only what the party alone holds.** Before the re-add, the participant
  purges the party's old contracts. Keep every contract that another party on
  that participant is also a stakeholder of. The copy in step 7 leaves those
  out, so a contract purged by mistake never comes back.

Contact the BitSafe team before you recover a participant in this state.

## If something goes wrong

### DecMan refuses to start the run

| Error says | Do this |
|---|---|
| "is not a configured peer of this node" | Add the new member as a peer on the coordinator |
| "No cached membership for …" | Open **Parties** and search for the party, to refresh the list. Then retry |
| "is already a member of …" | Nothing. The participant already hosts the party |
| "This node is not a member of …" | Start the run from a node that hosts the party |
| "new_threshold must be between 1 and N" | Pick a value up to the current number of owners |
| "already has a … workflow in flight" | Wait for that run to end, or cancel it |
| A list of incompatible peers | Upgrade those nodes to the same release |

### The run fails

The coordinator's card shows the error. Click **Retry** on the coordinator. The
run continues from the step that failed. **Do not cancel a failed run.** A
cancel does not reconnect the new member, and it throws away the state that a
retry needs.

| Error says | Cause | Do this |
|---|---|---|
| "the new member … has not vetted N package(s)" | The new member has not uploaded or not vetted a DAR | Distribute the DAR, then click **Retry** |
| "Re-hosting … leaves the namespace as it is" | A former member was re-added with a new threshold | Start a new run with the current threshold |
| "… is quarantined for …" | An earlier copy failed after part of the contracts reached Canton | See [Quarantine](#quarantine) |
| A timeout in `RetireLegacyKeys` | Too few upgraded owners are online | Bring the owners online, then click **Retry** |
| "peer(s) are marked complete but have no … artefact" | The coordinator restarted during a hand-off and lost signatures | Start a new run |

**Do not reconnect the new member's participant by hand.** If a run fails after
step 5, the participant stays off the synchronizer on purpose. DecMan also sets
its synchronizer to manual connection, so a restart does not reconnect it.
Retry the run instead.

A connected participant that hosts the party without its contracts receives
the party's traffic. That state crashed a MainNet participant on 2026-09-16.
See [Re-adding a former member](#re-adding-a-former-member) for the rules that
follow from it.

### Quarantine

A copy can fail after some contracts already reached the new member's Canton.
Canton cannot report how many arrived. DecMan then quarantines the new member
for this party and refuses every later copy onto it.

1. Repair the participant with Canton's `RepairCommitmentsUsingAcs`, or restore
   it from a backup.
2. Lift the quarantine on the new member's DecMan node, which ran the import:

   ```bash
   curl -sS -X DELETE \
     "$NEW_MEMBER_NODE/acs-import-quarantine?party_id=$PARTY&participant_id=$NEW_MEMBER" \
     -H "Authorization: Bearer $TOKEN"
   ```

3. Start a new add-party run.

The `DELETE` call records your decision only. It repairs nothing.
