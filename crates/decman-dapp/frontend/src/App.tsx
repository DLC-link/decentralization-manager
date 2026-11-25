import { useEffect, useState } from 'react'

import { shortUid } from './format'
import { setActingWallet } from './mock/actions'
import { isSimulated, setSimulated } from './mock/participants'
import { actingWallet, resetState, useAppState } from './mock/store'
import { NewParty } from './pages/NewParty'
import { Parties, PartyDetail } from './pages/Parties'
import { Proposals } from './pages/Proposals'
import { Wallets } from './pages/Wallets'
import { Avatar, Icon, type IconName } from './ui'

// The page lives in the URL hash, so a reload keeps it: #/wallets, #/new,
// #/proposals, #/parties, or #/party/<party id>.
function useHashRoute(): [string, (route: string) => void] {
  const read = () => decodeURIComponent(window.location.hash.replace(/^#\/?/, '')) || 'wallets'
  const [route, setRoute] = useState(read)
  useEffect(() => {
    const onChange = () => setRoute(read())
    window.addEventListener('hashchange', onChange)
    return () => window.removeEventListener('hashchange', onChange)
  }, [])
  const navigate = (next: string) => {
    window.location.hash = `/${encodeURIComponent(next).replace(/%2F/g, '/')}`
  }
  return [route, navigate]
}

const TITLES: Record<string, string> = {
  wallets: 'Wallets',
  new: 'New party',
  proposals: 'Proposals',
  parties: 'Parties',
}

export function App() {
  const state = useAppState()
  const acting = actingWallet(state)
  const [simulated, setSimulatedState] = useState(isSimulated)
  const [route, navigate] = useHashRoute()
  const openParty = (partyId: string) => navigate(`party/${partyId}`)
  const pending = state.proposals.filter((p) => p.status === 'collecting').length
  const page = route.startsWith('party/') ? 'parties' : route

  const tabs: [string, string, IconName, number][] = [
    ['wallets', 'Wallets', 'wallet', state.wallets.length],
    ['new', 'New party', 'plus', 0],
    ['proposals', 'Proposals', 'signature', pending],
    ['parties', 'Parties', 'party', state.parties.length],
  ]

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <span className="logo">
            <Icon name="party" />
          </span>
          <span>
            DecMan <span className="muted">dApp</span>
          </span>
        </div>
        <nav>
          {tabs.map(([key, label, icon, count]) => (
            <button key={key} className={page === key ? 'active' : ''} onClick={() => navigate(key)}>
              <Icon name={icon} />
              <span>{label}</span>
              {count > 0 && <span className="count">{count}</span>}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <label className="switch">
            <input
              type="checkbox"
              checked={simulated}
              onChange={(e) => {
                setSimulated(e.target.checked)
                setSimulatedState(e.target.checked)
              }}
            />
            <span className="track" />
            Simulated participants
          </label>
          <button
            className="ghost"
            onClick={() => {
              if (window.confirm('Delete every wallet, covalidator, proposal and party in this browser?')) {
                resetState()
                navigate('wallets')
              }
            }}
          >
            Reset browser data
          </button>
        </div>
      </aside>
      <div className="content">
        <header className="topbar">
          <h1>{route.startsWith('party/') ? 'Party' : (TITLES[route] ?? 'DecMan dApp')}</h1>
          <label className="acting">
            {acting ? <Avatar name={acting.name} /> : null}
            <span className="muted">Acting as</span>
            <select
              aria-label="Acting as"
              value={state.actingWalletId ?? ''}
              onChange={(e) => setActingWallet(e.target.value)}
              disabled={state.wallets.length === 0}
            >
              {state.wallets.length === 0 && <option value="">no wallets</option>}
              {state.wallets.map((wallet) => (
                <option key={wallet.id} value={wallet.id}>
                  {wallet.name} ({shortUid(wallet.participant.participantId)})
                </option>
              ))}
            </select>
          </label>
        </header>
        <div className="page">
          <p className="banner">
            {simulated
              ? 'The participants are simulated. The dApp builds the real transactions and checks every signature, but no participant receives them.'
              : 'Party creation is real. The dApp creates the party on your participants through the JSON Ledger API.'}{' '}
            Owner keys stay in this browser and stand in for wallets. Holdings, transfers, history and dApp connections
            are simulated.
          </p>
          {route === 'wallets' && <Wallets />}
          {route === 'new' && <NewParty onCreated={() => navigate('proposals')} />}
          {route === 'proposals' && <Proposals onOpenParty={openParty} />}
          {route === 'parties' && <Parties onOpenParty={openParty} />}
          {route.startsWith('party/') && <PartyDetail partyId={route.slice('party/'.length)} />}
        </div>
      </div>
    </div>
  )
}
