import { isValidSignature } from '../checks'
import { actingWallet, useAppState, type HostResult, type Proposal, type Wallet } from '../mock/store'
import {
  isOpen,
  untouched,
  recheckProposal,
  rejectProposal,
  signProposal,
  submitProposal,
  verifiedOwners,
  withdrawProposal,
} from '../onboarding'
import {
  Badge,
  EmptyState,
  ErrorText,
  Hex,
  PartyId,
  Person,
  Section,
  SimulatedBadge,
  Stepper,
  Uid,
  useAction,
  type Tone,
} from '../ui'

export function Proposals({ onOpenParty }: { onOpenParty: (partyId: string) => void }) {
  const state = useAppState()
  const acting = actingWallet(state)
  return (
    <Section title="Party proposals">
      <p className="hint">
        Each owner signs one hash, which covers every topology transaction that creates the party. The browser checks
        every signature with the Rust code before it counts it. Every owner must sign.
      </p>
      {state.proposals.length === 0 && <EmptyState icon="signature">No proposals yet. Start one from New party.</EmptyState>}
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

const HOST_STEP: Record<HostResult['step'], [Tone, string]> = {
  waiting: ['muted', 'Waiting'],
  approved: ['accent', 'Approved hosting'],
  submitted: ['accent', 'Received the party'],
  hosted: ['ok', 'Hosts the party'],
  failed: ['error', 'Failed'],
}

const STATUS: Record<Proposal['status'], [Tone, string]> = {
  collecting: ['warn', 'Collecting signatures'],
  submitting: ['accent', 'Creating'],
  submitted: ['ok', 'Every host reports the party'],
  failed: ['error', 'Failed'],
  withdrawn: ['muted', 'Withdrawn'],
  rejected: ['muted', 'Rejected'],
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
  let checked: ReturnType<typeof recheckProposal>
  try {
    checked = recheckProposal(proposal)
  } catch (mismatch) {
    return (
      <article className="card">
        <h3>
          <PartyId value={proposal.plan.partyId} />
        </h3>
        <p className="error">{mismatch instanceof Error ? mismatch.message : String(mismatch)}</p>
      </article>
    )
  }
  const { plan, onboarding } = checked
  const signatureOf = (publicKey: string) => proposal.signatures.find((s) => s.publicKey === publicKey)
  const verifiedKeys = verifiedOwners(plan, onboarding, proposal.signatures)
  const verified = verifiedKeys.size
  const allSigned = verified === plan.owners.length
  const open = isOpen(proposal)
  const actingIsOwner = acting && plan.owners.some((o) => o.publicKey === acting.publicKey)
  const actingVerified = !!acting && verifiedKeys.has(acting.publicKey)
  const actingStoredBadly = !!acting && !actingVerified && !!signatureOf(acting.publicKey)
  const actingIsProposer = !!acting && acting.publicKey === proposal.proposerKey
  const canSubmit = actingIsOwner && allSigned && open
  const canEnd = untouched(proposal)
  const hostsApproved = proposal.hosts.every((h) => h.step !== 'waiting' && h.step !== 'failed')
  const current =
    proposal.status === 'submitted' ? 4 : !allSigned ? 1 : proposal.status === 'collecting' || !hostsApproved ? 2 : 3
  const [tone, label] = STATUS[proposal.status]

  return (
    <article className="card">
      <div className="card-head">
        <h3>
          <PartyId value={plan.partyId} />
        </h3>
        <span className="badges">
          {proposal.simulated && <SimulatedBadge />}
          <Badge tone={tone}>{label}</Badge>
        </span>
      </div>
      <Stepper
        steps={['Proposed', `Owners signed ${verified}/${plan.owners.length}`, 'Hosts approved', 'Every host reports the party']}
        current={current}
        failed={proposal.status === 'failed'}
      />
      <div className="chips">
        <span className="chip">Proposed by {proposal.createdBy}</span>
        <span className="chip">
          Signing {plan.signingThreshold} of {plan.owners.length}
        </span>
        <span className="chip">
          Namespace {plan.namespaceThreshold} of {plan.owners.length}
        </span>
        <span className="chip">
          Confirmation {plan.confirmationThreshold} of {plan.hosts.length} hosts
        </span>
        <span className="chip">Topology format v{proposal.topologyVersion}</span>
        <span className="chip">
          <Uid value={proposal.synchronizerId} />
        </span>
      </div>
      <div className="hosts">
        <span className="muted">Hosts</span>
        {plan.hosts.map((host) => (
          <span key={host} className="chip">
            <Uid value={host} />
          </span>
        ))}
      </div>
      <p className="signs">
        <span className="muted">Every owner signs</span> <Hex value={onboarding.combinedHash} />
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
            return (
              <tr key={owner.publicKey}>
                <td>
                  <Person name={name} />
                </td>
                <td>
                  <Uid value={owner.participant} />
                </td>
                <td>
                  {!signature ? (
                    <Badge tone="muted">Waiting</Badge>
                  ) : isValidSignature(owner.publicKey, onboarding.combinedHash, signature.signature) ? (
                    <Badge tone="ok">Verified in the browser</Badge>
                  ) : (
                    <Badge tone="error">Does not verify</Badge>
                  )}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
      {proposal.status !== 'collecting' && <HostResults proposal={proposal} />}
      {proposal.error && <p className="error">{proposal.error}</p>}
      {proposal.endedBy && (
        <p className="muted">
          {proposal.status === 'withdrawn' ? 'Withdrawn' : 'Rejected'} by {proposal.endedBy}.
        </p>
      )}
      {proposal.status === 'submitted' ? (
        <div className="actions">
          <button onClick={() => onOpenParty(plan.partyId)}>Open party</button>
        </div>
      ) : !open ? (
        proposal.status === 'submitting' && <p className="muted">Creating the party…</p>
      ) : (
        <div className="actions">
          {actingIsOwner && !actingVerified && (
            <button onClick={() => run(() => signProposal(proposal.id, acting))}>
              {actingStoredBadly ? 'Sign again' : 'Sign'} as {acting.name}
            </button>
          )}
          {actingIsOwner && !actingVerified && !actingIsProposer && canEnd && (
            <button
              className="ghost"
              onClick={() =>
                window.confirm('Reject this proposal? The party needs every owner, so this ends it.') &&
                run(() => rejectProposal(proposal.id, acting))
              }
            >
              Reject
            </button>
          )}
          {actingIsProposer && canEnd && !allSigned && (
            <button
              className="ghost"
              onClick={() =>
                window.confirm('Withdraw this proposal? No owner can sign it afterwards.') &&
                run(() => withdrawProposal(proposal.id, acting))
              }
            >
              Withdraw
            </button>
          )}
          {canSubmit && (
            <button onClick={() => run(() => submitProposal(proposal.id, acting))}>
              {proposal.status === 'failed' ? 'Try again' : 'Create the party'}
            </button>
          )}
          {!actingIsOwner && <span className="muted">Act as an owner to sign.</span>}
          {proposal.status === 'failed' && (
            <span className="muted">
              The signed bundle may already have reached participants, so the proposal can only be tried again.
            </span>
          )}
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
        {proposal.hosts.map((host) => {
          const [tone, label] = HOST_STEP[host.step]
          return (
            <tr key={host.participant}>
              <td title={host.participant}>
                <Person name={host.name} />
              </td>
              <td>{host.role === 'owner' ? 'Owner' : "Covalidator (the dApp approves for its DecMan)"}</td>
              <td>
                <Badge tone={tone}>{label}</Badge>
              </td>
              <td className={`detail ${host.step === 'failed' ? 'error' : 'muted'}`}>{host.detail ?? ''}</td>
            </tr>
          )
        })}
      </tbody>
    </table>
  )
}
