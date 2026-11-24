// The mock's state: wallets, proposals and parties, kept in localStorage.
// The real dApp reads parties from the ledger and proposals from the
// coordination contracts instead.
import { useSyncExternalStore } from 'react'

import type { PartyPlan, PlannedOwner } from '../checks'

export type Wallet = {
  id: string
  name: string
  participant: string
  publicKey: string
  namespace: string
  privateKeyJwk: JsonWebKey
}

export type Signature = { publicKey: string; signature: string; signedAt: string }

export type Proposal = {
  id: string
  createdAt: string
  createdBy: string
  plan: PartyPlan
  signatures: Signature[]
  status: 'collecting' | 'submitted'
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
  namespace: string
  owners: PlannedOwner[]
  hosts: Host[]
  signingThreshold: number
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
  proposals: Proposal[]
  parties: Party[]
}

const STORAGE_KEY = 'decman-dapp-mock-v1'
const EMPTY: AppState = { actingWalletId: null, wallets: [], proposals: [], parties: [] }

function load(): AppState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    return raw ? { ...EMPTY, ...(JSON.parse(raw) as Partial<AppState>) } : EMPTY
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
