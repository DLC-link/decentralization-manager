/**
 * True when a governance write failed at the Canton ledger. The server
 * writes "the ledger refused the submission" for a ledger refusal, and "the
 * Ledger API" for a failed call or connection. An operator cannot fix these
 * from the web UI, so the dialog asks them to forward the error to the
 * BitSafe team.
 */
export const isChainError = (message: string): boolean =>
  message.includes("the ledger refused the submission") ||
  message.includes("the Ledger API");
