import { API_BASE } from "../../constants";

const partyPath = (partyId: string) =>
  `${API_BASE}/external-parties/${encodeURIComponent(partyId)}`;

/** The server's `{ error }` message, or `fallback` when the body has none. */
const errorOf = async (response: Response, fallback: string) => {
  const body = await response.json().catch(() => null);
  return body?.error || `${fallback} (HTTP ${response.status})`;
};

interface DialogProps {
  /** The party the dialog is about, or `null` when it is closed. */
  partyId: string | null;
  onClose: () => void;
}

export { partyPath, errorOf, type DialogProps };
