import { useId, useState } from 'react'

import { LOCALNET_PARTICIPANTS, LOCALNET_TOKEN } from '../localnet'
import { setActingWallet } from '../mock/actions'
import { useAppState, type ParticipantLink } from '../mock/store'
import { addCovalidator, createWallet } from '../onboarding'
import { ErrorText, Hex, Section, useAction } from '../ui'

export function Wallets() {
  const state = useAppState()
  return (
    <>
      <Section title="Owner wallets">
        <p className="hint">
          Each owner has an Ed25519 key and a participant that hosts the party for them. The browser keeps every key, so
          you can act as each owner in turn. The key stands in for a wallet.
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
                  <td>
                    <ParticipantCell link={wallet.participant} />
                  </td>
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
      <ParticipantForm
        title="Add a wallet"
        namePlaceholder="Alice"
        button="Generate key and add"
        onAdd={createWallet}
      />
      <Section title="Covalidators">
        <p className="hint">
          A covalidator hosts the party but holds no owner key. On localnet the dApp sends its hosting approval itself.
          That stands in for the covalidator's DecMan approval.
        </p>
        {state.covalidators.length === 0 ? (
          <p>No covalidators. They are optional.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Participant</th>
              </tr>
            </thead>
            <tbody>
              {state.covalidators.map((covalidator) => (
                <tr key={covalidator.id}>
                  <td>{covalidator.name}</td>
                  <td>
                    <ParticipantCell link={covalidator.participant} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Section>
      <ParticipantForm
        title="Add a covalidator"
        namePlaceholder="Covalidator 1"
        button="Add covalidator"
        onAdd={addCovalidator}
      />
    </>
  )
}

function ParticipantCell({ link }: { link: ParticipantLink }) {
  return (
    <span title={link.apiUrl}>
      <Hex value={link.participantId} />
    </span>
  )
}

/** Name, JSON Ledger API URL and token. Adding reads the participant id. */
function ParticipantForm({
  title,
  namePlaceholder,
  button,
  onAdd,
}: {
  title: string
  namePlaceholder: string
  button: string
  onAdd: (name: string, apiUrl: string, token: string) => Promise<void>
}) {
  const [name, setName] = useState('')
  const [apiUrl, setApiUrl] = useState(LOCALNET_PARTICIPANTS[0].apiUrl)
  const [token, setToken] = useState(LOCALNET_TOKEN)
  const [error, run] = useAction()
  const listId = useId()

  const add = () =>
    run(async () => {
      await onAdd(name, apiUrl, token)
      setName('')
    })

  return (
    <Section title={title}>
      <div className="form">
        <label>
          Name
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder={namePlaceholder} />
        </label>
        <label>
          Participant JSON Ledger API URL
          <input value={apiUrl} onChange={(e) => setApiUrl(e.target.value)} list={`${listId}-participants`} />
          <datalist id={`${listId}-participants`}>
            {LOCALNET_PARTICIPANTS.map((p) => (
              <option key={p.apiUrl} value={p.apiUrl}>
                {p.name}
              </option>
            ))}
          </datalist>
        </label>
        <label>
          Token (the localnet token by default)
          <input value={token} onChange={(e) => setToken(e.target.value)} />
        </label>
        <button onClick={add}>{button}</button>
      </div>
      <p className="hint">The dApp reads the participant id from the participant when you add it.</p>
      <ErrorText error={error} />
    </Section>
  )
}
