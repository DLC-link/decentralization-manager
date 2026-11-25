// Creating a party on real participants. The Rust code builds the topology
// transactions and the request bodies. This file talks to the participants
// and keeps each one's answer in the proposal.
import {
  allocationRequests,
  generateTopologyRequest,
  isValidSignature,
  onboardingTransactions,
  partyHostedLocally,
  planParty,
  publicKeyNamespace,
  topologyVersionOfGenerated,
  type PartyProposal,
} from './checks'
import { shortUid } from './format'
import * as ledger from './ledger'
import { generateKeyPair, signDigest } from './mock/keys'
import { isSimulated } from './mock/participants'
import {
  getState,
  update,
  type Covalidator,
  type HostResult,
  type HostStep,
  type Party,
  type ParticipantLink,
  type Proposal,
  type Signature,
  type Wallet,
} from './mock/store'

const HOSTING_TIMEOUT_MS = 120_000
const HOSTING_POLL_MS = 2_000

const now = () => new Date().toISOString()
const newId = () => crypto.randomUUID()
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))
const message = (error: unknown) => (error instanceof Error ? error.message : String(error))

async function linkParticipant(apiUrl: string, token: string): Promise<ParticipantLink> {
  const connection = { apiUrl: apiUrl.trim(), token: token.trim() }
  if (!connection.apiUrl) throw new Error('Enter the JSON Ledger API URL.')
  return { ...connection, participantId: await ledger.participantId(connection) }
}

// ---- Wallets and covalidators ----------------------------------------------

export async function createWallet(name: string, apiUrl: string, token: string): Promise<void> {
  if (!name.trim()) throw new Error('Enter a name.')
  const participant = await linkParticipant(apiUrl, token)
  const keys = await generateKeyPair()
  const wallet: Wallet = {
    id: newId(),
    name: name.trim(),
    participant,
    ...keys,
    namespace: publicKeyNamespace(keys.publicKey),
  }
  update((s) => ({
    ...s,
    wallets: [...s.wallets, wallet],
    actingWalletId: s.actingWalletId ?? wallet.id,
  }))
}

export async function addCovalidator(name: string, apiUrl: string, token: string): Promise<void> {
  if (!name.trim()) throw new Error('Enter a name.')
  const participant = await linkParticipant(apiUrl, token)
  const covalidator: Covalidator = { id: newId(), name: name.trim(), participant }
  update((s) => ({ ...s, covalidators: [...s.covalidators, covalidator] }))
}

/** The connection, name and role of the host with `participantId`. */
function hostOf(participantId: string): { link: ParticipantLink; name: string; role: HostResult['role'] } {
  const { wallets, covalidators } = getState()
  const owners = wallets.filter((w) => w.participant.participantId === participantId)
  if (owners.length > 0) {
    return { link: owners[0].participant, name: owners.map((w) => w.name).join(', '), role: 'owner' }
  }
  const covalidator = covalidators.find((c) => c.participant.participantId === participantId)
  if (covalidator) return { link: covalidator.participant, name: covalidator.name, role: 'covalidator' }
  throw new Error(`No wallet or covalidator uses participant ${participantId}.`)
}

// ---- Proposals -------------------------------------------------------------

/**
 * Checks the proposal, reads the synchronizer and its topology format from
 * the proposer's participant, and builds the transactions the owners sign.
 */
export async function createProposal(proposal: PartyProposal, creator: Wallet): Promise<void> {
  const plan = planParty(proposal)
  if (!plan.owners.some((o) => o.publicKey === creator.publicKey)) {
    throw new Error('Act as one of the owners to propose the party.')
  }
  const hosts = plan.hosts.map((participant): HostResult => {
    const { name, role } = hostOf(participant)
    return { participant, name, role, step: 'waiting' }
  })

  const synchronizers = await ledger.connectedSynchronizers(creator.participant)
  if (synchronizers.length !== 1) {
    throw new Error(
      `${creator.name}'s participant is connected to ${synchronizers.length} synchronizers. The dApp needs exactly one.`,
    )
  }
  const { synchronizerId } = synchronizers[0]
  const generated = await ledger.generateTopology(
    creator.participant,
    generateTopologyRequest(synchronizerId, creator.publicKey),
  )
  const topologyVersion = topologyVersionOfGenerated(generated)

  const entry: Proposal = {
    id: newId(),
    createdAt: now(),
    createdBy: creator.name,
    proposal,
    plan,
    synchronizerId,
    topologyVersion,
    onboarding: onboardingTransactions(proposal, topologyVersion),
    signatures: [],
    status: 'collecting',
    hosts,
  }
  update((s) => ({ ...s, proposals: [entry, ...s.proposals] }))
}

function findProposal(id: string): Proposal {
  const proposal = getState().proposals.find((p) => p.id === id)
  if (!proposal) throw new Error('The proposal no longer exists.')
  return proposal
}

function changeProposal(id: string, change: (proposal: Proposal) => Proposal): void {
  update((s) => ({ ...s, proposals: s.proposals.map((p) => (p.id === id ? change(p) : p)) }))
}

function setHost(id: string, participant: string, step: HostStep, detail?: string): void {
  changeProposal(id, (p) => ({
    ...p,
    hosts: p.hosts.map((h) => (h.participant === participant ? { ...h, step, detail } : h)),
  }))
}

/** The owners whose signature on the combined hash verifies. */
export function verifiedOwners(proposal: Proposal): Set<string> {
  const owners = proposal.plan.owners.map((o) => o.publicKey)
  return new Set(
    proposal.signatures
      .filter((s) => owners.includes(s.publicKey))
      .filter((s) => isValidSignature(s.publicKey, proposal.onboarding.combinedHash, s.signature))
      .map((s) => s.publicKey),
  )
}

export async function signProposal(id: string, wallet: Wallet): Promise<void> {
  const proposal = findProposal(id)
  if (!proposal.plan.owners.some((o) => o.publicKey === wallet.publicKey)) {
    throw new Error(`${wallet.name} is not an owner of this party.`)
  }
  if (proposal.signatures.some((s) => s.publicKey === wallet.publicKey)) return
  const signature = await signDigest(wallet.privateKeyJwk, proposal.onboarding.combinedHash)
  const entry: Signature = { publicKey: wallet.publicKey, signature, signedAt: now() }
  changeProposal(id, (p) => ({ ...p, signatures: [...p.signatures, entry] }))
}

/**
 * Creates the party. Every other host first approves the hosting. Then the
 * submitting owner's participant receives every transaction and the owners'
 * signatures. The party exists once every host reports it as local.
 */
export async function submitProposal(id: string, wallet: Wallet): Promise<void> {
  const proposal = findProposal(id)
  if (!proposal.plan.owners.some((o) => o.publicKey === wallet.publicKey)) {
    throw new Error(`${wallet.name} is not an owner, so its participant cannot submit.`)
  }
  if (getState().parties.some((p) => p.partyId === proposal.plan.partyId)) {
    throw new Error('This party already exists.')
  }
  const requests = allocationRequests(
    proposal.proposal,
    proposal.topologyVersion,
    proposal.synchronizerId,
    proposal.signatures.map(({ publicKey, signature }) => ({ publicKey, signature })),
  )
  const submitter = wallet.participant.participantId
  changeProposal(id, (p) => ({
    ...p,
    status: 'submitting',
    error: undefined,
    hosts: p.hosts.map((h) => ({ ...h, step: 'waiting', detail: undefined })),
  }))

  try {
    const others = proposal.hosts.filter((h) => h.participant !== submitter)
    const approvals = await Promise.allSettled(
      others.map((h) => ledger.allocateExternalParty(hostOf(h.participant).link, requests.hostingApproval)),
    )
    approvals.forEach((result, index) => {
      const host = others[index]
      if (result.status === 'fulfilled') setHost(id, host.participant, 'approved', 'Signed its hosting approval.')
      else setHost(id, host.participant, 'failed', message(result.reason))
    })
    if (approvals.some((result) => result.status === 'rejected')) {
      throw new Error('A host refused its hosting approval. See its answer below.')
    }

    try {
      await ledger.allocateExternalParty(hostOf(submitter).link, requests.allocate)
      setHost(id, submitter, 'submitted', 'Received every transaction and the owner signatures.')
    } catch (error) {
      setHost(id, submitter, 'failed', message(error))
      throw new Error('The submitting participant refused the party. See its answer below.')
    }

    await waitUntilHosted(id)
  } catch (error) {
    changeProposal(id, (p) => ({ ...p, status: 'failed', error: message(error) }))
    throw error
  }

  addParty(findProposal(id), wallet)
}

async function waitUntilHosted(id: string): Promise<void> {
  const deadline = Date.now() + HOSTING_TIMEOUT_MS
  for (;;) {
    const proposal = findProposal(id)
    const pending = proposal.hosts.filter((h) => h.step !== 'hosted')
    if (pending.length === 0) return
    await Promise.all(
      pending.map(async (h) => {
        try {
          const answer = await ledger.getParty(hostOf(h.participant).link, proposal.plan.partyId)
          if (partyHostedLocally(answer, proposal.plan.partyId)) setHost(id, h.participant, 'hosted', 'Hosts the party.')
        } catch (error) {
          setHost(id, h.participant, h.step, `Still waiting: ${message(error)}`)
        }
      }),
    )
    if (Date.now() > deadline) {
      const names = findProposal(id)
        .hosts.filter((h) => h.step !== 'hosted')
        .map((h) => h.name)
      if (names.length === 0) return
      throw new Error(`Timed out waiting for ${names.join(', ')} to host the party.`)
    }
    await sleep(HOSTING_POLL_MS)
  }
}

function addParty(proposal: Proposal, wallet: Wallet): void {
  const { plan } = proposal
  const party: Party = {
    partyId: plan.partyId,
    namespace: plan.namespace,
    owners: plan.owners,
    hosts: proposal.hosts.map((h) => ({
      participant: h.participant,
      role: h.role,
      approval: isSimulated()
        ? 'Approved by a simulated participant.'
        : h.role === 'owner'
          ? 'Approved by its own participant.'
          : "Approved by the dApp on localnet. This stands in for the covalidator's DecMan approval.",
    })),
    signingThreshold: plan.signingThreshold,
    confirmationThreshold: plan.confirmationThreshold,
    createdAt: now(),
    holdings: {},
    connections: [],
    requests: [],
    history: [
      {
        at: now(),
        text: `${isSimulated() ? 'Party created on simulated participants.' : 'Party created on the ledger.'} ${wallet.name} submitted it through ${shortUid(wallet.participant.participantId)}.`,
      },
    ],
  }
  update((s) => ({
    ...s,
    proposals: s.proposals.map((p) => (p.id === proposal.id ? { ...p, status: 'submitted' } : p)),
    parties: [party, ...s.parties],
  }))
}
