import { useState } from 'react'

import { isValidSignature } from '../checks'
import {
  decideConnection,
  executeRequest,
  receiveHoldings,
  rejectRequest,
  requestConnection,
  requestTransfer,
  signRequest,
} from '../mock/actions'
import { actingWallet, useAppState, type Party, type TransferRequest, type Wallet } from '../mock/store'
import { ErrorText, Hex, PartyId, Section, useAction } from '../ui'

export function Parties({ onOpenParty }: { onOpenParty: (partyId: string) => void }) {
  const state = useAppState()
  return (
    <Section title="Parties">
      {state.parties.length === 0 && <p>No parties yet. Create one from a proposal.</p>}
      <ul>
        {state.parties.map((party) => (
          <li key={party.partyId}>
            <button className="link" onClick={() => onOpenParty(party.partyId)}>
              <PartyId value={party.partyId} />
            </button>{' '}
            — {party.owners.length} owners, {party.hosts.length} hosts
          </li>
        ))}
      </ul>
    </Section>
  )
}

export function PartyDetail({ partyId }: { partyId: string }) {
  const state = useAppState()
  const party = state.parties.find((p) => p.partyId === partyId)
  const acting = actingWallet(state)
  if (!party) return <p>This party does not exist in the mock ledger.</p>
  const actingIsOwner = !!acting && party.owners.some((o) => o.publicKey === acting.publicKey)
  const nameOf = (publicKey: string) => state.wallets.find((w) => w.publicKey === publicKey)?.name ?? '—'

  return (
    <>
      <Section title="Party">
        <p>
          <PartyId value={party.partyId} />
        </p>
        <p>
          Signing threshold {party.signingThreshold} of {party.owners.length}. Confirmation threshold{' '}
          {party.confirmationThreshold} of {party.hosts.length}.
        </p>
        <h3>Owners</h3>
        <table>
          <thead>
            <tr>
              <th>Owner</th>
              <th>Namespace</th>
              <th>Participant</th>
            </tr>
          </thead>
          <tbody>
            {party.owners.map((owner) => (
              <tr key={owner.publicKey}>
                <td>{nameOf(owner.publicKey)}</td>
                <td>
                  <Hex value={owner.namespace} />
                </td>
                <td>{owner.participant}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <h3>Hosts</h3>
        <table>
          <thead>
            <tr>
              <th>Participant</th>
              <th>Role</th>
              <th>Hosting approval</th>
            </tr>
          </thead>
          <tbody>
            {party.hosts.map((host) => (
              <tr key={host.participant}>
                <td>{host.participant}</td>
                <td>{host.role}</td>
                <td>{host.approval}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <Holdings party={party} />
      <Connections party={party} acting={actingIsOwner ? acting : undefined} />
      <Requests party={party} acting={actingIsOwner ? acting : undefined} />
      <Section title="History">
        <ul className="history">
          {party.history.map((entry, index) => (
            <li key={index}>
              <time>{new Date(entry.at).toLocaleTimeString()}</time> {entry.text}
            </li>
          ))}
        </ul>
      </Section>
    </>
  )
}

function Holdings({ party }: { party: Party }) {
  const [instrument, setInstrument] = useState('CC')
  const [amount, setAmount] = useState('100')
  const [error, run] = useAction()
  const entries = Object.entries(party.holdings)
  return (
    <Section title="Holdings">
      {entries.length === 0 ? (
        <p>No holdings.</p>
      ) : (
        <table>
          <tbody>
            {entries.map(([name, value]) => (
              <tr key={name}>
                <td>{name}</td>
                <td className="number">{value}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <div className="form row">
        <input value={instrument} onChange={(e) => setInstrument(e.target.value)} aria-label="Instrument" />
        <input value={amount} onChange={(e) => setAmount(e.target.value)} aria-label="Amount" />
        <button onClick={() => run(() => receiveHoldings(party.partyId, instrument, amount))}>
          Simulate an incoming transfer
        </button>
      </div>
      <ErrorText error={error} />
    </Section>
  )
}

function Connections({ party, acting }: { party: Party; acting: Wallet | undefined }) {
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')
  const [error, run] = useAction()
  return (
    <Section title="Connected dApps">
      <p className="hint">
        The real dApp receives these over WalletConnect. Here, the form below plays the part of a dApp.
      </p>
      {party.connections.length === 0 && <p>No dApps.</p>}
      <ul>
        {party.connections.map((connection) => (
          <li key={connection.id}>
            <strong>{connection.dappName}</strong> {connection.dappUrl} — {connection.status}{' '}
            {connection.status === 'pending' && acting && (
              <>
                <button onClick={() => run(() => decideConnection(party.partyId, connection.id, true, acting))}>
                  Approve as {acting.name}
                </button>{' '}
                <button onClick={() => run(() => decideConnection(party.partyId, connection.id, false, acting))}>
                  Reject
                </button>
              </>
            )}
          </li>
        ))}
      </ul>
      <div className="form row">
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="dApp name" aria-label="dApp name" />
        <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://…" aria-label="dApp URL" />
        <button
          onClick={() =>
            run(() => {
              requestConnection(party.partyId, name, url)
              setName('')
              setUrl('')
            })
          }
        >
          Simulate a connection request
        </button>
      </div>
      <ErrorText error={error} />
    </Section>
  )
}

function Requests({ party, acting }: { party: Party; acting: Wallet | undefined }) {
  const connected = party.connections.filter((c) => c.status === 'connected')
  const [connectionId, setConnectionId] = useState('')
  const [receiver, setReceiver] = useState('')
  const [instrument, setInstrument] = useState('CC')
  const [amount, setAmount] = useState('10')
  const [error, run] = useAction()
  const chosen = connectionId || connected[0]?.id || ''

  return (
    <Section title="Incoming requests">
      {party.requests.length === 0 && <p>No requests.</p>}
      {party.requests.map((request) => (
        <RequestCard key={request.id} party={party} request={request} acting={acting} />
      ))}
      <h3>Simulate a transfer request from a dApp</h3>
      {connected.length === 0 ? (
        <p>Connect a dApp first.</p>
      ) : (
        <div className="form row">
          <select value={chosen} onChange={(e) => setConnectionId(e.target.value)} aria-label="dApp">
            {connected.map((c) => (
              <option key={c.id} value={c.id}>
                {c.dappName}
              </option>
            ))}
          </select>
          <input
            value={receiver}
            onChange={(e) => setReceiver(e.target.value)}
            placeholder="receiver party"
            aria-label="Receiver"
          />
          <input value={instrument} onChange={(e) => setInstrument(e.target.value)} aria-label="Instrument" />
          <input value={amount} onChange={(e) => setAmount(e.target.value)} aria-label="Amount" />
          <button onClick={() => run(() => requestTransfer(party.partyId, chosen, receiver, instrument, amount))}>
            Send request
          </button>
        </div>
      )}
      <ErrorText error={error} />
    </Section>
  )
}

function RequestCard({
  party,
  request,
  acting,
}: {
  party: Party
  request: TransferRequest
  acting: Wallet | undefined
}) {
  const [error, run] = useAction()
  const owners = party.owners.map((o) => o.publicKey)
  const verified = request.signatures.filter(
    (s) => owners.includes(s.publicKey) && isValidSignature(s.publicKey, request.digest, s.signature),
  ).length
  const actingSigned = acting && request.signatures.some((s) => s.publicKey === acting.publicKey)

  return (
    <article className="card">
      <p>
        <strong>{request.dappName}</strong> asks to send {request.amount} {request.instrument} to {request.receiver}.
        Status: {request.status}.
      </p>
      <details>
        <summary>What the owners sign</summary>
        <pre>{request.payload}</pre>
        <p>
          Digest: <Hex value={request.digest} />
        </p>
      </details>
      <p>
        {verified} of {party.signingThreshold} required owner signatures, each verified in the browser.
      </p>
      {request.status === 'pending' && acting && (
        <div className="actions">
          {!actingSigned && (
            <button onClick={() => run(() => signRequest(party.partyId, request.id, acting))}>
              Sign as {acting.name}
            </button>
          )}
          {verified >= party.signingThreshold && (
            <button onClick={() => run(() => executeRequest(party.partyId, request.id, acting))}>
              Submit through {acting.participant}
            </button>
          )}
          <button onClick={() => run(() => rejectRequest(party.partyId, request.id, acting))}>Reject</button>
        </div>
      )}
      {request.status === 'pending' && !acting && <em>Act as an owner to sign.</em>}
      {request.result && <p className="result">{request.result}</p>}
      <ErrorText error={error} />
    </article>
  )
}
