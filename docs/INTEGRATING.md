# Integrating with a Canton you already run

[`CUSTOM_DAML_TEMPLATES.md`](CUSTOM_DAML_TEMPLATES.md) covers writing a
`GovernableAction` and driving it: the package layout, `proposal_cid` and its
placeholder action, `disclosed_contracts`, granting propose-only rights. It
assumes Decentralization Manager is already talking to a participant.

This page covers how to reach that point, and the problems that appear later.

> **Items 1 to 4 apply to a local, insecure setup only.** They describe the
> handshake between Canton's `unsafe-jwt-hmac-256` auth service and the token
> Decentralization Manager mints for itself in `DECPM_INSECURE` mode; the
> `DECPM_CANTON_HMAC_*` settings are read only in that mode. **A participant
> configured this way accepts any token signed with a public secret, so none
> of it belongs on a real deployment.** For production, configure an identity
> provider on the participant and point Decentralization Manager at the same
> issuer. See [`DEPLOYMENT_GUIDE.md`](DEPLOYMENT_GUIDE.md).
>
> Items 5 to 7 apply to any deployment.

Seven items. Four are configuration, where the errors name the vote rather
than the config and so read as governance faults. Three are operational, and
each arrives long after the step that caused it.

## 1. Canton must have authentication enabled, even locally

*Insecure setup only.*

A node with no `auth-services` at all fails every vote:

```text
INVALID_TOKEN(8): The submitted request is missing a user-id:
Cannot default user_id field because claims do not specify an user-id.
Is authentication turned on?
```

Onboarding and package distribution still succeed, because those go through
the Admin API. It is the first `/governance/confirm` that fails, which makes
it look like a governance problem rather than a configuration one.

A command needs a user id, and Canton takes it from the token's `sub` claim.
With no auth service there are no claims and nothing to default from.

**The block below is for a local node only.** A participant carrying it
accepts any token signed with the public secret `unsafe`, and it must not be
copied onto a real participant.

```hocon
# LOCAL / INSECURE ONLY - accepts any token signed with the secret below.
ledger-api {
  auth-services = [{
    type = unsafe-jwt-hmac-256
    target-audience = "https://canton.network.global"
    secret = "unsafe"
  }]
}
```

Those are the values `DECPM_CANTON_HMAC_AUDIENCE` and
`DECPM_CANTON_HMAC_SECRET` default to, so an insecure-mode Decentralization
Manager lines up with no further configuration.

## 2. `auth-services` belongs on `ledger-api` only

*Insecure setup only.*

`http-ledger-api` has no `auth-services` key and no `max-token-lifetime`
key. Adding either to that section does not weaken or duplicate anything - it
stops the node starting, at config parse, before any component runs:

```text
GENERIC_CONFIG_ERROR(8,0): Cannot convert configuration to a config of class
com.digitalasset.canton.config.CantonConfig. Failures are:
  at 'canton.participants.<name>.http-ledger-api.auth-services':
    - (canton.conf: 74) Unknown key.
  at 'canton.participants.<name>.http-ledger-api.max-token-lifetime':
    - (canton.conf: 73) Unknown key.
Failed to read config at startup
```

Worth stating because a config file usually lists the two sections next to
each other, and the JSON API is the one a browser talks to, so putting the
auth settings there is a natural guess.

Reproduced on Canton 3.5.8.

## 3. Canton needs `max-token-lifetime = Inf`, because the insecure token never expires

*Insecure setup only.*

In insecure mode Decentralization Manager mints its Canton token with `aud`,
`sub` and `iat` and **no `exp`** (`crates/decman/src/auth/mock.rs`). Canton
rejects a token with no expiry:

```text
Could not verify JWT token: token has no expiration time
```

Substituting a hand-made token with a distant expiry does not help either:

```text
Could not verify JWT token: token lifetime (2099-01-01T00:00:00Z) too long
```

One setting covers both, on the same section:

```hocon
ledger-api { max-token-lifetime = Inf }
```

This is what the Splice LocalNet bundle sets (`conf/canton/app.conf`).

## 4. Use `participant_admin`; do not create a ledger user first

*Insecure setup only.*

With `target-audience` set, Canton reads audience-based tokens and takes the
user id from `sub`, so **the user must already exist**. Creating one from the
bootstrap console needs a token the console does not have, which is a loop.

Canton creates exactly one user for itself, `participant_admin`, with
`ParticipantAdmin` rights. Naming it in the token removes the loop:

```text
DECPM_CANTON_HMAC_SUBJECT=participant_admin
```

Any party the governance flow acts as still needs `CanActAs` granted to that
user, as usual.

## 5. Peers need every package the action touches, not only the action's own

Distributing the action's own DAR to a peer is not enough. Naming a
decentralised party as an approver makes the peer's participant a stakeholder
and, where the party is also an executor, it must validate everything the
action does.

An action that settles a Token Standard V2 batch therefore needs the asset
packages on the peer as well. Without them:

```text
UNRESOLVED_PACKAGE_NAME(11): Interpretation error: Update failed due to a
failed package name resolution: splice-test-token-v2
```

Package names resolve to a version vetted by **every** informee, so one node
missing one package fails the whole submission. The error names the package
but not the node, and it arrives at the settlement rather than at
distribution, long after the step that caused it.

## 6. A member with no node can lock the party out of its own rules

Adding a governance member is a single dialog, and its failure mode is a
party that can no longer govern itself.

A member that cannot confirm still counts towards the threshold. If the other
members cannot reach the threshold without it, no action can execute,
including the action that removes it.

Two details make this sharper:

- `get_member_party_id` resolves the confirming member from the node's stored
  credentials, taking the **first** whose `dec_party_id` matches. One node
  casts exactly one confirmation; adding a second credential for the same
  decentralised party does not give a second vote.
- The UI does not allow **Member Party ID** to be edited once saved.

### Recovery, as a last resort

`PUT /party-config` accepts `member_party_id` and treats absent credential
fields as "keep existing". Pointing a node at the stranded member, confirming,
and pointing it back will break the deadlock:

```text
PUT /party-config   { dec_party_id, member_party_id: <stranded>, user_id, ... }
POST /governance/confirm
PUT /party-config   { dec_party_id, member_party_id: <original>, user_id, ... }
```

> **This is recovery, not a routine step.** It works only where the node's
> ledger user can act as the stranded member, and in that case **one operator
> casts two of the party's confirmations**. That defeats the assumption the
> threshold encodes, one operator per member, and while the procedure runs,
> the party's decisions are not what its rules describe. Use it to escape a
> deadlock, record that it was used, and remove the stranded member
> immediately afterwards.

## 7. The Execute button for custom proposals sends no disclosed contracts

This is specific to custom actions filed with `proposal_cid`. The standard
execute dialog (`ExecuteDialog.tsx`) does accept disclosed contracts, and
actions executed through it are unaffected.

For a custom `GovernableAction` whose `executeImpl` reaches a contract the
executing node has never seen, the Approvals tab's Execute button submits an
empty `disclosed_contracts` and the click fails:

```text
CONTRACT_NOT_FOUND(11): Contract could not be found with id 00bf0947...
```

The id in the message is the contract that should have been disclosed: a
registry's rules contract, for example. Nothing in the error mentions
disclosure, so it reads as a missing contract rather than a missing
parameter.

So for a custom proposal of this kind, **execute over
`POST /governance/execute` rather than from the Approvals tab**.

The failure is harmless: the proposal and its confirmations survive, and a
subsequent API execute succeeds.
