/** A long hex string as its start and end. */
export function shortHex(value: string): string {
  return value.length > 24 ? `${value.slice(0, 12)}…${value.slice(-8)}` : value
}

/** A Canton `name::namespace` id with the namespace shortened. */
export function shortUid(value: string): string {
  const [name, namespace] = value.split('::')
  return namespace === undefined ? shortHex(value) : `${name}::${shortHex(namespace)}`
}
