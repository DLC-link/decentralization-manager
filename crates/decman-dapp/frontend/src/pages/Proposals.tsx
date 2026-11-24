import { isValidSignature } from '../checks'
import { signProposal, submitProposal } from '../mock/actions'
import { actingWallet, useAppState, type Proposal, type Wallet } from '../mock/store'
import { ErrorText, Hex, PartyId, Section, useAction } from '../ui'

export function Proposals({ onOpenParty }: { onOpenParty: (partyId: string) => void }) {
  const state = useAppState()
  const acting = actingWallet(state)
  return (
    <Section title="Party proposals">
      <p className="hint">
        Owners sign at different times. The browser checks every signature with the Rust code before it counts it.
        Every owner must sign before the party exists.
      </p>
      {state.proposals.length === 0 && <p>No proposals yet.</p>}
      {state.proposals.map((proposal) => (
        <ProposalCard
          key={proposal.id}
          proposal={proposal}
          wallets={state.wallets}
          acting={acting}
          onOpenParty={onOpenParty}
        />
      ))}
    </Section>
  )
}

function ProposalCard({
  proposal,
  wallets,
  acting,
  onOpenParty,
}: {
  proposal: Proposal
  wallets: Wallet[]
  acting: Wallet | undefined
  onOpenParty: (partyId: string) => void
}) {
  const [error, run] = useAction()
  const { plan } = proposal
  const signatureOf = (publicKey: string) => proposal.signatures.find((s) => s.publicKey === publicKey)
  const verified = plan.owners.filter((owner) => {
    const signature = signatureOf(owner.publicKey)
    return signature && isValidSignature(owner.publicKey, plan.digest, signature.signature)
  }).length
  const actingIsOwner = acting && plan.owners.some((o) => o.publicKey === acting.publicKey)
  const actingSigned = acting && signatureOf(acting.publicKey)

  return (
    <article className="card">
      <h3>
        <PartyId value={plan.partyId} />
      </h3>
      <p>
        Proposed by {proposal.createdBy}. Signing threshold {plan.signingThreshold} of {plan.owners.length}, confirmation
        threshold {plan.confirmationThreshold} of {plan.hosts.length} hosts.
      </p>
      <p>
        Digest: <Hex value={plan.digest} />
      </p>
      <table>
        <thead>
          <tr>
            <th>Owner</th>
            <th>Participant</th>
            <th>Signature</th>
          </tr>
        </thead>
        <tbody>
          {plan.owners.map((owner) => {
            const signature = signatureOf(owner.publicKey)
            const name = wallets.find((w) => w.publicKey === owner.publicKey)?.name ?? owner.namespace
            const status = !signature
              ? 'waiting'
              : isValidSignature(owner.publicKey, plan.digest, signature.signature)
                ? '✓ verified in the browser'
                : '✗ does not verify'
            return (
              <tr key={owner.publicKey}>
                <td>{name}</td>
                <td>{owner.participant}</td>
                <td>{status}</td>
              </tr>
            )
          })}
        </tbody>
      </table>
      <p>
        {verified} of {plan.owners.length} owners signed.
      </p>
      {proposal.status === 'submitted' ? (
        <button onClick={() => onOpenParty(plan.partyId)}>Open party</button>
      ) : (
        <div className="actions">
          {actingIsOwner && !actingSigned && (
            <button onClick={() => run(() => signProposal(proposal.id, acting))}>Sign as {acting.name}</button>
          )}
          {actingIsOwner && verified === plan.owners.length && (
            <button onClick={() => run(() => submitProposal(proposal.id, acting))}>
              Create the party through {acting.participant}
            </button>
          )}
          {!actingIsOwner && <em>Act as an owner to sign.</em>}
        </div>
      )}
      <ErrorText error={error} />
    </article>
  )
}
