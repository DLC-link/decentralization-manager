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
      "PAR::custodian-b::1220...",
      "PAR::custodian-c::1220..."
    ]
  }'
```

After all attestors accept and the workflow completes, a decentralized party `joint-vault::1220...` exists with threshold 2.

**2. Deploy governance contracts:**

```bash
curl -X POST http://custodian-a:8080/contracts \
  -H "Content-Type: application/json" \
  -d '{
    "decentralized_party_id": "joint-vault::1220...",
    "participant_ids": ["PAR::a::1220...", "PAR::b::1220...", "PAR::c::1220..."],
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
        "package_id": "#bitsafe-vault-governance-v0-rc5",
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
      "vault_far_config": null
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
      "vault_far_config": null
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
        "participant_id": "PAR::custodian-c::1220...",
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
    }
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

**4. Accept Holder Service Requests:**

When a holder requests service access:

```json
{
  "action": {
    "type": "utility_accept_holder_service_request",
    "operator": "operator::1220...",
    "provider_service_cid": "<provider-service-contract-id>",
    "holder_service_request_cid": "<request-contract-id>",
    "holder": "holder-party::1220..."
  }
}
```

**5. Create Transfer Rule:**

```json
{
  "action": {
    "type": "utility_create_transfer_rule",
    "operator": "operator::1220...",
    "registrar_service_cid": "<registrar-service-contract-id>"
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
