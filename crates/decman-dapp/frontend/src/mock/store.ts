// The dApp's state in localStorage: wallets, covalidators, proposals and
// parties. Party creation is real, so a proposal records what each
// participant answered. The party's holdings, history and dApp connections
// stay simulated.
import { useSyncExternalStore } from 'react'

import type { Onboarding, PartyPlan, PartyProposal, PlannedOwner } from '../checks'
import type { Connection as LedgerConnection } from '../ledger'

/** A participant's JSON Ledger API and the id it reported. */
export type ParticipantLink = LedgerConnection & { participantId: string }

/** An owner. The browser key stands in for a wallet. */
export type Wallet = {
  id: string
  name: string
  participant: ParticipantLink
  publicKey: string
  namespace: string
  privateKeyJwk: JsonWebKey
}

/** A participant that hosts parties but holds no owner key. */
export type Covalidator = { id: string; name: string; participant: ParticipantLink }

export type Signature = { publicKey: string; signature: string; signedAt: string }

export type HostStep = 'waiting' | 'approved' | 'submitted' | 'hosted' | 'failed'

/** What one hosting participant did during the submission. */
export type HostResult = {
  participant: string
  name: string
  role: 'owner' | 'covalidator'
  step: HostStep
  detail?: string
}

export type Proposal = {
  id: string
  /** Whether the proposal uses simulated participants, fixed at creation. */
  simulated: boolean
  createdAt: string
  createdBy: string
  /** The proposer's public key. Only the proposer may withdraw. */
  proposerKey: string
  proposal: PartyProposal
  plan: PartyPlan
  synchronizerId: string
  topologyVersion: number
  onboarding: Onboarding
  signatures: Signature[]
  status: 'collecting' | 'submitting' | 'submitted' | 'failed' | 'withdrawn' | 'rejected'
  /** Who withdrew or rejected the proposal. */
  endedBy?: string
  hosts: HostResult[]
  error?: string
}

export type Host = { participant: string; role: 'owner' | 'covalidator'; approval: string }

export type Connection = {
  id: string
  dappName: string
  dappUrl: string
  status: 'pending' | 'connected' | 'rejected'
  createdAt: string
}

export type TransferRequest = {
  id: string
  connectionId: string
  dappName: string
  receiver: string
  instrument: string
  amount: number
  payload: string
  digest: string
  signatures: Signature[]
  status: 'pending' | 'executed' | 'rejected' | 'failed'
  result?: string
  createdAt: string
}

export type Party = {
  partyId: string
  /** Whether simulated participants host the party. */
  simulated: boolean
  namespace: string
  owners: PlannedOwner[]
  hosts: Host[]
  signingThreshold: number
  namespaceThreshold: number
  confirmationThreshold: number
  createdAt: string
  holdings: Record<string, number>
  connections: Connection[]
  requests: TransferRequest[]
  history: { at: string; text: string }[]
}

export type AppState = {
  actingWalletId: string | null
  wallets: Wallet[]
  covalidators: Covalidator[]
  proposals: Proposal[]
  parties: Party[]
}

// v3 records on each record whether its participants are simulated. Older
// data does not load.
const STORAGE_KEY = 'decman-dapp-v3'
const EMPTY: AppState = { actingWalletId: null, wallets: [], covalidators: [], proposals: [], parties: [] }

function load(): AppState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    const loaded: AppState = raw ? { ...EMPTY, ...(JSON.parse(raw) as Partial<AppState>) } : EMPTY
    // A reload stops a running submission, so it can no longer finish.
    return {
      ...loaded,
      proposals: loaded.proposals.map((p) =>
        p.status === 'submitting'
          ? { ...p, status: 'failed', error: 'The page reloaded during the submission. Try again.' }
          : p,
      ),
    }
  } catch {
    return EMPTY
  }
}

let state = load()
const listeners = new Set<() => void>()

export function getState(): AppState {
  return state
}

export function update(change: (current: AppState) => AppState): void {
  state = change(state)
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state))
  } catch {
    // Storage can be blocked, for example in a private window. The state then
    // lasts until the page reloads.
  }
  listeners.forEach((listener) => listener())
}

export function resetState(): void {
  update(() => EMPTY)
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useAppState(): AppState {
  return useSyncExternalStore(subscribe, getState)
}

export function actingWallet(current: AppState): Wallet | undefined {
  return current.wallets.find((wallet) => wallet.id === current.actingWalletId)
}
