import type {
  AcsView,
  ConfigView,
  HoldingsView,
  OffersView,
  OnboardedParty,
  PartyView,
  StatusView,
} from "./types";

/** The wallet answers errors as `{ "error": "..." }`; surface that text as-is. */
async function failure(response: Response): Promise<Error> {
  const body = await response.text();
  try {
    const parsed: unknown = JSON.parse(body);
    if (
      parsed &&
      typeof parsed === "object" &&
      "error" in parsed &&
      typeof parsed.error === "string"
    ) {
      return new Error(parsed.error);
    }
  } catch {
    // Not JSON — fall through to the raw body.
  }
  return new Error(body || `${response.status} ${response.statusText}`);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    headers: init?.body ? { "Content-Type": "application/json" } : undefined,
    ...init,
  });
  if (!response.ok) throw await failure(response);
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

export const getConfig = () => request<ConfigView>("/api/config");

export const getParty = () => request<PartyView>("/api/party");

export const getStatus = () => request<StatusView>("/api/party/status");

export const getAcs = () => request<AcsView>("/api/party/acs");

export const getHoldings = () => request<HoldingsView>("/api/party/holdings");

export const getOffers = () => request<OffersView>("/api/party/offers");

/** Send an asset. The wallet prepares it on a host, signs here, and executes there. */
export const sendAsset = (
  receiver: string,
  instrumentAdmin: string,
  instrumentId: string,
  amount: string,
) =>
  request<{ served_by: string }>("/api/party/transfers", {
    method: "POST",
    body: JSON.stringify({
      receiver,
      instrument_admin: instrumentAdmin,
      instrument_id: instrumentId,
      amount,
    }),
  });

/** Accept an inbound transfer, taking the escrowed funds into this party's holdings. */
export const acceptOffer = (transferInstructionCid: string) =>
  request<{ served_by: string }>("/api/party/offers/accept", {
    method: "POST",
    body: JSON.stringify({ transfer_instruction_cid: transferInstructionCid }),
  });

export const createParty = (partyHint: string, confirmationThreshold: number | null) =>
  request<OnboardedParty>("/api/party", {
    method: "POST",
    body: JSON.stringify({
      party_hint: partyHint,
      confirmation_threshold: confirmationThreshold,
    }),
  });

/** The party's own signing key, for display. Loopback only; never sent to a host. */
export const getSecret = () => request<{ seed: string }>("/api/party/secret");

export const reset = () => request<void>("/api/party", { method: "DELETE" });
