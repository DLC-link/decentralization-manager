# Signing with AWS KMS party keys

This guide covers decman on a participant that uses AWS KMS as its crypto
provider (`canton.participants.<p>.crypto.provider = kms`). On such a node the
party's signing keys are created inside AWS KMS and cannot be exported. decman
therefore signs ledger transactions by calling the AWS KMS `Sign` API. That
call needs IAM permission, which the operator must grant once per node.

## Background

Each decentralized-party member holds two keys:

| Key | Signs | How it signs on a KMS node |
|---|---|---|
| Namespace key | Topology transactions | The participant signs through its own KMS access. No setup. |
| Daml key | Ledger transactions (governance contract deployment) | decman calls AWS KMS `Sign` directly. **Needs IAM setup.** |

Canton has no API that signs a ledger transaction with a vault key, so decman
must reach the KMS itself. decman discovers the KMS key id automatically from
the participant (`VaultService.ListMyKeys` reports `kms_key_id`).

## What the operator must set up

1. **AWS credentials for decman.** decman uses the default AWS credential
   chain. On EKS, attach an IAM role to decman's service account (IRSA). The
   region must match the KMS keys' region (set `AWS_REGION` if the chain does
   not resolve it).

2. **`kms:Sign` on the party's Daml keys.** The keys are created by the
   participant under its own KMS role, so decman's role gets no access by
   default. Add a statement to the key policy of each party Daml key, or grant
   it via an IAM policy on decman's role:

   ```json
   {
     "Sid": "AllowDecmanPartyKeySigning",
     "Effect": "Allow",
     "Principal": { "AWS": "arn:aws:iam::<account>:role/<decman-irsa-role>" },
     "Action": "kms:Sign",
     "Resource": "*"
   }
   ```

   To find the key: run a workflow once and read the key id from the error, or
   list the participant's keys — the party Daml key is the one whose name is
   `<party-prefix>-daml-transactions` and whose metadata carries `kmsKeyId`.

3. Nothing else. Key discovery, algorithm selection (EC-P256 → ECDSA-SHA-256),
   signature format (DER), and local pre-submission verification are automatic.

## Failure modes

| Symptom | Cause |
|---|---|
| `KMS signing failed: ... AccessDeniedException` | decman's role lacks `kms:Sign` on the key (step 2). |
| `KMS signing failed: ... dispatch failure` | No AWS credentials or wrong region (step 1). |
| `failed local verification against the registered public key` | The KMS key does not match the registered public key. Check that the `kms_key_id` belongs to this party's Daml key. |
| Workflow fails at export with `ExportKeyPair` | The key carries no `kms_key_id`, so decman used the vault-export path. Expected on JCE nodes; on a KMS node this means the key metadata is inconsistent. |

## Scope

- Only AWS KMS is supported. A participant with a different KMS driver (for
  example MPCH) also reports a `kms_key_id`; signing then fails with a clear
  KMS error until a dedicated backend exists.
- Parties created before this feature hold Ed25519 vault keys on JCE nodes;
  those continue to sign through the export path with no changes.
