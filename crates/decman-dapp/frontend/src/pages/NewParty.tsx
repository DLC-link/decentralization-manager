import { useState } from 'react'

import { tryPlanParty, type PartyProposal } from '../checks'
import { createProposal } from '../mock/actions'
import { actingWallet, useAppState } from '../mock/store'
import { ErrorText, Hex, PartyId, Section, useAction } from '../ui'

export function NewParty({ onCreated }: { onCreated: () => void }) {
  const state = useAppState()
  const acting = actingWallet(state)
  const [hint, setHint] = useState('')
  const [ownerIds, setOwnerIds] = useState<string[]>([])
  const [signingThreshold, setSigningThreshold] = useState(2)
  const [confirmationThreshold, setConfirmationThreshold] = useState(1)
  const [covalidators, setCovalidators] = useState('')
  const [error, run] = useAction()

  const owners = state.wallets.filter((w) => ownerIds.includes(w.id))
  const proposal: PartyProposal = {
    partyHint: hint,
    owners: owners.map((w) => ({ publicKey: w.publicKey, participant: w.participant })),
    signingThreshold,
    confirmationThreshold,
    covalidators: covalidators.split(',').map((c) => c.trim()).filter(Boolean),
  }
  const check = tryPlanParty(proposal)

  const toggle = (id: string) =>
    setOwnerIds((ids) => (ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id]))

  const create = () =>
    run(() => {
      if (!acting) throw new Error('Pick the wallet you act as first.')
      createProposal(proposal, acting)
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
                <input type="checkbox" checked={ownerIds.includes(wallet.id)} onChange={() => toggle(wallet.id)} />
                {wallet.name} ({wallet.participant})
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
          <label>
            Covalidators: extra hosting participants, separated by commas (optional)
            <input
              value={covalidators}
              onChange={(e) => setCovalidators(e.target.value)}
              placeholder="covalidator-1"
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
            <dt>Hosts</dt>
            <dd>{check.plan.hosts.join(', ')}</dd>
            <dt>Digest that every owner signs</dt>
            <dd>
              <Hex value={check.plan.digest} />
            </dd>
          </dl>
        )}
        <button disabled={'error' in check} onClick={create}>
          Propose as {acting?.name ?? '…'}
        </button>
        <ErrorText error={error} />
      </Section>
    </>
  )
}
