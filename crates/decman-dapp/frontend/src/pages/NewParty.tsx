import { useState } from 'react'

import { tryPlanParty, type PartyProposal } from '../checks'
import { actingWallet, useAppState } from '../mock/store'
import { createProposal } from '../onboarding'
import { shortUid } from '../format'
import { ErrorText, Hex, PartyId, Section, Uid, useAction } from '../ui'

// The Rust side reads thresholds as u32. A whole number in that range always
// parses, so the form shows the Rust range check rather than a parse error.
const MAX_U32 = 4_294_967_295

function threshold(value: number): number {
  return Number.isFinite(value) ? Math.min(MAX_U32, Math.max(1, Math.round(value))) : 1
}

export function NewParty({ onCreated }: { onCreated: () => void }) {
  const state = useAppState()
  const acting = actingWallet(state)
  const [hint, setHint] = useState('')
  const [ownerIds, setOwnerIds] = useState<string[]>([])
  const [covalidatorIds, setCovalidatorIds] = useState<string[]>([])
  const [signingThreshold, setSigningThreshold] = useState(2)
  // Null follows the signing threshold. A typed value replaces it.
  const [chosenNamespace, setChosenNamespace] = useState<number | null>(null)
  // Null follows the default of one host fewer than the hosts, so that one
  // host can be offline. A typed value replaces it.
  const [chosenConfirmation, setChosenConfirmation] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, run] = useAction()

  const owners = state.wallets.filter((w) => ownerIds.includes(w.id))
  const covalidators = state.covalidators
    .filter((c) => covalidatorIds.includes(c.id))
    .map((c) => c.participant.participantId)
  const hostCount = new Set([...owners.map((w) => w.participant.participantId), ...covalidators]).size
  const namespaceThreshold = chosenNamespace ?? signingThreshold
  const confirmationThreshold = chosenConfirmation ?? Math.max(1, hostCount - 1)
  const proposal: PartyProposal = {
    partyHint: hint,
    owners: owners.map((w) => ({ publicKey: w.publicKey, participant: w.participant.participantId })),
    signingThreshold: threshold(signingThreshold),
    namespaceThreshold: threshold(namespaceThreshold),
    confirmationThreshold: threshold(confirmationThreshold),
    covalidators,
  }
  const check = tryPlanParty(proposal)

  const toggle = (ids: string[], id: string) => (ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id])

  const create = () =>
    run(async () => {
      if (!acting) throw new Error('Pick the wallet you act as first.')
      setBusy(true)
      try {
        await createProposal(proposal, acting, covalidatorIds)
      } finally {
        setBusy(false)
      }
      onCreated()
    })

  return (
    <>
      <Section title="New multi-owner party">
        <div className="form">
          <label>
            Party name
            <input value={hint} onChange={(e) => setHint(e.target.value)} placeholder="treasury" />
          </label>
          <fieldset>
            <legend>Owners</legend>
            {state.wallets.length === 0 && <p>Add wallets on the Wallets page first.</p>}
            {state.wallets.map((wallet) => (
              <label key={wallet.id} className="inline">
                <input
                  type="checkbox"
                  checked={ownerIds.includes(wallet.id)}
                  onChange={() => setOwnerIds((ids) => toggle(ids, wallet.id))}
                />
                {wallet.name} ({shortUid(wallet.participant.participantId)})
              </label>
            ))}
          </fieldset>
          <fieldset>
            <legend>Covalidators (optional)</legend>
            {state.covalidators.length === 0 && <p>Add covalidators on the Wallets page.</p>}
            {state.covalidators.map((covalidator) => (
              <label key={covalidator.id} className="inline">
                <input
                  type="checkbox"
                  checked={covalidatorIds.includes(covalidator.id)}
                  onChange={() => setCovalidatorIds((ids) => toggle(ids, covalidator.id))}
                />
                {covalidator.name} ({shortUid(covalidator.participant.participantId)})
              </label>
            ))}
          </fieldset>
          <label>
            Signing threshold: owner signatures that later actions need
            <input
              type="number"
              min={1}
              value={signingThreshold}
              onChange={(e) => setSigningThreshold(Number(e.target.value))}
              onBlur={() => setSigningThreshold(threshold(signingThreshold))}
            />
            {owners.length > 1 && proposal.signingThreshold === owners.length && (
              <span className="warning">
                Every owner must sign each transaction, so one unavailable owner blocks them all.
              </span>
            )}
          </label>
          <label>
            Namespace threshold: owners needed to change the party's topology
            <input
              type="number"
              min={1}
              value={namespaceThreshold}
              onChange={(e) => setChosenNamespace(Number(e.target.value))}
              onBlur={() => chosenNamespace !== null && setChosenNamespace(threshold(chosenNamespace))}
            />
            <span className="hint">
              Every party with the same owners shares one namespace, so this threshold applies to all of them. It
              follows the signing threshold until you change it.
            </span>
            {owners.length > 1 && proposal.namespaceThreshold === 1 && (
              <span className="warning">Any single owner can change the owners and the party's keys.</span>
            )}
            {owners.length > 1 && proposal.namespaceThreshold === owners.length && (
              <span className="warning">One lost owner key blocks replacing an owner.</span>
            )}
            {proposal.namespaceThreshold < proposal.signingThreshold && (
              <span className="warning">
                Owners at the namespace threshold can rewrite the signing keys, so the signing threshold protects no
                more than this one.
              </span>
            )}
          </label>
          <label>
            Confirmation threshold: hosts that must confirm each transaction
            <input
              type="number"
              min={1}
              value={confirmationThreshold}
              onChange={(e) => setChosenConfirmation(Number(e.target.value))}
              onBlur={() => chosenConfirmation !== null && setChosenConfirmation(threshold(chosenConfirmation))}
            />
            {hostCount > 1 && (
              <span className="hint">
                The default is one fewer than the {hostCount} hosts, so one host can be offline.
              </span>
            )}
            {hostCount > 1 && proposal.confirmationThreshold === hostCount && (
              <span className="warning">
                Every host must confirm each transaction, so one offline host blocks them all.
              </span>
            )}
          </label>
        </div>
      </Section>
      <Section title="Check">
        <p className="hint">The Rust checks run in the browser on every change.</p>
        {'error' in check ? (
          <p className="error">{check.error}</p>
        ) : (
          <dl>
            <dt>Party id</dt>
            <dd>
              <PartyId value={check.plan.partyId} />
            </dd>
            <dt>Namespace owners</dt>
            <dd>
              {check.plan.owners.map((owner) => (
                <div key={owner.namespace}>
                  <Hex value={owner.namespace} />
                </div>
              ))}
            </dd>
            <dt>Thresholds</dt>
            <dd>
              <div>
                Signing {check.plan.signingThreshold} of {check.plan.owners.length} owners
              </div>
              <div>
                Namespace {check.plan.namespaceThreshold} of {check.plan.owners.length} owners
              </div>
              <div>
                Confirmation {check.plan.confirmationThreshold} of {check.plan.hosts.length} hosts
              </div>
            </dd>
            <dt>Hosts</dt>
            <dd>
              {check.plan.hosts.map((host) => (
                <div key={host}>
                  <Uid value={host} />
                </div>
              ))}
            </dd>
          </dl>
        )}
        <p className="hint">
          Proposing asks the proposer's participant for its synchronizer and topology format. The proposer then signs
          the real topology transactions, and the other owners sign after.
        </p>
        <button disabled={'error' in check || busy} onClick={create}>
          {busy ? 'Asking the participant…' : `Propose and sign as ${acting?.name ?? '…'}`}
        </button>
        <ErrorText error={error} />
      </Section>
    </>
  )
}
