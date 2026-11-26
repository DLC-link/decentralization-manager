// Calls to a participant's JSON Ledger API. Each call names the participant
// in its error, and keeps the participant's own error text.
import { simulatedCall } from './mock/participants'

/**
 * A participant's JSON Ledger API. `simulated` is fixed when the record is
 * created, so a switch flipped later cannot send its calls elsewhere.
 */
export type Connection = { apiUrl: string; token: string; simulated: boolean }

// A participant that never answers would otherwise hold a call, and with it
// the hosting deadline, forever.
const CALL_TIMEOUT_MS = 30_000

export type ConnectedSynchronizer = { synchronizerAlias: string; synchronizerId: string }

/**
 * Whether an HTTP status shows that the participant refused the request. A
 * 5xx can come from a proxy or a gateway after the request reached the
 * participant. So can 408, a gateway's timeout, and 429, a gateway's rate
 * limit. Any other 4xx is the participant's own answer.
 */
export function isDefiniteRefusal(status: number): boolean {
  return status >= 400 && status < 500 && status !== 408 && status !== 429
}

/** A participant's error answer, kept whole so that callers can classify it. */
export class LedgerError extends Error {
  readonly status: number
  readonly answer: unknown

  constructor(message: string, status: number, answer: unknown) {
    super(message)
    this.status = status
    this.answer = answer
  }

  /** Whether the participant itself refused the request. */
  get isRefusal(): boolean {
    return isDefiniteRefusal(this.status)
  }
}

async function call(connection: Connection, method: 'GET' | 'POST', path: string, body?: unknown): Promise<unknown> {
  if (connection.simulated) return simulatedCall(connection.apiUrl, method, path, body)
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
    throw new LedgerError(
      `${method} ${url} failed with ${response.status}: ${participantError(text)}`,
      response.status,
      parsed(text),
    )
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
