# governance-price-feed-v1

A price feed run by a decentralized party. A new price is published only after
the governance threshold of members confirms the exact move, so no single
operator can set the price an app relies on.

| Template | Kind | What it does |
| --- | --- | --- |
| `PriceFeed` | contract | The current price of one asset, signed by the governance party and visible to its `subscribers`. Each update increments `round`. |
| `OpenPriceFeedProposal` | `GovernableAction` | Opens a feed at an initial price. |
| `PublishPriceProposal` | `GovernableAction` | Moves a feed from a stated round and price to a new price. If the feed changed after the proposal, execution fails. |

Both proposals follow the conventions in
[CUSTOM_DAML_TEMPLATES.md](../../docs/CUSTOM_DAML_TEMPLATES.md): `proposer` is the
signatory, `governanceParty` observes, and `executeImpl` runs with the governance
party's authority after `GovernanceRules_ExecuteConfirmedAction`.

Apps consume the feed by adding their party to `subscribers` (or by receiving the
`PriceFeed` as a disclosed contract) and passing its contract ID to their own
choices. Because each update replaces the contract, a consumer that fetches a
specific `ContractId PriceFeed` always gets a price that was confirmed by the
threshold.

## Build and test

```bash
cd daml/governance-price-feed && dpm build
cd ../governance-price-feed-test && dpm build && dpm test
```

The tests run the real `GovernanceRules` with a 2-of-3 committee: one confirmation
cannot publish, two can; a proposal against a superseded round or a misstated
price fails with valid confirmations; non-members cannot confirm; only
subscribers see the feed.

## Used by

[Veil](https://github.com/no-witness-labs/veil-lite-hackathon) (private secured
lending on Canton) runs its valuation agent as a 2-of-3 decentralized party with
the same pattern: its `committee/` package drives a real loan's margin call from
a committee-confirmed price, with a reproducible LocalNet demo on this
repository's `hackathon/` stack.
