import { useEffect, useState } from 'react'

import { setActingWallet } from './mock/actions'
import { resetState, useAppState } from './mock/store'
import { NewParty } from './pages/NewParty'
import { Parties, PartyDetail } from './pages/Parties'
import { Proposals } from './pages/Proposals'
import { Wallets } from './pages/Wallets'

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

export function App() {
  const state = useAppState()
  const [route, navigate] = useHashRoute()
  const openParty = (partyId: string) => navigate(`party/${partyId}`)
  const pending = state.proposals.filter((p) => p.status === 'collecting').length

  const tabs: [string, string][] = [
    ['wallets', `Wallets (${state.wallets.length})`],
    ['new', 'New party'],
    ['proposals', `Proposals (${pending})`],
    ['parties', `Parties (${state.parties.length})`],
  ]

  return (
    <main>
      <header>
        <h1>DecMan dApp</h1>
        <p className="banner">
          Mock mode. The wallets, the ledger and the dApps are simulated in this browser. The party ids, digests and
          signature checks are real, and they run in Rust compiled to WebAssembly.
        </p>
        <div className="row">
          <label className="inline">
            Acting as
            <select
              value={state.actingWalletId ?? ''}
              onChange={(e) => setActingWallet(e.target.value)}
              disabled={state.wallets.length === 0}
            >
              {state.wallets.length === 0 && <option value="">no wallets</option>}
              {state.wallets.map((wallet) => (
                <option key={wallet.id} value={wallet.id}>
                  {wallet.name} ({wallet.participant})
                </option>
              ))}
            </select>
          </label>
          <button
            onClick={() => {
              if (window.confirm('Delete every mock wallet, proposal and party in this browser?')) {
                resetState()
                navigate('wallets')
              }
            }}
          >
            Reset mock data
          </button>
        </div>
        <nav>
          {tabs.map(([key, label]) => (
            <button key={key} className={route === key ? 'active' : ''} onClick={() => navigate(key)}>
              {label}
            </button>
          ))}
        </nav>
      </header>
      {route === 'wallets' && <Wallets />}
      {route === 'new' && <NewParty onCreated={() => navigate('proposals')} />}
      {route === 'proposals' && <Proposals onOpenParty={openParty} />}
      {route === 'parties' && <Parties onOpenParty={openParty} />}
      {route.startsWith('party/') && <PartyDetail partyId={route.slice('party/'.length)} />}
    </main>
  )
}
