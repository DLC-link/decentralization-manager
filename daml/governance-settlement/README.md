# `governance-settlement-v1`

A Token Standard V2 batch settlement as a governable action. The batch
executes only once a decentralised party's members have confirmed it to
threshold.

Depends on `governance-action-v1` and the Splice V2 API packages, and on
nothing application-specific.

## How it works

`SettlementFactory_SettleBatch` is controlled by its actors, and the
standard's default implementation requires those actors to equal the
settlement's executors. An application that names a governance party among
the executors when it allocates has made every settlement of that batch
require a threshold of that party's members.

`BatchSettlementProposal` carries the batch through the vote. Authority passes
along the exercise chain: `GovernanceRules` to `GovernableAction_Execute` to
`executeImpl` to `SettlementFactory_SettleBatch`.

## Using it

1. Allocate both sides of the batch with the governance party and the
   proposer as the settlement's executors.
2. File a `BatchSettlementProposal` as the proposer, who must be a member or
   an additional proposer of the rules contract.
3. Members confirm to threshold.
4. Any member executes.

The `ensure` clause requires the settlement's executors to be exactly the
governance party and the proposer, because `executeImpl` holds no other
authority. It also requires non-empty legs and allocations, and unique
transfer leg ids.

## Limits

**Contract ids are captured when the proposal is filed.** `factoryCid`,
`allocationCids` and the ids inside `extraArgs` are fixed at that moment. If
the registry replaces any of them before execution, the settlement fails and
the proposer must file again. Keep action confirmation timeouts short.

**The factory is a contract id a member cannot judge.** `TransferLeg` carries
only a text instrument id, so the proposal has nothing in it to bind
`factoryCid` to. The standard's default settle checks every allocation's admin
against the factory's, so the wrong factory is refused, but at execution rather
than at filing.

**Execute a domain action through the API, not from its card.** A custom
`GovernableAction` such as this one appears in Approvals as a **domain**
action, and that card's Execute sends an empty `disclosed_contracts`
(`NotificationsView.tsx`, `DomainActionCard`). This action needs the
registry's rules contract disclosed, so use `POST /governance/execute` with
`disclosed_contracts` populated. The `ExecuteDialog` that does collect
disclosures belongs to the other card, for self and standard actions.

**One finalization shape, deliberately.** Every allocation is settled with
`extraTransferLegSides = []` and `nextIterationFunding = None`. V2 models both
per `FinalizedAllocation`, so a batch needing either cannot use this action as
written. Widening it means carrying `[V2.FinalizedAllocation]` in the proposal
instead of `[ContractId V2.Allocation]`; that is a small change and a
deliberate omission rather than an oversight, because it would put the
construction of those records on every caller for a case most do not have.

**The executors see every leg**, because allocations list their executors as
observers. That is a property of the standard, and an application asking a
committee to approve a payout should say so in its own terms.

## Tests

`governance-settlement-test` covers execution below threshold, execution by
the proposer outside the vote, a settlement at threshold paying every
receiver, and each of the three `ensure` conjuncts: the governance party
missing from the executors, the proposer missing from them, and a third party
present among them. It runs on the IDE ledger with no network.

Each refusal test is the passing case with exactly one value changed, so it
can only fail for the condition it names.
