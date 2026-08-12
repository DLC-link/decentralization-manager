/** Wire types of the demo wallet's own API (see src/demo/api.rs). */

export type HostStatus = "hosted" | "pending" | "not_hosted";

export interface HostView {
  base_url: string;
  participant_id: string;
}

/** One host's answer, or why it could not be reached. */
export interface HostReport {
  base_url: string;
  participant_id: string;
  status: HostStatus | null;
  error: string | null;
}

export interface PartyView {
  party_id: string;
  party_hint: string;
  fingerprint: string;
  public_key: string;
}

export interface ConfigView {
  hosts: HostView[];
  confirmation_threshold: number | null;
  party: PartyView | null;
}

export interface OnboardedParty {
  party_id: string;
  fingerprint: string;
  public_key: string;
  hosts: HostReport[];
}

export interface StatusView {
  party_id: string;
  hosts: HostReport[];
  fully_hosted: boolean;
}

export interface TenantContract {
  contract_id: string;
  template_id: string;
  create_arguments: unknown;
}

export interface AcsView {
  contracts: TenantContract[];
  served_by: string;
}

/** A balance in one instrument. Amounts are decimal strings: Daml `Decimal` keeps
 *  10 fractional digits and a JSON number would round them. */
export interface TenantHolding {
  instrument_admin: string;
  instrument_id: string;
  total: string;
  locked: string;
  available: string;
}

export interface HoldingsView {
  holdings: TenantHolding[];
  served_by: string;
}

/** An inbound transfer this party can accept. */
export interface TenantTransferOffer {
  contract_id: string;
  sender: string;
  instrument_admin: string;
  instrument_id: string;
  amount: string;
  acceptable: boolean;
  expires_at: number;
}

export interface OffersView {
  offers: TenantTransferOffer[];
  served_by: string;
}
