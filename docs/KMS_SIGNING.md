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
the participant (`VaultService.ListMyKeys` reports `kms_key_id`; grpcurl's
JSON output renders the same field as `kmsKeyId`).

## What the operator must set up

1. **AWS credentials for decman.** decman uses the default AWS credential
   chain. On EKS, attach an IAM role to decman's service account (IRSA). The
   region must match the KMS keys' region (set `AWS_REGION` if the chain does
   not resolve it).

2. **`kms:Sign` on the party's Daml keys.** The keys are created by the
   participant under its own KMS role, so decman's role gets no access by
   default. Edit the **key policy** of each party Daml key — this is the
   reliable route, because it works regardless of how the key policy handles
   IAM delegation:

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
     "Resource": "arn:aws:kms:<region>:<account>:key/<party-daml-key-id>"
   }
   ```

   To find the key id, list the participant's keys over the Admin API — the
   party Daml key is named `<party-prefix>-daml-transactions` and its metadata
   carries `kmsKeyId`:

   ```bash
   grpcurl -plaintext -d '{"filters":{"name":"<party-prefix>-daml-transactions"}}' \
     <admin-host>:<admin-port> \
     com.digitalasset.canton.crypto.admin.v30.VaultService/ListMyKeys
   ```

   Alternatively, run the contracts workflow once and read the key id from the
   `AccessDeniedException` error, which names the key ARN.

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
