import { useState } from 'react'

import { tryPlanParty, type PartyProposal } from '../checks'
import { actingWallet, useAppState } from '../mock/store'
import { createProposal } from '../onboarding'
import { shortUid } from '../format'
import { ErrorText, Hex, PartyId, Section, Uid, useAction } from '../ui'

export function NewParty({ onCreated }: { onCreated: () => void }) {
  const state = useAppState()
  const acting = actingWallet(state)
  const [hint, setHint] = useState('')
  const [ownerIds, setOwnerIds] = useState<string[]>([])
  const [covalidatorIds, setCovalidatorIds] = useState<string[]>([])
  const [signingThreshold, setSigningThreshold] = useState(2)
  const [confirmationThreshold, setConfirmationThreshold] = useState(1)
  const [busy, setBusy] = useState(false)
  const [error, run] = useAction()

  const owners = state.wallets.filter((w) => ownerIds.includes(w.id))
  const proposal: PartyProposal = {
    partyHint: hint,
    owners: owners.map((w) => ({ publicKey: w.publicKey, participant: w.participant.participantId })),
    signingThreshold,
    confirmationThreshold,
    covalidators: state.covalidators
      .filter((c) => covalidatorIds.includes(c.id))
      .map((c) => c.participant.participantId),
  }
  const check = tryPlanParty(proposal)

  const toggle = (ids: string[], id: string) => (ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id])

  const create = () =>
    run(async () => {
      if (!acting) throw new Error('Pick the wallet you act as first.')
      setBusy(true)
      try {
        await createProposal(proposal, acting)
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
            />
          </label>
          <label>
            Confirmation threshold: hosts that must confirm each transaction
            <input
              type="number"
              min={1}
              value={confirmationThreshold}
              onChange={(e) => setConfirmationThreshold(Number(e.target.value))}
            />
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
          Proposing asks the proposer's participant for its synchronizer and topology format. The owners then sign the
          real topology transactions.
        </p>
        <button disabled={'error' in check || busy} onClick={create}>
          {busy ? 'Asking the participant…' : `Propose as ${acting?.name ?? '…'}`}
        </button>
        <ErrorText error={error} />
      </Section>
    </>
  )
}
