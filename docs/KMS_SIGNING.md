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
- Parties created before this rule keep their software keys. decman signs with
  its stored copies, as [Signing on a node without KMS](#signing-on-a-node-without-kms)
  explains. To move such a party's key into KMS, see
  [Rotating a Daml key](#rotating-a-daml-key).
- decman logs at boot whether its participant keeps its keys in KMS, and which
  rule applies.

### Participants without KMS

A participant that has not moved to KMS yet can still hold new party keys.
Set `DECPM_ALLOW_SOFTWARE_PARTY_KEYS=true` on its decman. decman then
generates each new party Daml key itself, and Canton's vault never holds it:

1. Before it creates a key, decman checks that its key store is on. It
   refuses the run otherwise.
2. decman generates an Ed25519 key and stores it encrypted in its key store,
   in the `generated_party_keys` table. The key's name follows the vault's
   naming of party keys, so a retried run finds the same key.
3. Canton requires each new party key to sign the topology change that adds
   it. The vault cannot sign with a key it does not hold, so decman adds that
   signature itself.
4. decman files the key under the party, so deploys sign with it. Add-party
   and rotate-key know the party from the start. Onboarding learns it just
   before the members sign the party's `PartyToParticipant` proposal. While
   the setting is on, a member that cannot file its key refuses to sign, so
   the party does not come into force.

The setting decides only where a new key goes. A key that decman generated
keeps signing after an operator removes the setting, as a vault key does. A
deploy files such a key under its party when onboarding did not.

Canton never exports these keys, so they need no key download on Splice 0.10.
The namespace key stays in the participant's vault, because the participant
signs topology with it inside Canton.

On a participant that already uses KMS, the setting changes nothing: Canton
creates the Daml key in KMS. A software key that such a vault finds by name
predates the move to KMS, and decman refuses it, because it would need an
export to sign.

Insecure mode (`DECPM_INSECURE`, devnet only) lets the vault create a software
key instead. It only logs a warning, so a party created there on Splice 0.10
needs key download for its first deploy. A node that sets both settings
follows the opt-out, so decman generates the key.

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

   To find the key id, list the participant's keys over the Admin API. Each
   party Daml key has a name that starts with `<party-prefix>-daml-transactions`,
   and its metadata carries `kmsKeyId`. Each onboarding or add-party run adds
   `-run-<random id>` to the name, and a rotation adds
   `-rotated-from-<fingerprint>`. A party created before per-run names keeps
   the bare name. A rotation that lacks the grant names the key id in its
   error:

   ```bash
   grpcurl -plaintext -d '{}' \
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
| Rotation fails at GenerateKey with `DecMan cannot sign with the new KMS key` | decman's role lacks `kms:Sign` on the new key. Grant it and start the rotation again, as [the KMS grant for the new key](#the-kms-grant-for-the-new-key) explains. |
| Run fails with `The vault holds the software key ... from before this participant moved to KMS` | The participant uses KMS, but its vault still holds a software key under the new key's name. Delete that key from the vault, then retry. |
| Onboarding fails with `DecMan cannot file this node's new Daml key under the party` | The member has no record of its key for the party, or its key store does not decrypt. Check `DECPM_KEY_STORE_KEY_FILE`, then retry the run from its coordinator. |
| A peer refuses a rotation: `has ... on record as ...'s Daml key` | The coordinator's last rotation failed after it was submitted, and this peer did not record the new key. Retry that run. Do not start a new one. |
| A peer refuses a rotation: `has no Daml key on record for ...` | The peer's party cache lacks a member's key. Refresh `/decentralized-parties` on that peer and retry. |
| A rotation refuses to start: `inline signing key(s) for ... hosts` | The party keeps some members' keys in a legacy mapping. Run change-threshold first, then rotate. |
| Workflow fails at export with `ExportKeyPair` | The key carries no `kms_key_id`, so decman used the vault-export path. Expected on JCE nodes; on a KMS node this means the key metadata is inconsistent. |

## Rotating a Daml key

The rotate-key workflow replaces one member's Daml key for a party with a new
key. The members, the owners and both thresholds stay
the same. Use it to replace a key you no longer trust. It also moves a key
into KMS, when the participant kept its participant id through its move to
KMS.

1. On the member whose key changes, open the party and choose **Rotate My Daml
   Key**. `decman-cli parties rotate-key` and `POST /rotate-key` do the same.
2. Every other member accepts the invitation.
3. decman creates the new key and checks that it can sign with it. On a KMS
   node, the vault creates the key, and decman signs a fixed test message
   through KMS. On a node with the opt-out, decman generates the key in its
   key store and signs the proposal with it.
4. Each other member checks the proposal before it signs. The proposal must
   keep the host entries and both thresholds in force, and swap only the
   named key for one new key. The signing threshold stays as it is, even
   where it differs from the namespace threshold. The member's own records must name the replaced key as
   the coordinator's, and every other member's key must stay.
5. decman submits the change. Every member then records the new key, so a
   later kick or rotation attributes it to the right member. A member records
   only the key it checked in step 4.

If a rotation fails after it was submitted, retry that run with
`POST /workflows/<instance>/retry` or from the workflow list. Do not start a
new rotation. A member that missed the new key would refuse it.

decman starts a rotation only when every other member is a configured peer
and runs a version that knows the workflow. Otherwise it refuses with a 409
that names those members.

The new key gets a name of the form
`<prefix>-daml-transactions-rotated-from-<old key fingerprint>`. The name
depends only on the key it replaces. A rotation that failed and starts again
therefore reuses the same new key. The exception is a participant that moved
to KMS in between. It gets a new KMS key instead of a key that decman
generated earlier. The old key stays in the vault and in
decman's key store, because decman never deletes a key. A party whose keys
still sit in a legacy `PartyToKeyMapping` has its members' keys moved inline
in the same change, and the members then retire the legacy mapping.

decman refuses to rotate a key of a party that carries fewer inline keys than
hosts. Such a party keeps the other keys in its legacy mapping, and Canton
counts only the inline keys. A rotation would put every key inline at the
inline threshold, so more members could sign alone. Run change-threshold
first. It moves every key inline under a threshold that all members vote on.

### The KMS grant for the new key

On a KMS node, step 2 of the setup above applies to the new key too. The
participant creates the key under its own role, so decman cannot sign with it
until an operator grants `kms:Sign`. decman checks this before it submits the
change:

1. Start the rotation. If the grant is missing, the run fails at GenerateKey.
   The error names the new KMS key id.
2. Grant decman's role `kms:Sign` on that key, as step 2 of the setup
   describes.
3. Start the rotation again. It reuses the key from the first run, so the
   grant applies, and the rotation completes.

The party keeps its old key until the rotation completes, so deploys keep
working while the grant is missing.

## Moving a participant to KMS

Restarting a participant with `crypto.provider = kms` does not move its
existing keys into KMS. Moving a running participant is a Canton migration.
Follow the KMS migration guide for your Canton version, for example
[Canton 3.4](https://docs.digitalasset.com/operate/3.4/howtos/secure/kms/migration/kms_migration.html).
The Splice
[validator security guide](https://docs.dev.sync.global/validator_operator/validator_security.html)
says that moving an existing validator to KMS is not generally supported.

What decman needs afterwards depends on the participant id:

- **The participant id stays the same.** Rotate the Daml key of each party the
  node hosts, as [Rotating a Daml key](#rotating-a-daml-key) explains.
- **The participant gets a new id.** Add the new participant to each party
  with add-party. Then kick the old participant.

Until the move, set `DECPM_ALLOW_SOFTWARE_PARTY_KEYS=true`, so the node can
still create and join parties.

## Scope

- Only AWS KMS is supported. A participant with a different KMS driver (for
  example MPCH) also reports a `kms_key_id`; signing then fails with a clear
  KMS error until a dedicated backend exists.
- Parties created before this feature hold Ed25519 vault keys on JCE nodes.
  decman copies those keys into its own key store, as the next section
  explains. Keys created under the opt-out never sit in the vault, so they
  need no copy.

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
- A key that decman generated has no copy in the vault. Its only copy is the
  `generated_party_keys` row, and only the encryption key opens it. Back up
  decman's database and keep the encryption key safe. A member that loses
  either can no longer sign for the party. The other members must then kick
  it and add it again with add-party.
- Each onboarding or add-party run creates a new Daml key. The key's name
  holds a random id that decman records with the run, so a retry finds the
  same key. A member that is kicked and added again therefore gets a new key,
  never the key that a rotation replaced. A former host that still owns the
  party keeps its key in the party's key set, so hosting it again reuses that
  key. On a KMS node, grant decman's role
  `kms:Sign` on that new key, as step 2 of the setup describes. decman
  refuses to reuse a key that no longer decrypts, and asks for the previous
  encryption key.

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
  needs the flag. Outside insecure mode, decman never puts a new party key
  there, as [New party keys require KMS](#new-party-keys-require-kms)
  explains.

