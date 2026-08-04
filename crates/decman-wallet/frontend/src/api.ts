import type {
  AcsView,
  ConfigView,
  OnboardedParty,
  PartyView,
  StatusView,
  TemplateId,
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

export const createParty = (partyHint: string, confirmationThreshold: number | null) =>
  request<OnboardedParty>("/api/party", {
    method: "POST",
    body: JSON.stringify({
      party_hint: partyHint,
      confirmation_threshold: confirmationThreshold,
    }),
  });

export const createContract = (templateId: TemplateId, createArguments: unknown) =>
  request<{ served_by: string }>("/api/party/contracts", {
    method: "POST",
    body: JSON.stringify({
      template_id: templateId,
      create_arguments: createArguments,
    }),
  });

export const reset = () => request<void>("/api/party", { method: "DELETE" });
