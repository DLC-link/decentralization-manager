# Co-validating a decentralized party

This guide is for the operator of a covalidator. A covalidator hosts a
decentralized party, but it does not own the party. DecMan approves the
hosting request when a dApp asks for it.

- **Owner:** a holder of one of the party's signing keys. Owners sign in their
  wallets. Their participants submit the party's onboarding topology.
- **Covalidator:** a participant that hosts the party with Confirmation
  permission and is not an owner. It must authorize the pending
  `PartyToParticipant` proposal before the party exists.
- **dApp:** the browser application that creates the party. After the owners'
  participants submit, it asks each covalidator to approve.

A covalidator does not need DecMan. Its operator can authorize the proposal by
hand instead. DecMan only automates that step.

## What the node checks

DecMan signs nothing that the caller sends. The caller names a party and the
hash of its `PartyToParticipant` transaction. DecMan reads the pending proposal
from its own synchronizer store and picks the one with that hash.

It signs the proposal only when every rule below holds:

- The transaction is an add at serial 1. It creates a party and changes none.
- The mapping is a `PartyToParticipant` for the party the request names.
- Every host has Confirmation permission and no onboarding marker.
- No host is listed twice, and this participant is one of the hosts.
- The confirmation threshold is between 1 and one less than the number of
  hosts, so a host can still leave later.
- The party has at least one signing key, and no key twice.
- The signing threshold is between 1 and the number of signing keys.
- The party's namespace has a `DecentralizedNamespaceDefinition` in force.
- No other `PartyToParticipant` is in force for the party.

DecMan then signs the proposal with the node's topology key and submits it.
Canton puts the mapping in force once every host has signed.

## Set up the node

The operator does this once.

1. **Turn on dApp keys.** Set `DECPM_DAPP_API_KEYS` to one or more random keys,
   separated by commas, and restart DecMan. A dApp key opens only two routes:
   the approval and the status read. Every other tenant route answers it with
   `403`. Each key may make `DECPM_TENANT_RATE_LIMIT_PER_MINUTE` calls per
   minute, 120 by default.
2. **Let the dApp reach DecMan.** Route DecMan's HTTP port to the internet over
   TLS. The two routes accept calls from any origin, without credentials, so
   the browser needs no `DECPM_ALLOWED_ORIGIN`.
3. **Test the key.** A party that does not exist gives `404` with the right
   key and `401` with a wrong one:

   ```bash
   curl -sS -o /dev/null -w '%{http_code}\n' \
     -H "Authorization: Bearer $DAPP_KEY" \
     "$HOST/v0/tenant/test::1220$(printf '0%.0s' {1..64})/status"
   ```

4. **Send the dApp three things:** the DecMan URL, a dApp key, and the
   participant ID. The participant ID is on the **Configuration** page, under
   **Node**.

A dApp key sits in a browser, so treat it as easy to copy. It can only ask the
node to approve a proposal that passes the checks above. A tenant key from
`DECPM_TENANT_API_KEYS` also works on both routes.

## Approve hosting

The dApp calls this after the owners' participants have submitted the party.
`TRANSACTION_HASH` is Canton's hash of the `PartyToParticipant` transaction,
in base64.

```bash
curl -sS -X POST "$HOST/v0/tenant/decentralized-party/approve-hosting" \
  -H "Authorization: Bearer $DAPP_KEY" \
  -H "Content-Type: application/json" \
  -d "{\"party_id\": \"$PARTY_ID\", \"transaction_hash\": \"$TRANSACTION_HASH\"}"
```

A `202` answer reports this host's view:

```json
{ "status": "inprogress", "party_id": "..." }
```

`inprogress` means the node signed, and other hosts still have to sign.
`completed` means the party is in force and hosted here. A repeat call is
safe. A party the node already hosts answers `completed` and signs nothing
again.

The dApp then polls `GET /v0/tenant/{party}/status` until every covalidator
reports the party hosted.

## Answers

| Status | Meaning | Do this |
|---|---|---|
| `202` | The node signed, or it already hosts the party | Poll the status route |
| `400` | The request is malformed, or the proposal fails a check | Do not retry it as it is. The error names the rule |
| `401` | The key is not one of this node's keys | Send this node its own key. Check `DECPM_DAPP_API_KEYS` |
| `403` | A dApp key called a route other than the two dApp routes | Use a tenant key for that route |
| `404` | No pending proposal for the party names this node yet | Retry for a short time. The owners' submission may still be on its way |
| `409` | A pending proposal names this node under another hash, or the party already exists | Stop. Check the hash, or start the party again |
| `429` | More calls per minute than the node allows for that key | Wait the seconds in `Retry-After`, then retry |
| `500` | A Canton call failed on this node | Ask this node's operator to check its logs |

## Audit

The operator can read every approval call at `GET /tenant-call-audit`. Each
row has the endpoint `/v0/tenant/decentralized-party/approve-hosting`, the
party, the HTTP status, the error, and the key. The row names a dApp key by its
index in `DECPM_DAPP_API_KEYS` and by its key id. The key id tells a dApp key
from a tenant key at the same index.

## Limits

- **New parties only.** The node approves a party's first hosting mapping. It
  does not approve a change to a party that exists.
- **Threshold cap.** A confirmation threshold equal to the number of hosts is
  refused, as on the other tenant routes.
- **No party limit.** The node approves any pending proposal that names it and
  passes the checks. A key authorizes the call, not one party.
