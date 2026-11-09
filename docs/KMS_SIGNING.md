# Signing with AWS KMS party keys

This guide covers decman on a participant that uses AWS KMS as its crypto
provider (`canton.participants.<p>.crypto.provider = kms`). On such a node the
party's signing keys are created inside AWS KMS and cannot be exported. decman
therefore signs ledger transactions by calling the AWS KMS `Sign` API. That
call needs IAM permission, which the operator must grant once per node.

## New party keys require KMS

decman creates a party's keys only on a participant that keeps its keys in a
KMS. Onboarding, add-party and rotate-key check this before they create a key.
They refuse a participant whose vault keys carry no KMS key id, and they refuse
a key that a name lookup finds outside the KMS.

The rule exists because of Splice 0.10. Canton 3.7 refuses to export a key from
its software vault unless key download is on, and Canton says not to turn that
on in production. A KMS key never needs an export, because decman signs with
the KMS `Sign` API.

- A vault that only wraps its software keys with a KMS key does not count.
  Those keys still need an export to sign.
- Insecure mode (`DECPM_INSECURE`, devnet only) skips the rule, and so do
  `test-mode` builds. A party created there gets software keys. On Splice 0.10
  its first deploy needs key download, as the last section explains.
- Parties created before this rule keep their software keys. decman signs with
  its stored copies, as the section on nodes without KMS explains. To move
  such a party's key into KMS, see [Rotating a Daml key](#rotating-a-daml-key).

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

The metadata must contain a bare key ID: a UUID or `mrk-` followed by 32
hexadecimal digits. decman rejects ARNs, aliases, and malformed IDs before
calling `Sign`. Bare IDs resolve in the AWS credentials' account and configured
region, so metadata cannot redirect signing to another account. If the
participant's keys are in another account, configure decman's credentials for
that account. Signatures are still verified against the registered public key.

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
| `Invalid kms_key_id` | Vault metadata contains an ARN, alias, or malformed ID. Use the bare ID of the participant's key and credentials for its account. |
| `failed local verification against the registered public key` | The KMS key does not match the registered public key. Check that the `kms_key_id` belongs to this party's Daml key. |
| Workflow fails at export with `ExportKeyPair` | The key carries no `kms_key_id`, so decman used the vault-export path. Expected on JCE nodes; on a KMS node this means the key metadata is inconsistent. |

## Rotating a Daml key

The rotate-key workflow replaces one member's Daml key for a party with a new
key from that member's vault. The members, the owners and both thresholds stay
the same. Use it to move a key into KMS after the participant moved to KMS, or
to replace a key you no longer trust.

1. On the member whose key changes, open the party and choose **Rotate my Daml
   key**. `decman-cli parties rotate-key` and `POST /rotate-key` do the same.
2. Every other member accepts the invitation. Each one checks the proposal
   before it signs. The proposal may only swap the named key for one new key,
   and the member's own records must name that key as the coordinator's.
3. decman submits the change. Every member then records the new key, so a
   later kick or rotation attributes it to the right member.

The new key gets a name of the form `<prefix>-daml-transactions-rotated-<run>`.
The old key stays in the vault and in decman's key store, because decman never
deletes a key. A party whose keys still sit in a legacy `PartyToKeyMapping` has
its members' keys moved inline in the same change.

Every member must run a decman version that knows the workflow. A member on an
older version cannot read the invitation, and the run fails.

On a KMS node, step 2 of the setup above applies to the new key too. Grant
decman's role `kms:Sign` on it before the next deploy.

## Scope

- Only AWS KMS is supported. A participant with a different KMS driver (for
  example MPCH) also reports a `kms_key_id`; signing then fails with a clear
  KMS error until a dedicated backend exists.
- Parties created before this feature hold Ed25519 vault keys on JCE nodes.
  decman copies those keys into its own key store, as the next section
  explains.

## Signing on a node without KMS

A participant with the JCE crypto provider keeps exportable keys in its vault.
Canton offers no RPC that signs a submission hash with a vault key, so decman
must hold the private key itself.

Before Splice 0.10, decman exported the key with `VaultService.ExportKeyPair`
for every deploy (`VaultExportSigner`). Canton 3.7, which Splice 0.10 runs,
refuses that export unless the node sets
`canton.parameters.enable-key-download = yes`. decman therefore keeps its own
copy of each key.

### decman's key store

- decman stores this node's Daml key for each party in the `party_key_store`
  table. Each key is encrypted with AES-256-GCM, bound to its party and
  fingerprint.
- decman takes the encryption key from the first of these sources that is
  set:
  1. The file named by `DECPM_KEY_STORE_KEY_FILE`. Mount it from a Secret,
     apart from the data volume. The file holds 64 hex characters or the
     base64 of 32 bytes, such as the output of `openssl rand -base64 32`.
  2. A key derived from `DECPM_DB_ENCRYPTION_KEY`, when that value has at
     least 32 characters. One SHA-256 pass makes a shorter value cheap to
     guess.
  3. In insecure mode only, `key-store.key` in the data directory. decman
     creates it on first use. It sits next to the database, so it suits local
     tests only.
- With none of these, the store is off. decman logs an error every 15 minutes
  and signs as it did before Splice 0.10.
- A deploy signs with the stored key first. A KMS-backed key signs through
  `AwsKmsSigner` as before. Any other key is exported, used, and stored for the
  next deploy.
- A node can move from the derived key to a key file. Keep
  `DECPM_DB_ENCRYPTION_KEY` set when you mount the file. decman then reads
  the older rows with the derived key and re-encrypts each one under the
  file's key.
- A stored key that does not decrypt is skipped. That happens when the
  encryption key changes or is lost. decman then exports the key again and
  replaces the stored copy. A Splice 0.10 participant refuses that export by
  default, so keep the encryption key stable and keep a copy of it. When it
  refuses, the copy task logs an error that says to restore the previous key.
- decman never deletes a key from the vault.

### Copying existing keys

A background task copies each party's key into the store at boot and then
every 15 minutes. It copies only the parties that this node hosts. It skips
KMS-backed keys and keys the store already holds. When a run cannot reach the
participant, the task retries after 30 seconds. Each further failure doubles
the wait, up to 15 minutes.

The task finds the hosted parties in decman's party cache. A new database holds
an empty cache until someone opens the party list. The task then copies those
keys on its next run, within 15 minutes.

Run this decman version on every node before its participant moves to Splice
0.10. The task then copies the keys while export still works. The log reports
`Copied this node's Daml key for <party> into DecMan's key store`.

Key download is the last resort. The `splice-participant` Helm chart turns it
on with `enableKeyDownload: true`. A participant without the chart sets
`canton.parameters.enable-key-download = yes`. Canton's configuration
reference says the setting "should not be enabled in production". While it is on, the admin API exports every key in the vault.
Two cases need it:

- **The participant moved to Splice 0.10 before decman.** Set the flag and
  restart the participant. Wait until the log stops reporting
  `Canton refused to export this node's Daml key`. Then remove the flag and
  restart again.
- **A party onboarded in insecure mode on a Splice 0.10 node.** The party's
  Daml key comes from the participant's software vault, so its first deploy
  needs the flag. Outside insecure mode, decman refuses to create such a key,
  as [New party keys require KMS](#new-party-keys-require-kms) explains.

