// Small shared pieces for the pages.
import { useState, type ReactNode } from 'react'

import { shortHex } from './format'

/** Shows a long hex string shortened, with the full value on hover. */
export function Hex({ value }: { value: string }) {
  return (
    <code title={value} className="hex">
      {shortHex(value)}
    </code>
  )
}

/** Shows a participant id as its name plus a shortened namespace. */
export function Uid({ value }: { value: string }) {
  const [name, namespace] = value.split('::')
  if (namespace === undefined) return <Hex value={value} />
  return (
    <span title={value} className="uid">
      {name}
      <span className="muted">::</span>
      <Hex value={namespace} />
    </span>
  )
}

/** Shows a party id as its name plus a shortened namespace. */
export function PartyId({ value }: { value: string }) {
  const [hint, namespace] = value.split('::')
  return (
    <span title={value}>
      <strong>{hint}</strong>::<Hex value={namespace ?? ''} />
    </span>
  )
}

/**
 * Runs an action and shows its error under the children. Each action button
 * on a page goes through this, so a failed check always shows its reason.
 */
export function useAction(): [string | null, (action: () => unknown) => Promise<void>] {
  const [error, setError] = useState<string | null>(null)
  const run = async (action: () => unknown) => {
    setError(null)
    try {
      await action()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }
  return [error, run]
}

export function ErrorText({ error }: { error: string | null }) {
  return error ? <p className="error">{error}</p> : null
}

export function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h2>{title}</h2>
      {children}
    </section>
  )
}

const ICON_PATHS = {
  wallet: 'M3 7a2 2 0 0 1 2-2h12v4M3 7v10a2 2 0 0 0 2 2h14V9H5a2 2 0 0 1-2-2Zm13 6h.01',
  plus: 'M12 5v14M5 12h14',
  signature: 'M4 20h16M6 16l9.5-9.5a2.1 2.1 0 0 1 3 3L9 19H6v-3Z',
  party: 'M12 3l8 4v5c0 4.5-3.4 8.3-8 9-4.6-.7-8-4.5-8-9V7l8-4Z',
  check: 'M5 12.5l4.5 4.5L19 7.5',
  clock: 'M12 7v5l3 2M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z',
  cross: 'M6 6l12 12M18 6L6 18',
} as const

export type IconName = keyof typeof ICON_PATHS

export function Icon({ name }: { name: IconName }) {
  return (
    <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d={ICON_PATHS[name]} />
    </svg>
  )
}

/** A round badge with a name's first letter, in a color derived from the name. */
export function Avatar({ name }: { name: string }) {
  let hue = 0
  for (const char of name) hue = (hue * 31 + char.charCodeAt(0)) % 360
  return (
    <span className="avatar" style={{ background: `hsl(${hue} 55% 46%)` }} aria-hidden="true">
      {name.trim().charAt(0).toUpperCase() || '?'}
    </span>
  )
}

/** A name with its avatar in front. */
export function Person({ name }: { name: string }) {
  return (
    <span className="person">
      <Avatar name={name} />
      {name}
    </span>
  )
}

export type Tone = 'ok' | 'warn' | 'error' | 'muted' | 'accent'

export function Badge({ tone, children }: { tone: Tone; children: ReactNode }) {
  return <span className={`badge ${tone}`}>{children}</span>
}

/** Marks a record whose participants are simulated. */
export function SimulatedBadge() {
  return <Badge tone="warn">Simulated</Badge>
}

/** Numbered steps. Steps before `current` are done; `failed` marks the current one as failed. */
export function Stepper({ steps, current, failed }: { steps: string[]; current: number; failed?: boolean }) {
  return (
    <ol className="stepper">
      {steps.map((step, index) => {
        const state = index < current ? 'done' : index === current ? (failed ? 'failed' : 'current') : 'todo'
        return (
          <li key={step} className={state}>
            <span className="dot">
              {state === 'done' ? <Icon name="check" /> : state === 'failed' ? <Icon name="cross" /> : index + 1}
            </span>
            {step}
          </li>
        )
      })}
    </ol>
  )
}

export function EmptyState({ icon, children }: { icon: IconName; children: ReactNode }) {
  return (
    <div className="empty">
      <Icon name={icon} />
      <p>{children}</p>
    </div>
  )
}
