// Simulated participants, for trying the dApp without a localnet. When the
// switch is on, ledger.ts sends every JSON Ledger API call here instead of
// over the network. The Rust code still builds the real transactions and
// checks every signature. Only the participants' answers are made up.

const SWITCH_KEY = 'decman-dapp-simulated-participants'
// Canton writes topology in format v31 on protocol version 37, as the localnet does.
const SIMULATED_TOPOLOGY_FORMAT = 31

export function isSimulated(): boolean {
  try {
    return localStorage.getItem(SWITCH_KEY) === '1'
  } catch {
    return false
  }
}

export function setSimulated(on: boolean): void {
  try {
    if (on) localStorage.setItem(SWITCH_KEY, '1')
    else localStorage.removeItem(SWITCH_KEY)
  } catch {
    // Without storage the switch stays off.
  }
}

// One creation round at a time: every host's allocate call marks it, and the
// submitting owner's call (the one with signatures) completes the round.
const hosting = new Set<string>()
let roundComplete = false

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('')
}

/** A participant id in Canton's `name::namespace` form, stable for `apiUrl`. */
async function participantIdFor(apiUrl: string): Promise<string> {
  const name = apiUrl.replace(/\/+$/, '').split('/').pop()?.replace(/[^\w-]/g, '') || 'participant'
  return `${name}::1220${await sha256Hex(`participant ${apiUrl}`)}`
}

/** A versioned envelope that holds only the format number, which is all the probe reads. */
function envelopeWithFormat(format: number): string {
  return btoa(String.fromCharCode(0x10, format))
}

function hasSignatures(body: unknown): boolean {
  const signatures = (body as { multiHashSignatures?: unknown[] } | undefined)?.multiHashSignatures
  return Array.isArray(signatures) && signatures.length > 0
}

export async function simulatedCall(apiUrl: string, method: string, path: string, body?: unknown): Promise<unknown> {
  await sleep(300 + Math.random() * 400)
  const key = apiUrl.replace(/\/+$/, '')

  if (method === 'GET' && path === '/v2/parties/participant-id') {
    return { participantId: await participantIdFor(key) }
  }
  if (method === 'GET' && path === '/v2/state/connected-synchronizers') {
    return {
      connectedSynchronizers: [
        { synchronizerAlias: 'global', synchronizerId: `global-domain::1220${await sha256Hex('global-domain')}` },
      ],
    }
  }
  if (method === 'POST' && path === '/v2/parties/external/generate-topology') {
    return { topologyTransactions: [envelopeWithFormat(SIMULATED_TOPOLOGY_FORMAT)] }
  }
  if (method === 'POST' && path === '/v2/parties/external/allocate') {
    if (roundComplete && !hasSignatures(body)) {
      hosting.clear()
      roundComplete = false
    }
    hosting.add(key)
    if (hasSignatures(body)) roundComplete = true
    return {}
  }
  if (method === 'GET' && path.startsWith('/v2/parties/')) {
    const party = decodeURIComponent(path.slice('/v2/parties/'.length))
    const isLocal = roundComplete && hosting.has(key)
    return { partyDetails: isLocal ? [{ party, isLocal: true }] : [] }
  }
  throw new Error(`The simulated participant does not answer ${method} ${path}.`)
}
