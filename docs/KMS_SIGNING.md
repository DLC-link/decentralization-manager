# Signing with AWS KMS party keys

This guide covers decman on a participant that uses AWS KMS as its crypto
provider (`canton.participants.<p>.crypto.provider = kms`). On such a node the
party's signing keys are created inside AWS KMS and cannot be exported. decman
therefore signs ledger transactions by calling the AWS KMS `Sign` API. That
call needs IAM permission, which the operator must grant once per node.

## Background

For every party created from decman 2.0 onward, each member holds **one** key
per decentralized party. decman names it `{prefix}-key` in the vault and gives
it both usages, `Namespace` and `Protocol`. Its self-signed root
`NamespaceDelegation` publishes the public key into the synchronizer topology
store, and the same public key goes into `PartyToParticipant.party_signing_keys`.

| What the key signs | How it signs on a KMS node |
|---|---|
| Topology transactions (the namespace and party mappings) | The participant signs through its own KMS access. No setup. |
| Ledger transactions (governance contract deployment) | decman calls AWS KMS `Sign` directly. **Needs IAM setup.** |

Canton has no API that signs a ledger transaction with a vault key, so decman
must reach the KMS itself. decman discovers the KMS key id automatically from
the participant (`VaultService.ListMyKeys` reports `kms_key_id`; grpcurl's
JSON output renders the same field as `kmsKeyId`).

### One key signs both, and what that means for the key policy

The `{prefix}-key` of a 2.0 party is the member's namespace key **and** its
Daml signing key. Granting decman `kms:Sign` on that key therefore grants
decman the power to sign topology transactions for the party as well.

Two consequences follow:

- Scope the grant to that one key. An IAM policy with `"Resource": "*"` hands
  decman the participant's own namespace key, which governs every party the
  participant hosts. Never write that policy.
- A key policy on `{prefix}-key` is per party. One party's grant gives decman
  nothing on another party's key, so a compromise of decman's role is bounded
  by the parties you granted.

The decentralized namespace still bounds what a single signature achieves. A
topology change needs the namespace threshold of owner signatures, so decman's
access to one member's key never changes the party by itself.

### Key discovery

decman finds the key by **fingerprint**, not by name:

1. It reads the head `PartyToParticipant` of the party and takes the
   `party_signing_keys` list.
2. It keeps the one fingerprint in that list that this node's vault also holds
   with the `Protocol` usage.
3. It calls `ListMyKeys` filtered by that fingerprint and reads `kms_key_id`
   from the metadata.

Renaming a vault key therefore does not break signing. A node that holds no
matching key fails with "this node holds no Daml signing key for `<party>`".

## What the operator must set up

1. **AWS credentials for decman.** decman uses the default AWS credential
   chain. On EKS, attach an IAM role to decman's service account (IRSA). The
   region must match the KMS keys' region (set `AWS_REGION` if the chain does
   not resolve it).

2. **`kms:Sign` on the party's key.** The keys are created by the participant
   under its own KMS role, so decman's role gets no access by default. Edit the
   **key policy** of each party key — this is the reliable route, because it
   works regardless of how the key policy handles IAM delegation:

   ```json
   {
     "Sid": "AllowDecmanPartyKeySigning",
     "Effect": "Allow",
     "Principal": { "AWS": "arn:aws:iam::<account>:role/<decman-irsa-role>" },
     "Action": "kms:Sign",
     "Resource": "*"
   }
   ```

   In a key policy, `"Resource": "*"` means "this key" and is safe.

   An **IAM policy** on decman's role works only when the key policy delegates
   to IAM. If you take that route, the statement has no `Principal`, and the
   `Resource` must name the specific key ARNs. Never use `"Resource": "*"` in
   an IAM policy — that grants signing with **every** key in the account,
   including the participant's namespace key:

   ```json
   {
     "Sid": "AllowDecmanPartyKeySigning",
     "Effect": "Allow",
     "Action": "kms:Sign",
     "Resource": "arn:aws:kms:<region>:<account>:key/<party-key-id>"
   }
   ```

   To find the key id, list the participant's keys over the Admin API. The key
   of a 2.0 party is named `<party-prefix>-key` and its metadata carries
   `kmsKeyId`:

   ```bash
   grpcurl -plaintext -d '{"filters":{"name":"<party-prefix>-key"}}' \
     <admin-host>:<admin-port> \
     com.digitalasset.canton.crypto.admin.v30.VaultService/ListMyKeys
   ```

   For a 2.0 party the `my_owner_key` field of `GET /decentralized-parties`
   carries the same fingerprint, so you can filter on it instead of the name:

   ```bash
   grpcurl -plaintext -d '{"filters":{"fingerprint":"<fingerprint>"}}' \
     <admin-host>:<admin-port> \
     com.digitalasset.canton.crypto.admin.v30.VaultService/ListMyKeys
   ```

   Alternatively, run the contracts workflow once and read the key id from the
   `AccessDeniedException` error, which names the key ARN.

3. Nothing else. Key discovery, algorithm selection (EC-P256 → ECDSA-SHA-256),
   signature format (DER), and local pre-submission verification are automatic.

## Parties created before 2.0

A party created before 2.0 holds **two** keys per member: `{prefix}-namespace`
for topology transactions and `{prefix}-daml-transactions` for ledger
transactions. Those parties keep working, and the split keeps its old benefit:
the `kms:Sign` grant covers the Daml key only, so decman never signs a topology
transaction for that party.

Grant `kms:Sign` on the Daml key, and find its id by that name:

```bash
grpcurl -plaintext -d '{"filters":{"name":"<party-prefix>-daml-transactions"}}' \
  <admin-host>:<admin-port> \
  com.digitalasset.canton.crypto.admin.v30.VaultService/ListMyKeys
```

decman falls back to this name only after the fingerprint lookup finds nothing,
so a legacy party needs no configuration. Do not rename or delete the two keys:
the party still authorizes its topology changes with `{prefix}-namespace`.

## Failure modes

| Symptom | Cause |
|---|---|
| `KMS signing failed: ... AccessDeniedException` | decman's role lacks `kms:Sign` on the key (step 2). |
| `KMS signing failed: ... dispatch failure` | No AWS credentials or wrong region (step 1). |
| `this node holds no Daml signing key for <party>` | No vault key of this node appears in the party's `party_signing_keys`. The node never joined the party, or an operator removed the key. |
| `signing key <fingerprint> is not in this node's vault` | The party names a key this node does not hold. Check that you are on the participant that joined the party. |
| `failed local verification against the registered public key` | The KMS key does not match the registered public key. Check that the `kms_key_id` belongs to this party's key. |
| Workflow fails at export with `ExportKeyPair` | The key carries no `kms_key_id`, so decman used the vault-export path. Expected on JCE nodes; on a KMS node this means the key metadata is inconsistent. |

## Scope

- Only AWS KMS is supported. A participant with a different KMS driver (for
  example MPCH) also reports a `kms_key_id`; signing then fails with a clear
  KMS error until a dedicated backend exists.
- Parties created before this feature hold Ed25519 vault keys on JCE nodes;
  those continue to sign through the export path with no changes.
- A KMS node must prove the dual-usage key before a mainnet rollout: generate
  `{prefix}-key` with both usages, publish its root `NamespaceDelegation`,
  co-sign one `PartyToParticipant`, and run one contracts round with an
  ECDSA/DER signature.
