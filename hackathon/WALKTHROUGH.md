# Walkthrough: from zero to an executed governance action

About 30 minutes. You create a decentralized party across three Canton
participants, give it a governance contract, and run one action that needs two
of the three members to agree. Every step has a UI path and an API path.

Prerequisites: [README.md](README.md), and a stack that is up:

```bash
./hackathon/up.sh
```

Open the three UIs in three browser tabs. Each tab is one operator of the same
party, so keep them side by side:

- http://localhost:8081 — P1, the coordinator in this tour
- http://localhost:8082 — P2, the confirmer
- http://localhost:8083 — P3, the executor

There is no login. The nodes run with authentication disabled.

## 1. Look around (5 minutes)

In the sidebar of P1:

- **Configuration** shows this node's identity: its Canton participant ID, its
  Noise public key, and the two peers `up.sh` configured. The peer rows carry
  the address and the public key each node uses to reach the other two.
- **Parties** is empty. Nothing exists yet.
- **Packages** lists the Daml packages the participant has vetted.

The same data is on the API:

```bash
curl -s localhost:8081/node-config | jq '.node'
curl -s localhost:8081/network-config | jq
curl -s localhost:8081/participants-status | jq
```

`participants-status` is the health of the Noise mesh. You want `CurrentNode`
for this node and `Connected` for the other two. Nothing below works until all
three nodes report that.

## 2. Create the decentralized party (10 minutes)

A decentralized party is one Canton party whose namespace is owned by several
participants together. A threshold says how many owners must sign.

On **P1**, open **Parties** and press **Create Party**:

- **Party ID prefix**: `demo-party`
- **Threshold**: `2`
- Select both peers.
- Press **Start onboarding**.

P1 now asks the two peers to co-sign the new namespace. Each peer must accept.

On **P2** and **P3**, open **Approvals**. An Onboarding invitation is waiting.
Press **Accept** on both.

Watch P1: the workflow moves through its steps and finishes. The party appears
in **Parties** as `demo-party::1220...`. The hex part is the namespace
fingerprint, derived from the three owner keys.

Open the party. The **Participants** section lists the three participants that
host it.

The API path:

```bash
P2=$(curl -s localhost:8082/node-config | jq -r '.node.participant_id')
P3=$(curl -s localhost:8083/node-config | jq -r '.node.participant_id')

curl -s -X POST localhost:8081/onboarding \
  -H 'Content-Type: application/json' \
  -d "{\"party_id_prefix\": \"demo-party\", \"peer_ids\": [\"$P2\", \"$P3\"], \"threshold\": 2}"

# on each peer: read the invitation id, then accept it
curl -s localhost:8082/invitations | jq
curl -s -X POST localhost:8082/invitations/accept \
  -H 'Content-Type: application/json' -d '{"id": "<id>"}'

curl -s localhost:8081/onboarding/status | jq
curl -s localhost:8081/decentralized-parties | jq '.parties[].party_id'
```

Try the threshold: it is 2 of 3, so the party keeps working when one node is
down, but no single node can act alone.

## 3. Distribute the governance DARs (5 minutes)

The governance logic is Daml code. All three participants need the same
packages, and each must vet them.

On **P1**, open **Packages** and press **Distribute DARs**. Pick the five DARs
from `releases/v1/`:

- `governance-action-v1-0.1.0.dar` — the `GovernableAction` interface
- `governance-core-v1-0.1.0.dar` — the rules and the proposal templates
- `governance-token-custody-v1-0.1.0.dar`
- `governance-utility-onboarding-v1-0.4.0.dar`
- `governance-rewards-automation-v1-0.1.0.dar`

**P2** and **P3** get a Dars invitation in **Approvals**. Accept both. P1 uploads
each DAR to its own participant and sends it to the peers, and every node vets
what it receives. **Packages** on each node then lists the new packages.

The API path uploads base64 DAR bytes; `hackathon/seed.sh` shows how to build
that payload with `jq --rawfile`:

```bash
curl -s localhost:8081/packages/vetted | jq
```

## 4. Give the party an identity on each node (5 minutes)

This step is LocalNet-only plumbing. In a real deployment your identity
provider already holds these identities.

DecMan acts for the decentralized party through a **member party**: one local
Canton party per node, which signs on that node's behalf. On LocalNet no such
party exists, so allocate one on each participant's Canton JSON Ledger API and
let the ledger user act as it:

```bash
TOKEN=$(grep '^CANTON_TOKEN=' hackathon/lib.sh | cut -d'"' -f2)

# participant 1 is on 3975, participant 2 on 2975, participant 3 on 4975
curl -s -X POST localhost:3975/v2/parties \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"party_id_hint": "gov-member-p1", "local_metadata": {"annotations": {}}}' | jq -r '.partyDetails.party'
```

`hackathon/seed.sh` does this for all three nodes, grants `CanActAs` and
`CanReadAs` on both the member party and the decentralized party, and then
registers the pair on each node.

In the UI the registration is the **Party Configuration** dialog: it stores the
member party for this node and the package names the governance workflows use.
On LocalNet the user is `ledger-api-user` and the Keycloak fields stay empty.

Shortcut for this step and the two before it:

```bash
./hackathon/seed.sh
```

## 5. Deploy the governance core (2 minutes)

On **P1**, open the party, press **Deploy Contracts**, and in the dialog press
**Deploy Governance Core**. This creates one `GovernanceRules` contract, signed
by the decentralized party itself, so the peers must co-sign again: accept the
Contracts invitation on **P2** and **P3**.

The contract carries the governance member set, the confirmation threshold (2),
and how long a proposal stays open (30 minutes). The party's **Contracts**
section then shows it. That contract ID is what every later action refers to.

## 6. Run one governance action (5 minutes)

Now the point of the whole exercise: an action that no single node can take.

On **P1**, open the party and press **Governance Actions**, then **New
Proposal**. Set **Proposal Type** to **Generic Vote** — a motion with a text
description and no on-chain effect beyond the record — write a description, and
submit.

On **P2**, open **Approvals**. The proposal is there under Governance. Press
**Confirm**. That is two of three: the threshold is met.

On **P3**, open **Approvals**. The same proposal now offers **Execute**. Press
it. The action runs on the ledger, and the proposal leaves every node's pending
list.

The API path is three calls. Note the `action` field on confirm and execute: for
a domain proposal the endpoint requires a well-formed action object, but the
action that actually runs comes from `proposal_cid`, so a placeholder is normal
here. `hackathon/demo.sh` uses exactly these calls:

```bash
PARTY=$(curl -s localhost:8081/decentralized-parties | jq -r '.parties[0].party_id')
RULES=$(curl -s "localhost:8081/governance/state?party_id=$PARTY" | jq -r '.state.contract_id')

curl -s -X POST localhost:8081/governance/propose \
  -H 'Content-Type: application/json' \
  -d "{\"party_id\": \"$PARTY\", \"rules_contract_id\": \"$RULES\",
       \"proposal\": {\"type\": \"generic_vote\", \"description\": \"Ship it\"}}"

curl -s "localhost:8082/governance/confirmations?party_id=$PARTY" | jq '.domain_actions'
```

Try the negative case: propose again and execute with only one confirmation.
The ledger rejects it, because the threshold lives in the Daml contract, not in
DecMan.

## 7. Read the evidence (3 minutes)

Open the party on **P1** and expand **Audit Trail**. Each row is a ledger event
with its choice, its acting parties, and its update ID.

```bash
curl -s "localhost:8081/governance/chain-audit?party_id=$PARTY&limit=20&refresh=true" \
  | jq '[.entries[] | {event_type, update_id, offset, timestamp, acting_parties, contract_id}]'
```

You get four kinds of event: `propose`, `confirm`, `execute` and
`execute_result`. There are more `confirm` entries than you pressed buttons for:
the proposer's own confirmation counts toward the threshold, and one transaction
can leave more than one entry. `acting_parties` names who signed each event, and
`update_id` and `offset` locate the transaction on the ledger.

This is the proof a decentralized party leaves behind: every step is
attributable, and the execute exists only because two members agreed. The
default scope is the governance trail; pass `scope=all` to see every ledger
event the party witnesses.

Do the same call on P2 and P3. All three nodes read the same trail from their
own participant. No node is the source of truth.

```bash
./hackathon/demo.sh
```

runs section 6 and prints this evidence, if you would rather see it end to end
first and then repeat it by hand.

## Where to go next

- `http://localhost:8081/swagger-ui/` — every endpoint, with schemas.
- [../docs/USE_CASES.md](../docs/USE_CASES.md) — what people build on this.
- [../docs/CUSTOM_DAML_TEMPLATES.md](../docs/CUSTOM_DAML_TEMPLATES.md) — put your
  own Daml templates under governance, which is most likely what you want for a
  hackathon project.
- [../docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md) — how the workflows and the
  Noise peer protocol fit together.
- [README.md](README.md#what-localnet-is-not) — what LocalNet fakes, so you do
  not build on a shortcut.
