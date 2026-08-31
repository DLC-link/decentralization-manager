# Use Cases

Practical walkthroughs for the primary applications of the Decentralized Party Manager.

## Joint Custody Governance

The primary use case: several organizations jointly governing a shared decentralized party identity, where no single member can act alone.

### Scenario

Three custodial organizations (Custodian A, B, C) want to jointly govern a shared party. No single custodian should be able to unilaterally change the membership, onboard services, or issue tokens. All critical operations require a 2-of-3 majority.

### Initial Setup

**1. Create the decentralized party (3 participants):**

Each custodian runs a DecMan node connected to their Canton participant. The coordinator initiates onboarding:

```bash
curl -X POST http://custodian-a:8080/onboarding \
  -H "Content-Type: application/json" \
  -d '{
    "party_id_prefix": "joint-custody",
    "peer_ids": [
      "custodian-b::1220...",
      "custodian-c::1220..."
    ]
  }'
```

After all peers accept and the workflow completes, a decentralized party `joint-custody::1220...` exists with threshold 2.

**2. Deploy governance contracts:**

```bash
curl -X POST http://custodian-a:8080/contracts \
  -H "Content-Type: application/json" \
  -d '{
    "decentralized_party_id": "joint-custody::1220...",
    "participant_ids": ["a::1220...", "b::1220...", "c::1220..."],
    "participant_parties": ["member-a::1220...", "member-b::1220...", "member-c::1220..."],
    "operator_party": "operator::1220...",
    "dar_files": [
      { "filename": "governance-core.dar", "data": "<base64>" },
      { "filename": "governance-action.dar", "data": "<base64>" }
    ],
    "contracts": [
      {
        "id": "governance-rules",
        "name": "GovernanceRules",
        "package_id": "#governance-core-<version>",
        "module_name": "Governance.Rules",
        "entity_name": "GovernanceRules",
        "fields": [
          { "type": "decentralized_party" },
          { "type": "party_set", "parties": [] },
          { "type": "governance_threshold" },
          { "type": "rel_time", "microseconds": 86400000000 },
          { "type": "optional", "inner": { "type": "party_set", "parties": [] } }
        ]
      }
    ]
  }'
```

This deploys a `GovernanceRules` contract with all 3 members, threshold 2, and a 24-hour confirmation timeout.

### Full Deployment Flow

The complete end-to-end deployment follows these steps. Steps 6-9 are **domain proposals**: `POST /governance/propose`, then the same confirm -> threshold -> execute flow as any other proposal. `GovernanceRules` accepts only self-management actions on the inline `POST /governance/confirm` path (add/remove member, threshold, timeout, additional proposers) — every domain operation goes through a proposal.

> **Note:** `#governance-*-<version>` package IDs use `<version>` as a placeholder — substitute the version of the governance packages you deployed (these are configured per party via `PUT /party-config`).

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | Create decentralized party | DecMan (onboarding workflow) | Create the shared party identity |
| 2 | Configure party credentials | DecMan (`PUT /party-config` API) | Configure OAuth credentials (Keycloak or Auth0) and package IDs for each party |
| 3 | Grant Ledger API rights | External (Canton admin) | Grant `actAs`/`readAs` rights for member parties on the decentralized party |
| 4 | Upload DARs | DecMan (DARs workflow) | Upload DAR packages to all participant nodes |
| 5 | Deploy GovernanceRules | DecMan (contracts workflow) | Deploy `GovernanceRules` contract with package `#governance-core-<version>` |
| 6 | Create ProviderService | Governance proposal | `provision_provider_service` (or `create_provider_service_request`) |
| 7 | Create UserService | Governance proposal | `create_user_service_request` |
| 8 | Setup Utility | Governance proposal | `setup_utility` -- runs the Utility-Registry onboarding |
| 9 | Accept Free Credential | Governance proposal | `accept_free_credential` |

Steps marked "External" are performed outside the DecMan application (e.g., via Canton admin console or deployment tooling).

### Day-to-Day Operations

All governed operations follow the same flow: **Confirm -> Threshold Check -> Execute**. Membership and quorum changes are inline actions on `POST /governance/confirm`; everything else is a proposal on `POST /governance/propose`. See [Utility Services](#utility-services), [Token Custody](#token-custody) and [Generic Voting](#generic-voting) below for the concrete payloads.

### Membership Changes

#### Add a New Custodian

Adding a 4th custodian involves both governance and topology:

1. **Governance**: Propose `GovernanceAddMember` action via governance flow

    ```json
    {
      "action": {
        "type": "governance_add_member",
        "member": "new-member-d::1220...",
        "new_threshold": 3
      }
    }
    ```

2. **Topology**: Run the add-party workflow to add the new participant to the
   `DecentralizedNamespaceDefinition` and `PartyToParticipant` mappings:

    ```bash
    curl -X POST http://custodian-a:8080/add-party \
      -H "Content-Type: application/json" \
      -d '{
        "decentralized_party_id": "joint-custody::1220...",
        "new_participant_id": "custodian-d::1220...",
        "new_threshold": 3,
        "previous_threshold": 2
      }'
    ```

   If the party has active contracts, the workflow replicates them to the new
   custodian itself (ACS export/import around a brief synchronizer disconnect on
   the joining node only). No participant restart is needed.

#### Remove a Custodian

1. **Governance**: Propose `GovernanceRemoveMember`

    ```json
    {
      "action": {
        "type": "governance_remove_member",
        "member": "member-c::1220...",
        "new_threshold": 2
      }
    }
    ```

2. **Topology**: Run the kick workflow to remove the participant:

    ```bash
    curl -X POST http://custodian-a:8080/kick \
      -H "Content-Type: application/json" \
      -d '{
        "decentralized_party_id": "joint-custody::1220...",
        "participant_id": "custodian-c::1220...",
        "new_threshold": 2,
        "previous_threshold": 3
      }'
    ```

### Querying Governance State

**Get governance state:**
```bash
curl "http://localhost:8080/governance/state?party_id=joint-custody::1220..."
```

```json
{
  "state": {
    "contract_id": "00def...",
    "governance_party": "joint-custody::1220...",
    "members": ["member-a::1220...", "member-b::1220...", "member-c::1220..."],
    "threshold": 2,
    "action_confirmation_timeout_microseconds": 86400000000
  }
}
```

## Featured App Rewards (FAR)

FAR is a reward distribution mechanism for featured application participants in the Amulet ecosystem. It lets a decentralized party configure how rewards from its featured app right are distributed among beneficiaries.

### Beneficiary Structure

```json
{
  "beneficiaries": [
    { "beneficiary": "party-a::1220...", "weight": "0.50" },
    { "beneficiary": "party-b::1220...", "weight": "0.30" },
    { "beneficiary": "party-c::1220...", "weight": "0.20" }
  ]
}
```

- `beneficiaries`: List of parties and their reward weights (must sum to 1.0)
- `weight`: Decimal string representing the proportion of rewards

### Setting the Provider Reward Beneficiaries

The beneficiaries live on the party's `InstrumentConfiguration` and are set through the `set_provider_app_reward_beneficiaries` domain proposal:

```json
{
  "party_id": "joint-custody::1220...",
  "proposal": {
    "type": "set_provider_app_reward_beneficiaries",
    "instrument_configuration_cid": "<instrument-configuration-cid>",
    "provider_app_reward_beneficiaries": [
      { "beneficiary": "custodian-a::1220...", "weight": "0.50" },
      { "beneficiary": "custodian-b::1220...", "weight": "0.30" },
      { "beneficiary": "custodian-c::1220...", "weight": "0.20" }
    ]
  }
}
```

Omitting `provider_app_reward_beneficiaries` clears the current setting.

### DevNet Feature App Registration

Registering as a featured app in the Amulet ecosystem is the prerequisite for
holding the `FeaturedAppRight` contract that backs the reward beneficiaries
above.

> **Not currently submittable through DecMan.** `dev_net_feature_app` exists
> only as an inline `ActionType`, and the inline path now carries governance
> self-management actions only. There is no `GovernableAction` proposal for it
> yet, so register the featured app outside DecMan (Canton admin console /
> deployment tooling) until one is added.

## Multi-Signature Wallet

The Decentralized Party Manager can serve as the foundation for a custodial multi-signature wallet product.

### How DecParty Maps to Multi-Sig

| Multi-Sig Concept | DecParty Equivalent |
|-------------------|---------------------|
| N-of-M signers | N = threshold, M = number of members |
| Transaction proposal | Governance proposal (any `ProposalType`) |
| Signature collection | Confirmation flow (`ConfirmAction` calls) |
| Quorum reached | `can_execute: true` in confirmations response |
| Transaction execution | `ExecuteConfirmedAction` call |
| Add signer | `GovernanceAddMember` + onboarding workflow |
| Remove signer | `GovernanceRemoveMember` + kick workflow |
| Change quorum | `GovernanceSetThreshold` |

### Architecture Example

```
End Users (Mobile/Web)
        |
        v
┌─────────────────────────┐
│  Wallet Application     │
│  (Custom Frontend)      │
│  - Proposes actions     │
│  - Displays status      │
└───────────┬─────────────┘
            |
            v
┌─────────────────────────┐     ┌─────────────────────────┐
│  DecMan (Custodian A)   │<--->│  DecMan (Custodian B)   │
│  threshold = 2 of 3     │     │                         │
│  POST /governance/      │     │  POST /governance/      │
│       confirm           │     │       confirm           │
└───────────┬─────────────┘     └───────────┬─────────────┘
            |                               |
            v                               v
┌─────────────────────────────────────────────────────────┐
│              Canton Ledger (Shared State)                │
│  - GovernanceRules (threshold, members, timeout)         │
│  - GovernanceSelfConfirmation (per-action approvals)      │
│  - Domain contracts (assets under management)            │
└─────────────────────────────────────────────────────────┘
```

### Key Properties for Multi-Sig Wallets

- **No single point of failure**: The decentralized party has no central controller
- **Configurable quorum**: Set any threshold via `GovernanceSetThreshold`
- **Time-bound approvals**: Stale confirmations auto-expire via `actionConfirmationTimeout`
- **Auditable**: All confirmations and executions are recorded on the Canton ledger
- **Dynamic membership**: Add/remove signers without recreating the wallet

## Utility Services

DecMan supports onboarding to the Utility Registry, which provides provider and user service management.

### Full Onboarding Flow

The following sequence of governance proposals sets up a complete utility
service. Each one is submitted to `POST /governance/propose` and then follows
the usual propose -> confirm -> execute flow.

**1. Create ProviderService:**

```json
{
  "proposal": {
    "type": "provision_provider_service"
  }
}
```

`provision_provider_service` takes no fields — it creates the `ProviderService`
for the governance party itself. Use `create_provider_service_request` instead
when the provider is a different party:

```json
{
  "proposal": {
    "type": "create_provider_service_request",
    "operator": "operator::1220...",
    "provider": "provider-party::1220..."
  }
}
```

**2. Create UserService:**

```json
{
  "proposal": {
    "type": "create_user_service_request",
    "operator": "operator::1220...",
    "user": "user-party::1220..."
  }
}
```

**3. Link Services (Setup):**

After the provider service exists, run the onboarding:

```json
{
  "proposal": {
    "type": "setup_utility",
    "operator": "operator::1220...",
    "provider_service_cid": "<provider-service-contract-id>",
    "instrument_id_text": "<instrument-uuid>",
    "create_transfer_rule": true,
    "create_allocation_factory": true
  }
}
```

### Dual Governance Onboarding Flow

This flow splits the utility roles across two decentralized parties. The provider decparty owns the `ProviderService` and decides which registrars to accept. The registrar decparty owns the `RegistrarService` and manages its instruments and instrument issuers. Each decparty has its own `GovernanceRules` and members. Step 1 is the same proposal as the Full Onboarding Flow above. Steps 2-7 follow the same propose -> confirm -> execute flow as generic votes, voted on the named decparty.

**1. Create ProviderService (provider decparty):**

```json
{
  "proposal": {
    "type": "provision_provider_service"
  }
}
```

**2. Configure the provider (provider decparty):**

Record the credential requirements a registrar must satisfy:

```json
{
  "proposal": {
    "type": "create_provider_configuration",
    "provider_service_cid": "<provider-service-cid>",
    "registrar_requirements": [
      {
        "issuer": "provider-gov::1220...",
        "required_claims": [{ "property": "role", "value": "registrar" }]
      }
    ],
    "holder_requirements": []
  }
}
```

A requirement whose `issuer` is the provider decparty itself is minted automatically during onboarding and must name at least one claim.

**3. Request registrar service (registrar decparty):**

```json
{
  "proposal": {
    "type": "create_registrar_service_request",
    "operator": "operator::1220...",
    "provider": "provider-gov::1220...",
    "create_transfer_rule": true,
    "create_allocation_factory": true
  }
}
```

**4. Onboard the registrar (provider decparty):**

Mint the self-issuable registrar credentials and accept the request in one vote:

```json
{
  "proposal": {
    "type": "onboard_registrar",
    "provider_service_cid": "<provider-service-cid>",
    "registrar_service_request_cid": "<registrar-service-request-cid>",
    "provider_configuration_cid": "<provider-configuration-cid>"
  }
}
```

The accept validates every registrar requirement. The provider decparty mints only the credentials it can issue itself, so a requirement issued by another party rolls the whole action back. Execution creates the `RegistrarService`, plus the `TransferRule` and `AllocationFactory` the request asked for.

**5. Provision an instrument (registrar decparty):**

Create an `InstrumentConfiguration` and credential the initial issuers:

```json
{
  "proposal": {
    "type": "provision_instrument",
    "registrar_service_cid": "<registrar-service-cid>",
    "instrument_id_text": "LAUNCH-TOKEN",
    "additional_identifiers": [],
    "issuer_requirements": [
      {
        "issuer": "registrar-gov::1220...",
        "required_claims": [{ "property": "role", "value": "instrument-issuer" }]
      }
    ],
    "holder_requirements": [],
    "initial_instrument_issuers": ["issuer-1::1220..."]
  }
}
```

**6. Onboard instrument issuers (registrar decparty):**

Credential new issuers against the existing configuration whenever they join after provisioning:

```json
{
  "proposal": {
    "type": "onboard_instrument_issuers",
    "instrument_configuration_cid": "<instrument-configuration-cid>",
    "instrument_issuers": ["issuer-2::1220..."]
  }
}
```

**7. Offboard instrument issuers (registrar decparty):**

Revoke the credentials the registrar decparty self-issued for the offboarded issuers:

```json
{
  "proposal": {
    "type": "offboard_instrument_issuers",
    "instrument_issuers": [
      {
        "instrument_issuer": "<issuer-party-id>",
        "credential_cids": ["<credential-cid-1>", "<credential-cid-2>"]
      }
    ]
  }
}
```

Each row names one issuer and lists that issuer's credentials. The action checks that every claim on a credential names the declared issuer, so a mistyped row fails instead of revoking the wrong credential. The action revokes only the listed credentials. An issuer keeps minting rights through any live credential the proposal omits, so list every credential the registrar decparty issued for that issuer.

Credentialed issuers request mints and burns through the `AllocationFactory`. The registrar decparty approves each request with the `accept_mint_request` and `accept_burn_request` proposals.

### Querying Services

**List ProviderServices:**
```bash
curl "http://localhost:8080/services/provider?party_id=joint-custody::1220..."
```

```json
{
  "services": [
    {
      "contract_id": "00abc...",
      "operator": "operator::1220...",
      "provider": "joint-custody::1220..."
    }
  ]
}
```

**List UserServices:**
```bash
curl "http://localhost:8080/services/user?party_id=joint-custody::1220..."
```

```json
{
  "services": [
    {
      "contract_id": "00def...",
      "operator": "operator::1220...",
      "user": "joint-custody::1220..."
    }
  ]
}
```

### Credential Management

Issue and accept credentials through governance. Both are domain proposals on
`POST /governance/propose`:

**Offer a free credential:**

```json
{
  "proposal": {
    "type": "offer_free_credential",
    "user_service_cid": "<user-service-contract-id>",
    "holder": "holder-party::1220...",
    "id": "kyc-verified",
    "description": "KYC verification credential",
    "claims": [
      { "subject": "holder-party::1220...", "property": "kyc_status", "value": "verified" },
      { "subject": "holder-party::1220...", "property": "verification_date", "value": "2026-01-15" }
    ]
  }
}
```

**Accept a credential:**

```json
{
  "proposal": {
    "type": "accept_free_credential",
    "user_service_cid": "<user-service-contract-id>",
    "credential_offer_cid": "<credential-offer-contract-id>"
  }
}
```

All credential operations go through the same governance confirmation flow, requiring threshold approval from the decentralized party members.

## Generic Voting

The `GovernanceRules` contract supports free-text governance votes through the `GenericVoteProposal` template. Unlike token actions, a generic vote has no on-chain side effect -- the outcome is recorded solely as a `GovernanceExecutionResult` contract on the ledger.

This is useful for off-chain decisions (e.g., policy changes, operational approvals) where you want an auditable on-chain record of the vote without triggering any contract state change.

### Scenario

Three custodians want to formally vote on a policy change. The vote itself doesn't modify any contracts, but the decision should be permanently recorded on the Canton ledger.

### Step 1: Propose a Vote (Custodian A)

```bash
curl -X POST http://custodian-a:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "proposal": {
      "type": "generic_vote",
      "description": "Approve migration to new custody infrastructure by Q3 2026"
    }
  }'
```

The proposer (Custodian A) automatically receives one confirmation.

### Step 2: Check Pending Proposals

```bash
curl "http://custodian-a:8080/governance/confirmations?party_id=joint-custody::1220..."
```

```json
{
  "actions": [],
  "domain_actions": [
    {
      "proposal_cid": "00abc123...",
      "action_label": "GenericVote",
      "description": "Approve migration to new custody infrastructure by Q3 2026",
      "confirmations": [
        {
          "contract_id": "confirm-cid-1",
          "action": { "type": "governance_set_threshold", "new_threshold": 0 },
          "confirming_party": "member-a::1220..."
        }
      ],
      "confirmation_count": 1,
      "can_execute": false
    }
  ],
  "threshold": 2
}
```

### Step 3: Confirm the Vote (Custodian B)

```bash
curl -X POST http://custodian-b:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": { "type": "governance_set_threshold", "new_threshold": 0 },
    "governance_type": "core_domain",
    "proposal_cid": "00abc123..."
  }'
```

After Custodian B's confirmation, threshold (2) is met and `can_execute` becomes `true`.

### Step 4: Execute the Vote

```bash
curl -X POST http://custodian-a:8080/governance/execute \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": { "type": "governance_set_threshold", "new_threshold": 0 },
    "confirmation_cids": ["confirm-cid-1", "confirm-cid-2"],
    "disclosed_contracts": [],
    "governance_type": "core_domain",
    "proposal_cid": "00abc123..."
  }'
```

After execution:
- The `GenericVoteProposal` contract is archived
- All `GovernanceConfirmation` contracts are consumed
- A `GovernanceExecutionResult` is created with the vote description, confirmers, and timestamp as a permanent on-chain record

## Token Custody

The `governance-token-custody` package enables governance-controlled token operations. All token actions follow the same propose -> confirm -> execute flow as generic votes, but trigger real on-chain state changes when executed.

### Prerequisites

- `GovernanceRules` contract deployed (from `#governance-core-<version>`)
- `governance-token-custody` DAR uploaded to all participants (from `#governance-token-custody-<version>`)
- Token infrastructure deployed (transfer factories, instruments, etc.)

### Set Up Canton Coin Preapproval

Allows the governance party to receive Canton Coin transfers without per-transfer approval. This creates a `TransferPreapprovalProposal` that a provider must separately accept.

```bash
curl -X POST http://custodian-a:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "proposal": {
      "type": "setup_cc_preapproval",
      "provider": "provider-party::1220...",
      "expected_dso": "dso-party::1220..."
    }
  }'
```

### Set Up Utility Token Preapproval

Allows the governance party to receive utility token transfers. This creates a `TransferPreapproval` directly (no separate accept step).

```bash
curl -X POST http://custodian-a:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "proposal": {
      "type": "setup_token_preapproval",
      "operator": "operator::1220...",
      "instrument_admin": "registrar::1220...",
      "instrument_allowances": [{ "id": "TEST-TOKEN" }]
    }
  }'
```

Omit `instrument_allowances` or pass an empty array to preapprove all instruments from the admin.

### Transfer Tokens

Transfers tokens from the governance party to a receiver via a `TransferFactory`.

```bash
curl -X POST http://custodian-a:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "proposal": {
      "type": "transfer",
      "transfer_factory_cid": "<transfer-factory-cid>",
      "expected_admin": "registrar::1220...",
      "receiver": "recipient::1220...",
      "amount": "100.00",
      "instrument_id": { "admin": "registrar::1220...", "id": "TEST-TOKEN" },
      "input_holding_cids": ["<holding-cid-1>"]
    }
  }'
```

> **UTXO timing risk**: The holdings referenced by `input_holding_cids` are captured at proposal creation time. If those holdings are spent before the proposal is executed, the transfer will fail. Mitigations: use dedicated holdings, keep confirmation timeouts short, and re-propose if holdings change.

### Accept Incoming Transfer

Accepts a pending `TransferInstruction` directed at the governance party.

```bash
curl -X POST http://custodian-a:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "proposal": {
      "type": "accept_transfer",
      "transfer_instruction_cid": "<transfer-instruction-cid>"
    }
  }'
```

> **Timing risk**: The sender can withdraw the transfer instruction before governance approval completes, which would cause execution to fail with a contract-not-found error.

### Governance Self-Management

The `GovernanceRules` contract supports self-management actions (add/remove members, change threshold, change timeout, manage the additional-proposers allowlist) through the `core_self` governance type. These do not require proposals -- they use value-based matching rather than a proposal contract id.

**Add a new member:**

```bash
curl -X POST http://custodian-a:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "governance_add_member",
      "member": "new-member-d::1220...",
      "new_threshold": 3
    },
    "governance_type": "core_self"
  }'
```

**Change the threshold:**

```bash
curl -X POST http://custodian-a:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "governance_set_threshold",
      "new_threshold": 3
    },
    "governance_type": "core_self"
  }'
```

**Change the confirmation timeout:**

```bash
curl -X POST http://custodian-a:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "governance_set_timeout",
      "new_timeout_microseconds": 172800000000
    },
    "governance_type": "core_self"
  }'
```

**Grant propose-only rights to a non-member (`v1`+):**

```bash
curl -X POST http://custodian-a:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "governance_add_additional_proposer",
      "additional_proposer": "ops-console::1220..."
    },
    "governance_type": "core_self"
  }'
```

`governance_remove_additional_proposer` (with the same `additional_proposer` field) revokes the right and normalizes the `additionalProposers` allowlist back to `None` once it becomes empty. After execution the named party can call `POST /governance/propose` against this `GovernanceRules` without holding a member seat -- the on-chain proposer-authorization rule (member ∪ allowlist) accepts them at confirm time.

After threshold confirmations are collected, execute with:

```bash
curl -X POST http://custodian-a:8080/governance/execute \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-custody::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "governance_set_threshold",
      "new_threshold": 3
    },
    "confirmation_cids": ["<confirmation-cid-1>", "<confirmation-cid-2>"],
    "governance_type": "core_self"
  }'
```

Self-management execution returns a new `GovernanceRules` contract with the updated state.
