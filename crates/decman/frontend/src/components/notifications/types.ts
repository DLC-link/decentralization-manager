import type { DomainGovernanceAction, GovernanceAction, GovernanceType } from "../../types";

export interface PartyActions {
  partyId: string;
  /** Contract that holds the GovernanceRules — needed for confirm/execute. */
  rulesContractId?: string;
  /** Caller's member party id for this dec party — used to detect own confirmations. */
  memberPartyId?: string;
  /** Used when sending mutating requests (core_self vs core_domain). */
  governanceType: GovernanceType;
  threshold: number;
  actions: GovernanceAction[];
  /** Resume token for this party's next batch; null when exhausted. */
  nextCursor?: string | null;
  /** On-chain DSO governance proposals (governance_type = "core_domain"). Surfaced
   *  as cards in the notification feed alongside off-chain actions. */
  domainActions: DomainGovernanceAction[];
}
