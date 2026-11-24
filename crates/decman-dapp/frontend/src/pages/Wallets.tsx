import { useState } from 'react'

import { createWallet, setActingWallet } from '../mock/actions'
import { useAppState } from '../mock/store'
import { ErrorText, Hex, Section, useAction } from '../ui'

export function Wallets() {
  const state = useAppState()
  const [name, setName] = useState('')
  const [participant, setParticipant] = useState('')
  const [error, run] = useAction()

  const add = () =>
    run(async () => {
      if (!name.trim() || !participant.trim()) throw new Error('Enter a name and a participant.')
      await createWallet(name.trim(), participant.trim())
      setName('')
      setParticipant('')
    })

  return (
    <>
      <Section title="Owner wallets">
        <p className="hint">
          Each owner has a wallet with an Ed25519 key, and a participant node that hosts the party for them. In this
          mock, the browser keeps every wallet, so you can act as each owner in turn.
        </p>
        {state.wallets.length === 0 ? (
          <p>No wallets yet. Add at least two.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Participant</th>
                <th>Namespace</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {state.wallets.map((wallet) => (
                <tr key={wallet.id}>
                  <td>{wallet.name}</td>
                  <td>{wallet.participant}</td>
                  <td>
                    <Hex value={wallet.namespace} />
                  </td>
                  <td>
                    {wallet.id === state.actingWalletId ? (
                      <em>acting</em>
                    ) : (
                      <button onClick={() => setActingWallet(wallet.id)}>Act as</button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>
      <Section title="Add a wallet">
        <div className="form">
          <label>
            Name
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Alice" />
          </label>
          <label>
            Participant
            <input
              value={participant}
              onChange={(e) => setParticipant(e.target.value)}
              placeholder="participant-a"
            />
          </label>
          <button onClick={add}>Generate key and add</button>
        </div>
        <ErrorText error={error} />
      </Section>
    </>
  )
}
