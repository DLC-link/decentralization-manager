# Decentralizing an external party

This guide hosts an external party on several participants. An external party
signs its own transactions with a key that its owner holds. Its namespace is
the fingerprint of that key.

The guide covers two jobs:

- [Create a new party](#create-a-new-party) that several participants host
  from the start.
- [Add hosts to an existing party](#add-hosts-to-an-existing-party), and copy
  its contracts to them.

Both jobs use the tenant API, `/v0/tenant/*`, on each host's DecMan node.

- **Host:** a participant that hosts the party, with a DecMan node next to it.
- **Joining host:** a host that the party gains in this run.
- **Wallet:** the program that holds the party's key and calls every host. The
  private key never leaves it. DecMan only receives the public key and
  signatures.

A Rust wallet can use the [`decman-wallet`](../crates/decman-wallet/README.md)
library, which runs these steps. This guide shows the same steps with `curl`.

For a party whose namespace is a participant's key, use
[Decentralizing an existing party](DECENTRALIZING_AN_EXISTING_PARTY.md). For a
decentralized party, use
[Adding a member](ADDING_A_MEMBER_TO_A_DECENTRALIZED_PARTY.md).

## What more hosts give the party

An external party signs its own transactions, so every host has Confirmation
permission. Two things follow:

- **Failover.** The wallet can submit through any host. The party keeps
  working while one host is down.
- **Co-validation.** The threshold is how many hosts must confirm each
  transaction. At threshold 1, any one host is enough. Above 1, no single host
  can act for the party alone.

Always send `"permission": "confirmation"`, or leave it out. A Submission host
submits on behalf of a party that has no key of its own. That is the failover
offer for a local party; see
[Decentralizing an existing party](DECENTRALIZING_AN_EXISTING_PARTY.md).

## Set up each host

The operator of each host does this once.

1. **Turn on the tenant API.** Set `DECPM_TENANT_API_KEYS` to one or more
   random keys, separated by commas, and restart DecMan. With no keys, the
   node refuses every tenant call.
2. **Let the wallet reach DecMan.** Route DecMan's HTTP port to the wallet over
   TLS. The tenant API checks the tenant key, not the operator login.
3. **Test the key.** A party that does not exist gives `404` with the right key
   and `401` with a wrong one:

   ```bash
   curl -sS -o /dev/null -w '%{http_code}\n' \
     -H "Authorization: Bearer $TENANT_KEY" \
     "$HOST/v0/tenant/test::1220$(printf '0%.0s' {1..64})/state"
   ```

4. **Vet the party's DARs.** A joining host needs every package that the
   party's contracts use. See [Check the DARs](#check-the-dars).
5. **Send the wallet three things:** the DecMan URL, a tenant key, and the
   participant ID. The participant ID is on the **Configuration** page, under
   **Node**.

## Sign a hash

Every write takes two calls. The `prepare` call returns `transaction_hashes`.
The wallet signs each hash with the party's key. The `execute` call then sends
the signatures with the `prepare` output.

These rules apply to every signature:

- Decode each hash from base64 and sign the bytes as they are. Do not hash them
  again.
- Send each signature base64-encoded, in the same order as the hashes.
- Set `signed_by` to the key's fingerprint, which is the party ID's namespace.

The fingerprint is SHA-256 over the 4-byte big-endian number 12 and the raw
32-byte Ed25519 public key, written in hex after the prefix `1220`.

This Python helper makes a key, prints its public half, and signs a `prepare`
answer. It needs the `cryptography` package.

```python
# key.py: new KEY_FILE | pub KEY_FILE | sign KEY_FILE PREPARE_JSON
import base64, hashlib, json, os, struct, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

mode, path = sys.argv[1], sys.argv[2]
if mode == "new":
    seed = Ed25519PrivateKey.generate().private_bytes_raw()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.write(fd, base64.b64encode(seed))
    sys.exit()
key = Ed25519PrivateKey.from_private_bytes(base64.b64decode(open(path).read()))
pub = key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
fingerprint = "1220" + hashlib.sha256(struct.pack(">i", 12) + pub).hexdigest()
if mode == "pub":
    print(json.dumps({"public_key": base64.b64encode(pub).decode(),
                      "fingerprint": fingerprint}))
else:
    prepared = json.load(open(sys.argv[3]))
    signatures = [base64.b64encode(key.sign(base64.b64decode(h))).decode()
                  for h in prepared["transaction_hashes"]]
    print(json.dumps({"signatures": signatures, "signed_by": fingerprint}))
```

An existing party can also have an ECDSA key on secp256k1, P-256 or P-384. The
host reads the key type from the party's mapping. Sign each hash with ECDSA over
SHA-256, or over SHA-384 for P-384. Send either a DER signature or the
fixed-width `r || s` pair, which is 64 bytes, or 96 bytes for P-384. Creating a
party takes an Ed25519 key only.

## Compare before you sign

The wallet calls `prepare` on **every** host and requires the same bytes from
each. The wallet cannot read a Canton hash. Without this check, one host could
return the hash of a different mapping, for example one that adds its own key.
One honest host is then enough to expose the dishonest one.

```bash
# Every line must show the same hash.
for f in prepare-*.json; do jq -S . "$f" | sha256sum; done
```

Stop if two hashes differ.

## Create a new party

The party gets 2 or more hosts and starts with no contracts.

1. Make a key and read its public half:

   ```bash
   python3 key.py new party.key
   PUB=$(python3 key.py pub party.key | jq -r .public_key)
   ```

2. Call `prepare` on every host. Each host lists the **other** hosts in
   `hosting_peers`:

   ```bash
   # hosts.txt holds one "DECMAN_URL PARTICIPANT_ID" line per host.
   HINT=alice
   while read -r url pid; do
     peers=$(awk -v me="$pid" '$2 != me {print $2}' hosts.txt | jq -R . | jq -s .)
     curl -sS -X POST "$url/v0/tenant/prepare" \
       -H "Authorization: Bearer $TENANT_KEY" -H 'Content-Type: application/json' \
       -d "$(jq -n --arg h "$HINT" --arg k "$PUB" --argjson p "$peers" \
             '{party_hint:$h, public_key:$k, hosting_peers:$p}')" \
       > "prepare-${pid%%::*}.json"
   done < hosts.txt
   ```

   `confirmation_threshold` is optional. It defaults to the number of hosts
   minus 1, and it may not be higher. A host can then always leave later.

3. [Compare the answers](#compare-before-you-sign), then sign one of them:

   ```bash
   python3 key.py sign party.key prepare-<any host>.json > signed.json
   ```

4. Call `execute` on **every** host with the same body:

   ```bash
   jq -n --arg h "$HINT" --arg k "$PUB" \
     --slurpfile p prepare-<any host>.json --slurpfile s signed.json \
     '{party_hint:$h, public_key:$k,
       topology_transactions:$p[0].topology_transactions,
       signatures:$s[0].signatures, signed_by:$s[0].signed_by}' > execute.json
   while read -r url pid; do
     curl -sS -X POST "$url/v0/tenant/execute" \
       -H "Authorization: Bearer $TENANT_KEY" -H 'Content-Type: application/json' \
       -d @execute.json
   done < hosts.txt
   ```

   Each call answers `202 Accepted`. Canton activates the party once the last
   host has signed, so the order does not matter.

5. Poll `GET /v0/tenant/{party}/status` on every host until each one answers
   `{"status": "completed"}`.

The party ID is `alice::<fingerprint>`. Each host also lists it on its
**Parties → External Parties** tab, within a few minutes.

## Add hosts to an existing party

> [!WARNING]
> **Keep the party idle from step 3 until the last import ends.** A joining host
> starts to receive the party's traffic as soon as the change takes effect. It
> leaves the synchronizer only when its import starts. If the party archives an
> older contract in between, the joining host records an archive for a contract
> it never held. A replay of that record crashed a MainNet participant on
> 2026-09-16. So pause every application that acts as the party.

### Check the DARs

The joining hosts need every package the party's contracts use. DecMan cannot
check this for an external party. The check reads the contracts over the
Ledger API, and the hosts hold no ledger credential for the party.

Without the packages, the import fails after the joining host has left the
synchronizer. So compare the vetted packages on the **Packages** page of each
joining host with a current host, and distribute what is missing.

### Steps

1. **Read the serial.** Every write pins the serial it builds on:

   ```bash
   curl -sS "$CURRENT_HOST/v0/tenant/$PARTY/state" -H "Authorization: Bearer $TENANT_KEY"
   # {"party_id": "...", "serial": 1, "threshold": 1, "host_count": 2,
   #  "onboarding_hosts": 0, "has_signing_key": true}
   ```

2. **Prepare on every host,** current and joining, with the same body:

   ```bash
   # all-hosts.txt: every current and joining host. NEW_HOSTS: a JSON array
   # of the joining hosts' participant IDs.
   jq -n --arg p "$PARTY" --argjson s "$SERIAL" --argjson n "$NEW_HOSTS" \
     '{party_id:$p, permission:"confirmation", new_hosts:$n, base_serial:$s}' > add.json
   while read -r url pid; do
     curl -sS -X POST "$url/v0/tenant/add-hosts/prepare" \
       -H "Authorization: Bearer $TENANT_KEY" -H 'Content-Type: application/json' \
       -d @add.json > "prepare-${pid%%::*}.json"
   done < all-hosts.txt
   ```

   [Compare the answers](#compare-before-you-sign), then sign one of them with
   `key.py sign`.

3. **Execute on each joining host only.** Canton needs the party's signature and
   each joining host's signature. The current hosts add nothing. Send the same
   body to every joining host.

   ```bash
   jq -n --arg p "$PARTY" --argjson s "$SERIAL" \
     --slurpfile x prepare-<any host>.json --slurpfile g signed.json \
     '{party_id:$p, base_serial:$s, topology_transactions:$x[0].topology_transactions,
       signatures:$g[0].signatures, signed_by:$g[0].signed_by}' > execute.json
   curl -sS -X POST "$JOINER/v0/tenant/add-hosts/execute" \
     -H "Authorization: Bearer $TENANT_KEY" -H 'Content-Type: application/json' \
     -d @execute.json
   ```

4. **Wait for the change.** Poll `/state` on a current host until `host_count`
   includes the joining hosts. `onboarding_hosts` then counts them too. A
   joining host holds the party now, but it does not confirm yet.

5. **Copy the contracts to each joining host.** Read block N from one current
   host and hand it to the joining host, until the import answers
   `"complete": true`:

   ```bash
   seq=1
   while :; do
     curl -sS -H "Authorization: Bearer $TENANT_KEY" \
       "$SOURCE/v0/tenant/$PARTY/acs/$JOINER_ID?base_serial=$SERIAL&seq=$seq" > block.json
     jq --arg p "$PARTY" --argjson s "$SERIAL" \
       '{party_id:$p, base_serial:$s, seq, chunk, end, total_len, sha256, package_ids}' \
       block.json > import.json
     curl -sS -X POST "$JOINER/v0/tenant/add-hosts/import" \
       -H "Authorization: Bearer $TENANT_KEY" -H 'Content-Type: application/json' \
       -d @import.json > result.json
     jq -e .complete result.json > /dev/null && break
     seq=$((seq + 1))
   done
   cat result.json   # {"complete": true, "imported": true, "marker_cleared": true, ...}
   ```

   `$SERIAL` is the serial from step 1, not the new one. Use the same source
   host for every block of one copy.

6. **Wait for the onboarding markers to clear.** Poll `/state` until
   `onboarding_hosts` is 0. The party can transact again from here.

### How the copy works

The joining host leaves the synchronizer when block 1 arrives. It reconnects
after the last block. For that time, every party on the joining host pauses.
The current hosts keep running.

- Neither node writes the contracts to disk or keeps more than one block in
  memory. Each import call returns once Canton has taken the block, which sets
  the pace.
- A copy cannot resume. If it breaks, ask for block 1 again. The source then
  opens a fresh export, and the joining host starts a fresh import.
- One block may be sent twice after a network error. The source serves the
  last block again, so one retry is safe.
- A copy that stays idle for 5 minutes ends. The joining host reconnects to
  the synchronizer, and you start again from block 1.

### Raise the threshold

This step is for co-validation only, and only after every marker has cleared.
A host with a marker cannot confirm, so it cannot count toward the threshold.

1. Read the serial again with `/state`. The add and each marker clear moved it.
2. Call `/v0/tenant/threshold/prepare` on every host with
   `{party_id, new_threshold, base_serial}`. Compare the answers, then sign.
3. Call `/v0/tenant/threshold/execute` on every host with
   `{party_id, base_serial, topology_transactions, signatures, signed_by}`.
4. Poll `/state` until `threshold` shows the new value.

Above threshold 1, the party needs that many hosts to confirm each transaction.
No single host can then act for it.

## Transact as the party

DecMan does not submit transactions for an external party. The wallet uses
interactive submission on the Ledger API of any host: `PrepareSubmission`, then
a signature over the prepared hash, then `ExecuteSubmission`.

The wallet needs a Ledger API user on that participant that may read as the
party and execute submissions for it. The host operator grants those rights.

## If something goes wrong

| Symptom | Cause | Do this |
|---|---|---|
| `401` | The tenant key is wrong, or the host has no keys set | Check `DECPM_TENANT_API_KEYS` on the host |
| `409` from a prepare or execute call | The party's serial changed after you read it | Read `/state` again and restart from prepare |
| `404` from a prepare or execute call | No host mapping exists for the party | Check the party ID. A joining host still answers `200`, because it reads the shared topology |
| `400` that names a field | The bundle does not match what the host built itself | Do not retry it as it is. Restart from prepare |
| The prepare answers differ | A host built a different mapping | Stop, and ask that host's operator |
| `409` from the ACS or import call | The two ends disagree about the next block | Restart the copy from block 1 |
| The import fails, and the joining host is back online | A block failed, or the copy was idle too long | Restart the copy from block 1 |
| `500` "Imported the ACS, but could not request the marker clear" on the last block | Canton already has its own marker clear pending | Nothing. Poll `/state` until `onboarding_hosts` is 0 |
| `"marker_cleared": false` | Canton clears the marker a short time after the import | Poll `/state` until `onboarding_hosts` is 0 |
| `"package_preflight": false` in a block | The source cannot list the party's packages | Normal for an external party. Check the DARs yourself before the copy |
| The joining host crash-loops after an import | It holds archives of contracts it never had | Manual repair. Contact the BitSafe team |

The host log can show `Could not record ACS transfer progress: FOREIGN KEY
constraint failed` during an import. It affects progress reporting only.

### Commitment mismatches after an import

A joining host can report commitment mismatches once it starts to confirm. Its
copy of the contracts then differs from what the other hosts expect. Canton's
`RepairCommitmentsUsingAcs` recomputes the commitments from the imported
contracts. Run it on the joining host only, and only after the import completed.

## Limits

- **No resume.** A broken copy restarts from block 1.
- **No transfer without the wallet.** The wallet carries every block. The hosts
  never talk to each other.
- **Very large parties.** The import re-validates every contract while the
  joining host is offline. For a very large party, that time is the limit, not
  the transfer.
- **No new party with contracts.** [Create a new party](#create-a-new-party)
  makes an empty party. To decentralize a party that holds contracts, add hosts
  to it.
