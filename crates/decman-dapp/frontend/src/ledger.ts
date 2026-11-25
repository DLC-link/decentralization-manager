// Calls to a participant's JSON Ledger API. Each call names the participant
// in its error, and keeps the participant's own error text.
import { isSimulated, simulatedCall } from './mock/participants'

export type Connection = { apiUrl: string; token: string }

// A participant that never answers would otherwise hold a call, and with it
// the hosting deadline, forever.
const CALL_TIMEOUT_MS = 30_000

export type ConnectedSynchronizer = { synchronizerAlias: string; synchronizerId: string }

/** A participant's error answer, kept whole so that callers can classify it. */
export class LedgerError extends Error {
  readonly answer: unknown

  constructor(message: string, answer: unknown) {
    super(message)
    this.answer = answer
  }
}

async function call(connection: Connection, method: 'GET' | 'POST', path: string, body?: unknown): Promise<unknown> {
  if (isSimulated()) return simulatedCall(connection.apiUrl, method, path, body)
  const url = `${connection.apiUrl.replace(/\/+$/, '')}${path}`
  let response: Response
  try {
    response = await fetch(url, {
      method,
      headers: {
        Authorization: `Bearer ${connection.token}`,
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(CALL_TIMEOUT_MS),
    })
  } catch (error) {
    throw new Error(`Cannot reach ${url}: ${error instanceof Error ? error.message : String(error)}`)
  }
  const text = await response.text()
  if (!response.ok) {
    throw new LedgerError(`${method} ${url} failed with ${response.status}: ${participantError(text)}`, parsed(text))
  }
  return text ? (JSON.parse(text) as unknown) : {}
}

function parsed(text: string): unknown {
  try {
    return JSON.parse(text) as unknown
  } catch {
    return text
  }
}

/** The readable part of a JSON Ledger API error. */
function participantError(text: string): string {
  try {
    const error = JSON.parse(text) as { code?: string; cause?: string }
    if (error.cause) return error.code ? `${error.code}: ${error.cause}` : error.cause
  } catch {
    // Not JSON. The raw text is still the best explanation.
  }
  return text || 'the participant sent no details'
}

export async function participantId(connection: Connection): Promise<string> {
  const response = (await call(connection, 'GET', '/v2/parties/participant-id')) as { participantId?: string }
  if (!response.participantId) throw new Error('The participant did not return its id.')
  return response.participantId
}

export async function connectedSynchronizers(connection: Connection): Promise<ConnectedSynchronizer[]> {
  const response = (await call(connection, 'GET', '/v2/state/connected-synchronizers')) as {
    connectedSynchronizers?: ConnectedSynchronizer[]
  }
  return response.connectedSynchronizers ?? []
}

/** Builds a one-key party's topology. The participant allocates nothing. */
export function generateTopology(connection: Connection, request: unknown): Promise<unknown> {
  return call(connection, 'POST', '/v2/parties/external/generate-topology', request)
}

export function allocateExternalParty(connection: Connection, request: unknown): Promise<unknown> {
  return call(connection, 'POST', '/v2/parties/external/allocate', request)
}

export function getParty(connection: Connection, partyId: string): Promise<unknown> {
  return call(connection, 'GET', `/v2/parties/${encodeURIComponent(partyId)}`)
}
