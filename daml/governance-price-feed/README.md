# governance-price-feed-v1

A price feed run by a decentralized party. A new price is published only after
the governance threshold of members confirms the exact move, so no single
operator can set the price an app relies on.

| Template | Kind | What it does |
| --- | --- | --- |
| `PriceFeed` | contract | The current price of one asset, signed by the governance party and visible to its `subscribers`. Each price update increments `round`. |
| `PriceFeedRegistry` | contract | The governance party's set of open feed ids. Keeps one active feed per `feedId`. |
| `OpenPriceFeedProposal` | `GovernableAction` (`OpenPriceFeed`) | Registers a new `feedId` and opens its feed at an initial price. Fails if the id is already open. |
| `PublishPriceProposal` | `GovernableAction` (`PublishPrice`) | Moves a feed from a stated round and price to a new price. |
| `SetPriceFeedSubscribersProposal` | `GovernableAction` (`SetPriceFeedSubscribers`) | Replaces a feed's subscriber list. |
| `ClosePriceFeedProposal` | `GovernableAction` (`ClosePriceFeed`) | Archives a feed and releases its id, so it can be opened again. |

All proposals follow the conventions in
[CUSTOM_DAML_TEMPLATES.md](../../docs/CUSTOM_DAML_TEMPLATES.md): `proposer` is the
signatory, `governanceParty` observes, and `executeImpl` runs with the governance
party's authority after `GovernanceRules_ExecuteConfirmedAction`. Each proposal's
`description`, which members confirm and `GovernanceExecutionResult` keeps as the
audit record, shows the feed id and everything the action changes: the asset and
prices, the observation time and deadline, and the subscriber list.

## Consuming a price safely

**Check who signed the feed.** `PriceFeed` has one signatory, `governanceParty`, so
any party can create a `PriceFeed` that names itself as `governanceParty`, with any
`feedId` and price and with your app as a subscriber. No vote is involved. A
borrower could create such a feed at a high price and pass its contract ID to your
margin-call choice; a bare `fetch` succeeds.

Never use a `PriceFeed` contract ID you are handed without checking it. Call
`fetchAttestedPrice` from your own choice instead of `fetch`:

```daml
import DA.Time (hours)
import Governance.PriceFeed (fetchAttestedPrice)

feed <- fetchAttestedPrice committeeParty "tbill-usd" (hours 1) feedCid
-- feed.price is the committee's confirmed price, observed at most an hour ago
```

It fails unless the feed is signed by the committee party you expect, has the
`feedId` you expect, and its `observedAt` is no older than the maximum age. The age
check matters because a feed keeps its last price until the next update.

Apps read the feed as subscribers (observers) and pass its contract ID to their own
choices. Because each update replaces the contract, a superseded feed contract can
no longer be fetched.

## Time

Both price-carrying proposals have `observedAt` (when the price was observed) and
`executeBefore` (a deadline). Execution fails if `now > executeBefore` or if
`observedAt` lies in the future. The feed records the proposal's `observedAt`, not
the execution time, so a vote executed late cannot make an old price look fresh.

## One active feed per id

Daml LF 2.x has no contract keys, so the ledger cannot enforce a unique `feedId` by
itself. `PriceFeedRegistry` does: opening a feed registers its id and fails if the id
is already open, and closing a feed releases the id. Without it, governance could
open a second `tbill-usd` feed while the first stays active at its last price, and a
borrower could pass whichever is better.

Deploy **exactly one** registry per governance party, before the first feed opens,
and open feeds only through `OpenPriceFeedProposal`. The registry has two fields,
both supported by the `POST /contracts` field types, so it is created like
`GovernanceRules`:

```bash
curl -X POST http://coordinator:8080/contracts \
  -H 'Content-Type: application/json' \
  -d '{
    "decentralized_party_id": "price-committee::1220abc...",
    "participant_ids":   ["node1::1220...", "node2::1220...", "node3::1220..."],
    "participant_parties": ["member1::1220...", "member2::1220...", "member3::1220..."],
    "operator_party":    "operator::1220...",
    "contracts": [{
      "id":          "price-feed-registry",
      "name":        "PriceFeedRegistry",
      "package_id":  "#governance-price-feed-v1",
      "module_name": "Governance.PriceFeed",
      "entity_name": "PriceFeedRegistry",
      "fields": [
        { "type": "decentralized_party" },
        { "type": "none" }
      ]
    }]
  }'
```

`openFeedIds` is `Optional (Set Text)`, and `None` means no feed is open; the
field-type table has no `Set Text` type, the same reason `GovernanceRules` starts
`additionalProposers` as `None`.

Open and close proposals name the registry's contract ID, and every open or close
replaces it. So when two of them are in flight, the second fails at execute and must
be re-proposed against the new registry. The same holds for publish, subscriber and
close proposals that name a feed contract which has since been updated.

The other proposals carry `Decimal`, `Time` and `Set Party` fields that have no
field type, so members create them through the Ledger API (Path B in the guide).

## Subscribers

`subscribers` is a `Set Party`. All subscribers observe the same contract, so each
subscriber sees the full subscriber list: a lending app learns which other apps and
counterparties use the feed. An app that must stay confidential should get its own
feed (a separate `feedId` with only that app subscribed).

`SetPriceFeedSubscribersProposal` replaces the list. A removed subscriber stops
seeing new prices but keeps any contracts it has already seen.

## Build and test

```bash
cd daml/governance-price-feed && dpm build
cd ../governance-price-feed-test && dpm build && dpm test
```

The tests run the real `GovernanceRules` with a 2-of-3 committee in the
given/when/then style, one outcome per script, and assert the exact failure message
of each rejected path: threshold, superseded feed, misstated round, price, asset or
feed id, the execution window, the `ensure` clauses, proposer and confirmer
membership, proposer cancel, duplicate open, close and reopen, subscriber changes,
and `fetchAttestedPrice` rejecting a forged, wrong-id or stale feed. CI runs them in
the Daml Build & Test job.

## Used by

[Veil](https://github.com/no-witness-labs/veil-lite-hackathon) (private secured
lending on Canton) runs its valuation agent as a 2-of-3 decentralized party with
the same pattern: its `committee/` package drives a real loan's margin call from
a committee-confirmed price, with a reproducible LocalNet demo on this
repository's `hackathon/` stack.
