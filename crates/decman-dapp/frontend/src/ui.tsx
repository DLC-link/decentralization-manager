// Small shared pieces for the pages.
import { useState, type ReactNode } from 'react'

/** Shows a long hex string shortened, with the full value on hover. */
export function Hex({ value }: { value: string }) {
  const short = value.length > 24 ? `${value.slice(0, 12)}…${value.slice(-8)}` : value
  return (
    <code title={value} className="hex">
      {short}
    </code>
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
