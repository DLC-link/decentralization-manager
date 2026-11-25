// The party wallet's simulated parts: holdings, dApp connections and
// transfer requests. The digests and signature checks they call are the real
// Rust ones. Party creation is real and lives in ../onboarding.ts.
import { shortUid } from '../format'
import { isValidSignature, payloadDigest } from '../checks'
import { signDigest } from './keys'
import { getState, update, type Party, type Signature, type TransferRequest, type Wallet } from './store'

const TRANSFER_DOMAIN = 'decman-dapp/transfer-request/v1'

const now = () => new Date().toISOString()
const newId = () => crypto.randomUUID()

/** The public keys among `allowed` whose signature on `digest` verifies. */
export function verifiedSigners(
  digest: string,
  signatures: Signature[],
  allowed: string[],
): Set<string> {
  return new Set(
    signatures
      .filter((s) => allowed.includes(s.publicKey))
      .filter((s) => isValidSignature(s.publicKey, digest, s.signature))
      .map((s) => s.publicKey),
  )
}

export function setActingWallet(id: string): void {
  update((s) => ({ ...s, actingWalletId: id }))
}

// ---- Party wallet ----------------------------------------------------------

function changeParty(partyId: string, change: (party: Party) => Party): void {
  update((s) => ({
    ...s,
    parties: s.parties.map((p) => (p.partyId === partyId ? change(p) : p)),
  }))
}

function findParty(partyId: string): Party {
  const party = getState().parties.find((p) => p.partyId === partyId)
  if (!party) throw new Error('The party no longer exists.')
  return party
}

function withHistory(party: Party, text: string): Party {
  return { ...party, history: [{ at: now(), text }, ...party.history] }
}

function parseAmount(amount: string): number {
  const value = Number(amount)
  if (!Number.isFinite(value) || value <= 0) throw new Error('The amount must be a positive number.')
  return value
}

export function receiveHoldings(partyId: string, instrument: string, amount: string): void {
  const value = parseAmount(amount)
  const name = instrument.trim()
  if (!name) throw new Error('Enter an instrument.')
  changeParty(partyId, (p) =>
    withHistory(
      { ...p, holdings: { ...p.holdings, [name]: (p.holdings[name] ?? 0) + value } },
      `Received ${value} ${name}.`,
    ),
  )
}

export function requestConnection(partyId: string, dappName: string, dappUrl: string): void {
  if (!dappName.trim()) throw new Error('Enter the dApp name.')
  changeParty(partyId, (p) =>
    withHistory(
      {
        ...p,
        connections: [
          ...p.connections,
          { id: newId(), dappName: dappName.trim(), dappUrl: dappUrl.trim(), status: 'pending', createdAt: now() },
        ],
      },
      `${dappName.trim()} asked to connect.`,
    ),
  )
}

export function decideConnection(
  partyId: string,
  connectionId: string,
  approve: boolean,
  wallet: Wallet,
): void {
  const connection = findParty(partyId).connections.find((c) => c.id === connectionId)
  if (!connection) throw new Error('The connection request no longer exists.')
  changeParty(partyId, (p) =>
    withHistory(
      {
        ...p,
        connections: p.connections.map((c) =>
          c.id === connectionId ? { ...c, status: approve ? 'connected' : 'rejected' } : c,
        ),
      },
      `${wallet.name} ${approve ? 'approved' : 'rejected'} the connection from ${connection.dappName}.`,
    ),
  )
}

export function requestTransfer(
  partyId: string,
  connectionId: string,
  receiver: string,
  instrument: string,
  amount: string,
): void {
  const party = findParty(partyId)
  const connection = party.connections.find((c) => c.id === connectionId && c.status === 'connected')
  if (!connection) throw new Error('Only a connected dApp can send a request.')
  if (!receiver.trim()) throw new Error('Enter a receiver.')
  if (!instrument.trim()) throw new Error('Enter an instrument.')
  const value = parseAmount(amount)
  const id = newId()
  // Owners sign exactly this text, and the app shows it to them.
  const payload = JSON.stringify(
    {
      kind: 'transfer',
      requestId: id,
      sender: partyId,
      receiver: receiver.trim(),
      instrument: instrument.trim(),
      amount: value,
      requestedBy: connection.dappName,
    },
    null,
    2,
  )
  const request: TransferRequest = {
    id,
    connectionId,
    dappName: connection.dappName,
    receiver: receiver.trim(),
    instrument: instrument.trim(),
    amount: value,
    payload,
    digest: payloadDigest(TRANSFER_DOMAIN, payload),
    signatures: [],
    status: 'pending',
    createdAt: now(),
  }
  changeParty(partyId, (p) =>
    withHistory(
      { ...p, requests: [request, ...p.requests] },
      `${connection.dappName} asked to send ${value} ${request.instrument} to ${request.receiver}.`,
    ),
  )
}

function findRequest(partyId: string, requestId: string): TransferRequest {
  const request = findParty(partyId).requests.find((r) => r.id === requestId)
  if (!request) throw new Error('The request no longer exists.')
  return request
}

function changeRequest(
  partyId: string,
  requestId: string,
  change: (request: TransferRequest) => TransferRequest,
  history: string,
  holdings?: Record<string, number>,
): void {
  changeParty(partyId, (p) =>
    withHistory(
      {
        ...p,
        holdings: holdings ?? p.holdings,
        requests: p.requests.map((r) => (r.id === requestId ? change(r) : r)),
      },
      history,
    ),
  )
}

export async function signRequest(partyId: string, requestId: string, wallet: Wallet): Promise<void> {
  const party = findParty(partyId)
  const request = findRequest(partyId, requestId)
  if (!party.owners.some((o) => o.publicKey === wallet.publicKey)) {
    throw new Error(`${wallet.name} is not an owner of this party.`)
  }
  if (request.signatures.some((s) => s.publicKey === wallet.publicKey)) return
  const signature = await signDigest(wallet.privateKeyJwk, request.digest)
  const entry: Signature = { publicKey: wallet.publicKey, signature, signedAt: now() }
  changeRequest(
    partyId,
    requestId,
    (r) => ({ ...r, signatures: [...r.signatures, entry] }),
    `${wallet.name} signed the request from ${request.dappName}.`,
  )
}

export function executeRequest(partyId: string, requestId: string, wallet: Wallet): void {
  const party = findParty(partyId)
  const request = findRequest(partyId, requestId)
  const owners = party.owners.map((o) => o.publicKey)
  const signed = verifiedSigners(request.digest, request.signatures, owners).size
  if (signed < party.signingThreshold) {
    throw new Error(`The request needs ${party.signingThreshold} owner signatures. It has ${signed}.`)
  }
  const balance = party.holdings[request.instrument] ?? 0
  if (balance < request.amount) {
    const result = `Reported to ${request.dappName}: failed, the party holds only ${balance} ${request.instrument}.`
    changeRequest(partyId, requestId, (r) => ({ ...r, status: 'failed', result }), result)
    return
  }
  const updateId = newId().slice(0, 8)
  const result = `Reported to ${request.dappName}: completed in update ${updateId}.`
  changeRequest(
    partyId,
    requestId,
    (r) => ({ ...r, status: 'executed', result }),
    `${wallet.name} submitted the transfer through ${shortUid(wallet.participant.participantId)}. ${result}`,
    { ...party.holdings, [request.instrument]: balance - request.amount },
  )
}

export function rejectRequest(partyId: string, requestId: string, wallet: Wallet): void {
  const request = findRequest(partyId, requestId)
  const result = `Reported to ${request.dappName}: rejected by ${wallet.name}.`
  changeRequest(partyId, requestId, (r) => ({ ...r, status: 'rejected', result }), result)
}
