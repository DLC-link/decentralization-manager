import { isValidSignature } from '../checks'
import { actingWallet, useAppState, type HostResult, type Proposal, type Wallet } from '../mock/store'
import { signProposal, submitProposal, verifiedOwners } from '../onboarding'
import { ErrorText, Hex, PartyId, Section, useAction } from '../ui'

export function Proposals({ onOpenParty }: { onOpenParty: (partyId: string) => void }) {
  const state = useAppState()
  const acting = actingWallet(state)
  return (
    <Section title="Party proposals">
      <p className="hint">
        Each owner signs one hash, which covers every topology transaction that creates the party. The browser checks
        every signature with the Rust code before it counts it. Every owner must sign.
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

const STEP_TEXT: Record<HostResult['step'], string> = {
  waiting: 'waiting',
  approved: 'approved the hosting',
  submitted: 'received the party',
  hosted: '✓ hosts the party',
  failed: '✗ failed',
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
  const { plan, onboarding } = proposal
  const signatureOf = (publicKey: string) => proposal.signatures.find((s) => s.publicKey === publicKey)
  const verified = verifiedOwners(proposal).size
  const actingIsOwner = acting && plan.owners.some((o) => o.publicKey === acting.publicKey)
  const actingSigned = acting && signatureOf(acting.publicKey)
  const canSubmit = actingIsOwner && verified === plan.owners.length && proposal.status !== 'submitting'

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
        Synchronizer <Hex value={proposal.synchronizerId} />, topology format {proposal.topologyVersion}.
      </p>
      <details>
        <summary>{onboarding.transactions.length} topology transactions</summary>
        <table>
          <thead>
            <tr>
              <th>Creates</th>
              <th>Hash</th>
            </tr>
          </thead>
          <tbody>
            {onboarding.transactions.map((transaction) => (
              <tr key={transaction.hash}>
                <td>{transaction.mapping}</td>
                <td>
                  <Hex value={transaction.hash} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </details>
      <p>
        Hash every owner signs: <Hex value={onboarding.combinedHash} />
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
              : isValidSignature(owner.publicKey, onboarding.combinedHash, signature.signature)
                ? '✓ verified in the browser'
                : '✗ does not verify'
            return (
              <tr key={owner.publicKey}>
                <td>{name}</td>
                <td>
                  <Hex value={owner.participant} />
                </td>
                <td>{status}</td>
              </tr>
            )
          })}
        </tbody>
      </table>
      <p>
        {verified} of {plan.owners.length} owners signed.
      </p>
      {proposal.status !== 'collecting' && <HostResults proposal={proposal} />}
      {proposal.error && <p className="error">{proposal.error}</p>}
      {proposal.status === 'submitted' ? (
        <button onClick={() => onOpenParty(plan.partyId)}>Open party</button>
      ) : (
        <div className="actions">
          {actingIsOwner && !actingSigned && (
            <button onClick={() => run(() => signProposal(proposal.id, acting))}>Sign as {acting.name}</button>
          )}
          {canSubmit && (
            <button onClick={() => run(() => submitProposal(proposal.id, acting))}>
              {proposal.status === 'failed' ? 'Try again' : 'Create the party'} through {acting.participant.participantId}
            </button>
          )}
          {proposal.status === 'submitting' && <em>Creating the party…</em>}
          {!actingIsOwner && <em>Act as an owner to sign.</em>}
        </div>
      )}
      <ErrorText error={error} />
    </article>
  )
}

function HostResults({ proposal }: { proposal: Proposal }) {
  return (
    <table>
      <thead>
        <tr>
          <th>Host</th>
          <th>Role</th>
          <th>Result</th>
          <th>Participant's answer</th>
        </tr>
      </thead>
      <tbody>
        {proposal.hosts.map((host) => (
          <tr key={host.participant}>
            <td title={host.participant}>{host.name}</td>
            <td>{host.role === 'owner' ? 'owner' : "covalidator (the dApp approves for its DecMan)"}</td>
            <td>{STEP_TEXT[host.step]}</td>
            <td className={host.step === 'failed' ? 'error' : undefined}>{host.detail ?? ''}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}
