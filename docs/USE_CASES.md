# Use Cases

Practical walkthroughs for the primary applications of the Decentralized Party Manager.

## Vault Governance

The primary use case: multiple custodians jointly managing BitsafeVault contracts through a shared decentralized party identity.

### Scenario

Three custodial organizations (Custodian A, B, C) want to jointly manage a digital asset vault. No single custodian should be able to unilaterally deploy, pause, or modify the vault. All critical operations require a 2-of-3 majority.

### Initial Setup

**1. Create the decentralized party (3 participants):**

Each custodian runs a DPM node connected to their Canton participant. The coordinator initiates onboarding:

```bash
curl -X POST http://custodian-a:8080/onboarding \
  -H "Content-Type: application/json" \
  -d '{
    "party_id_prefix": "joint-vault",
    "peer_ids": [
      "custodian-b::1220...",
      "custodian-c::1220..."
    ]
  }'
```

After all peers accept and the workflow completes, a decentralized party `joint-vault::1220...` exists with threshold 2.

**2. Deploy governance contracts:**

```bash
curl -X POST http://custodian-a:8080/contracts \
  -H "Content-Type: application/json" \
  -d '{
    "decentralized_party_id": "joint-vault::1220...",
    "participant_ids": ["a::1220...", "b::1220...", "c::1220..."],
    "participant_parties": ["member-a::1220...", "member-b::1220...", "member-c::1220..."],
    "operator_party": "operator::1220...",
    "dar_files": [
      { "filename": "vault-governance.dar", "data": "<base64>" },
      { "filename": "vault.dar", "data": "<base64>" }
    ],
    "contracts": [
      {
        "id": "governance-rules",
        "name": "VaultGovernanceRules",
        "package_id": "#bitsafe-vault-governance-v0-rc8",
        "module_name": "BitsafeVault.VaultGovernance",
        "entity_name": "VaultGovernanceRules",
        "fields": [
          { "type": "decentralized_party" },
          { "type": "attestors_set" },
          { "type": "governance_threshold" },
          { "type": "optional", "inner": { "type": "rel_time", "microseconds": 86400000000 } }
        ]
      }
    ]
  }'
```

This deploys a `VaultGovernanceRules` contract with all 3 members, threshold 2, and a 24-hour confirmation timeout.

> **Note:** New deployments should use `GovernanceRules` (from `#governance-core-v0-rc4`) instead. See the [Integration Guide](INTEGRATION_GUIDE.md#deploying-governance-contracts) for the recommended contract deployment payload.

### Full Deployment Flow

The complete end-to-end deployment of a vault system follows these steps. Each governance action (steps 6-15) requires threshold confirmations before execution. Steps 5a/5b show the two governance contract options.

| # | Step | Actor | Description |
|---|------|-------|-------------|
| 1 | Create decentralized party | DPM (onboarding workflow) | Create the shared party identity |
| 2 | Configure party credentials | DPM (`PUT /party-config` API) | Configure Keycloak credentials and package IDs for each party |
| 3 | Grant Ledger API rights | External (Canton admin) | Grant `actAs`/`readAs` rights for member parties on the decentralized party |
| 4 | Upload DARs | DPM (DARs workflow) | Upload DAR packages to all participant nodes |
| 5a | Deploy GovernanceRules | DPM (contracts workflow) | Deploy `GovernanceRules` contract with package `#governance-core-v0-rc4` (recommended) |
| 5b | Deploy VaultGovernance | DPM (contracts workflow) | Deploy `VaultGovernanceRules` contract with package `#bitsafe-vault-governance-v0-rc8` (legacy) |
| 6 | Create ProviderService | Governance action | `utility_create_provider_request` |
| 7 | Create UserService | Governance action | `utility_create_user_request` |
| 8 | Setup Utility | Governance action | `utility_setup` -- links provider and user services |
| 9 | Request DevNet FAR | Governance action | `dev_net_feature_app` -- register as featured app |
| 10 | Add VaultManager | External (Canton admin) | Grant VaultManager role to the decentralized party |
| 11 | Deploy Vault | Governance action | `vault_deployment` -- add member parties as beneficiaries |
| 12 | Deploy YieldEpoch | Governance action | `yield_epoch_deployment` |
| 13 | Request Processor | Governance action | `processor_deployment_request` -- same beneficiaries as vault |
| 14 | Accept Processor | External (Canton admin) | Accept the processor deployment |
| 15 | Accept Free Credential | Governance action | `credential_accept_free` |

Steps marked "External" are performed outside the DPM application (e.g., via Canton admin console or deployment tooling).

### Day-to-Day Operations

All vault operations follow the same governance flow: **Confirm -> Threshold Check -> Execute**.

#### Deploy a New Vault

**Step 1: Custodian A proposes a vault deployment:**

```bash
curl -X POST http://custodian-a:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-vault::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "vault_deployment",
      "vault_rules_cid": "<vault-rules-cid>",
      "vault_name": "BTC Custody Vault",
      "share_symbol": "vBTC",
      "asset_instrument_id": {
        "admin": "operator::1220...",
        "id": "btc-instrument"
      },
      "limits": {
        "max_total_deposit": "1000000.00",
        "min_deposit_amount": "0.001",
        "min_withdrawal_amount": "0.001"
      },
      "vault_backend_signatory": "backend::1220...",
      "vault_far_config": null,
      "allocation_factory_cid": "<allocation-factory-cid>",
      "registrar_service_cid": "<registrar-service-cid>"
    }
  }'
```

**Step 2: Custodian B confirms (reaching threshold):**

Same request from Custodian B's node. After 2 confirmations, the action can be executed.

**Step 3: Check confirmation status:**

```bash
curl "http://custodian-a:8080/governance/confirmations?party_id=joint-vault::1220..."
```

```json
{
  "actions": [
    {
      "action_hash": "abc123...",
      "action": {
        "type": "vault_deployment",
        "vault_name": "BTC Custody Vault",
        ...
      },
      "confirmations": [
        { "contract_id": "confirm-cid-1", "confirming_party": "member-a::1220..." },
        { "contract_id": "confirm-cid-2", "confirming_party": "member-b::1220..." }
      ],
      "confirmation_count": 2,
      "can_execute": true
    }
  ],
  "threshold": 2
}
```

**Step 4: Execute the deployment:**

```bash
curl -X POST http://custodian-a:8080/governance/execute \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-vault::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "vault_deployment",
      "vault_rules_cid": "<vault-rules-cid>",
      "vault_name": "BTC Custody Vault",
      "share_symbol": "vBTC",
      "asset_instrument_id": { "admin": "operator::1220...", "id": "btc-instrument" },
      "limits": {
        "max_total_deposit": "1000000.00",
        "min_deposit_amount": "0.001",
        "min_withdrawal_amount": "0.001"
      },
      "vault_backend_signatory": "backend::1220...",
      "vault_far_config": null,
      "allocation_factory_cid": "<allocation-factory-cid>",
      "registrar_service_cid": "<registrar-service-cid>"
    },
    "confirmation_cids": ["confirm-cid-1", "confirm-cid-2"]
  }'
```

#### Pause / Unpause a Vault

```json
{
  "action": { "type": "vault_pause", "vault_id": "<vault-contract-id>" }
}
```

```json
{
  "action": { "type": "vault_unpause", "vault_id": "<vault-contract-id>" }
}
```

#### Update Vault Limits

```json
{
  "action": {
    "type": "vault_update_limits",
    "vault_id": "<vault-contract-id>",
    "new_limits": {
      "max_total_deposit": "5000000.00",
      "min_deposit_amount": "0.01",
      "min_withdrawal_amount": "0.01"
    }
  }
}
```

#### Change Backend Signatory

```json
{
  "action": {
    "type": "vault_update_backend",
    "vault_id": "<vault-contract-id>",
    "new_backend_signatory": "new-backend::1220..."
  }
}
```

#### Deploy YieldEpoch

```json
{
  "action": {
    "type": "yield_epoch_deployment",
    "vault_rules_cid": "<vault-rules-cid>",
    "vault_cid": "<vault-contract-id>",
    "asset_instrument_id": { "admin": "operator::1220...", "id": "btc-instrument" },
    "vault_backend_signatory": "backend::1220..."
  }
}
```

#### Request Processor Deployment

```json
{
  "action": {
    "type": "processor_deployment_request",
    "vault_processor_rules_cid": "<vault-processor-rules-cid>",
    "vault_backend_signatory": "backend::1220...",
    "allocation_factory_cid": "<allocation-factory-cid>",
    "processor_far_config": null,
    "initial_supported_vaults": ["<vault-contract-id>"]
  }
}
```

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

2. **Topology**: Run the onboarding workflow to add the new participant to the `PartyToParticipant` mapping. If the party has active contracts, ACS sync (export/import) will be required.

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
        "decentralized_party_id": "joint-vault::1220...",
        "participant_id": "custodian-c::1220...",
        "namespace_fingerprint": "1220...",
        "new_threshold": 2
      }'
    ```

### Querying Vault State

**List deployed vaults:**
```bash
curl "http://localhost:8080/vaults?party_id=joint-vault::1220..."
```

```json
{
  "vaults": [
    {
      "contract_id": "00abc...",
      "vault_name": "BTC Custody Vault",
      "share_symbol": "vBTC",
      "is_paused": false,
      "vault_manager": "joint-vault::1220..."
    }
  ]
}
```

**Get governance state:**
```bash
curl "http://localhost:8080/governance/state?party_id=joint-vault::1220..."
```

```json
{
  "state": {
    "contract_id": "00def...",
    "vault_manager": "joint-vault::1220...",
    "members": ["member-a::1220...", "member-b::1220...", "member-c::1220..."],
    "threshold": 2,
    "action_confirmation_timeout_microseconds": 86400000000
  }
}
```

## Featured App Rewards (FAR)

FAR is a reward distribution mechanism for featured application participants in the Amulet ecosystem. It allows vault operators to configure how rewards from featured app rights are distributed among beneficiaries.

### FarConfig Structure

```json
{
  "featured_app_right_cid": "<featured-app-right-contract-id>",
  "beneficiaries": [
    { "beneficiary": "party-a::1220...", "weight": "0.50" },
    { "beneficiary": "party-b::1220...", "weight": "0.30" },
    { "beneficiary": "party-c::1220...", "weight": "0.20" }
  ]
}
```

- `featured_app_right_cid`: Contract ID of the `FeaturedAppRight` contract (from the Amulet ecosystem)
- `beneficiaries`: List of parties and their reward weights (must sum to 1.0)
- `weight`: Decimal string representing the proportion of rewards

### Where FAR Is Used

| Action | Purpose |
|--------|---------|
| `VaultDeployment` | Set initial FAR config when deploying a vault (`vault_far_config` field) |
| `ProcessorDeploymentRequest` | Set FAR config for processor rewards (`processor_far_config` field) |
| `VaultUpdateFarBeneficiaries` | Update beneficiary weights for an existing vault |

### Setting Initial FAR on Vault Deployment

```json
{
  "action": {
    "type": "vault_deployment",
    "vault_rules_cid": "...",
    "vault_name": "BTC Vault",
    "share_symbol": "vBTC",
    "asset_instrument_id": { "admin": "...", "id": "btc" },
    "limits": { "min_deposit_amount": "0.001" },
    "vault_backend_signatory": "backend::1220...",
    "vault_far_config": {
      "featured_app_right_cid": "00abc...",
      "beneficiaries": [
        { "beneficiary": "custodian-a::1220...", "weight": "0.50" },
        { "beneficiary": "custodian-b::1220...", "weight": "0.30" },
        { "beneficiary": "custodian-c::1220...", "weight": "0.20" }
      ]
    },
    "allocation_factory_cid": "<allocation-factory-cid>",
    "registrar_service_cid": "<registrar-service-cid>"
  }
}
```

### Updating FAR Beneficiaries

```json
{
  "action": {
    "type": "vault_update_far_beneficiaries",
    "vault_id": "<vault-contract-id>",
    "new_beneficiaries": [
      { "beneficiary": "custodian-a::1220...", "weight": "0.40" },
      { "beneficiary": "custodian-b::1220...", "weight": "0.40" },
      { "beneficiary": "custodian-c::1220...", "weight": "0.20" }
    ]
  }
}
```

### DevNet Feature App Registration

To register as a featured app in the Amulet ecosystem on DevNet:

```json
{
  "action": {
    "type": "dev_net_feature_app",
    "amulet_rules_cid": "<amulet-rules-contract-id>"
  }
}
```

This is a prerequisite for obtaining the `FeaturedAppRight` contract used in FAR configurations.

## Multi-Signature Wallet

The Decentralized Party Manager can serve as the foundation for a custodial multi-signature wallet product.

### How DecParty Maps to Multi-Sig

| Multi-Sig Concept | DecParty Equivalent |
|-------------------|---------------------|
| N-of-M signers | N = threshold, M = number of members |
| Transaction proposal | Governance action (any `ActionType`) |
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
│  DPM Node (Custodian A) │<--->│  DPM Node (Custodian B) │
│  threshold = 2 of 3     │     │                         │
│  POST /governance/      │     │  POST /governance/      │
│       confirm           │     │       confirm           │
└───────────┬─────────────┘     └───────────┬─────────────┘
            |                               |
            v                               v
┌─────────────────────────────────────────────────────────┐
│              Canton Ledger (Shared State)                │
│  - VaultGovernanceRules (threshold, members, timeout)    │
│  - VaultGovernanceConfirmation (per-action approvals)    │
│  - Vault contracts (assets under management)             │
└─────────────────────────────────────────────────────────┘
```

### Key Properties for Multi-Sig Wallets

- **No single point of failure**: The decentralized party has no central controller
- **Configurable quorum**: Set any threshold via `GovernanceSetThreshold`
- **Time-bound approvals**: Stale confirmations auto-expire via `actionConfirmationTimeout`
- **Auditable**: All confirmations and executions are recorded on the Canton ledger
- **Dynamic membership**: Add/remove signers without recreating the wallet

## Utility Services

The DPM supports onboarding to the Utility Registry, which provides provider and user service management.

### Full Onboarding Flow

The following sequence of governance actions sets up a complete utility service:

**1. Create ProviderService:**

```json
{
  "action": {
    "type": "utility_create_provider_request",
    "operator": "operator::1220..."
  }
}
```

**2. Create UserService:**

```json
{
  "action": {
    "type": "utility_create_user_request",
    "operator": "operator::1220..."
  }
}
```

**3. Link Services (Setup):**

After both services are created, link them:

```json
{
  "action": {
    "type": "utility_setup",
    "operator": "operator::1220...",
    "provider_service_cid": "<provider-service-contract-id>",
    "user_service_cid": "<user-service-contract-id>"
  }
}
```

### Querying Services

**List ProviderServices:**
```bash
curl "http://localhost:8080/services/provider?party_id=joint-vault::1220..."
```

```json
{
  "services": [
    {
      "contract_id": "00abc...",
      "operator": "operator::1220...",
      "provider": "joint-vault::1220..."
    }
  ]
}
```

**List UserServices:**
```bash
curl "http://localhost:8080/services/user?party_id=joint-vault::1220..."
```

```json
{
  "services": [
    {
      "contract_id": "00def...",
      "operator": "operator::1220...",
      "user": "joint-vault::1220..."
    }
  ]
}
```

### Credential Management

Issue and accept credentials through governance:

**Offer a free credential:**

```json
{
  "action": {
    "type": "credential_offer_free",
    "operator": "operator::1220...",
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
  "action": {
    "type": "credential_accept_free",
    "operator": "operator::1220...",
    "user_service_cid": "<user-service-contract-id>",
    "credential_offer_cid": "<credential-offer-contract-id>"
  }
}
```

All credential operations go through the same governance confirmation flow, requiring threshold approval from the decentralized party members.

## Generic Voting

The `GovernanceRules` contract supports free-text governance votes through the `GenericVoteProposal` template. Unlike vault or token actions, a generic vote has no on-chain side effect -- the outcome is recorded solely as a `GovernanceExecutionResult` contract on the ledger.

This is useful for off-chain decisions (e.g., policy changes, operational approvals) where you want an auditable on-chain record of the vote without triggering any contract state change.

### Scenario

Three custodians want to formally vote on a policy change. The vote itself doesn't modify any contracts, but the decision should be permanently recorded on the Canton ledger.

### Step 1: Propose a Vote (Custodian A)

```bash
curl -X POST http://custodian-a:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-vault::1220...",
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
curl "http://custodian-a:8080/governance/confirmations?party_id=joint-vault::1220..."
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
    "party_id": "joint-vault::1220...",
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
    "party_id": "joint-vault::1220...",
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

- `GovernanceRules` contract deployed (from `#governance-core-v0-rc4`)
- `governance-token-custody` DAR uploaded to all participants (from `#governance-token-custody-v0-rc4`)
- Token infrastructure deployed (transfer factories, instruments, etc.)

### Set Up Canton Coin Preapproval

Allows the governance party to receive Canton Coin transfers without per-transfer approval. This creates a `TransferPreapprovalProposal` that a provider must separately accept.

```bash
curl -X POST http://custodian-a:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-vault::1220...",
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
    "party_id": "joint-vault::1220...",
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
    "party_id": "joint-vault::1220...",
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
    "party_id": "joint-vault::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "proposal": {
      "type": "accept_transfer",
      "transfer_instruction_cid": "<transfer-instruction-cid>"
    }
  }'
```

> **Timing risk**: The sender can withdraw the transfer instruction before governance approval completes, which would cause execution to fail with a contract-not-found error.

### Governance Self-Management

The `GovernanceRules` contract supports self-management actions (add/remove members, change threshold, change timeout, manage the additional-proposers allowlist) through the `core_self` governance type. These do not require proposals -- they use value-based matching like `VaultGovernanceRules`.

**Add a new member:**

```bash
curl -X POST http://custodian-a:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-vault::1220...",
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
    "party_id": "joint-vault::1220...",
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
    "party_id": "joint-vault::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "action": {
      "type": "governance_set_timeout",
      "new_timeout_microseconds": 172800000000
    },
    "governance_type": "core_self"
  }'
```

**Grant propose-only rights to a non-member (`v0-rc4`+):**

```bash
curl -X POST http://custodian-a:8080/governance/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "joint-vault::1220...",
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
    "party_id": "joint-vault::1220...",
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
