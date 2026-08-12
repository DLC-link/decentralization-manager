import type { OffersView, TenantTransferOffer } from "../types";
import { Card } from "./ui";

const label = (offer: TenantTransferOffer) =>
  offer.instrument_id === "Amulet" ? "CC (Amulet)" : offer.instrument_id;

/** Render the accept deadline. Past-deadline offers stay listed — Daml refuses the
 *  accept, and hiding them reads as "the transfer never arrived". */
const deadline = (unixSeconds: number) => {
  if (!unixSeconds) return "—";
  const at = new Date(unixSeconds * 1000);
  if (Number.isNaN(at.getTime())) return "—";
  const expired = at.getTime() <= Date.now();
  return `${expired ? "expired " : ""}${at.toISOString().replace("T", " ").slice(0, 16)} UTC`;
};

/** Transfers waiting for this party to accept them.
 *
 *  A transfer to a party without a pre-approval escrows the funds and leaves a
 *  `TransferInstruction`; accepting it is the receiver's own signature, which is
 *  exactly the half of the flow a wallet-held key has to be able to do. */
export function Offers({
  offers,
  busy,
  onRefresh,
  onAccept,
}: {
  offers: OffersView | null;
  busy: boolean;
  onRefresh: () => void;
  onAccept: (contractId: string) => void;
}) {
  const rows = offers?.offers ?? [];

  return (
    <Card
      title="Incoming transfers"
      eyebrow={offers ? `served by ${offers.served_by}` : "awaiting this party's signature"}
      aside={
        <button className="btn-secondary btn-sm" type="button" onClick={onRefresh} disabled={busy}>
          ⟳ Refresh
        </button>
      }
    >
      {rows.length > 0 ? (
        <div className="table-scroll">
          <table>
            <thead>
              <tr>
                <th>From</th>
                <th>Instrument</th>
                <th className="num">Amount</th>
                <th>Accept before</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((offer) => (
                <tr key={offer.contract_id}>
                  <td className="mono">{offer.sender}</td>
                  <td>{label(offer)}</td>
                  <td className="num">{offer.amount}</td>
                  <td>{deadline(offer.expires_at)}</td>
                  <td className="num">
                    <button
                      className="btn-primary btn-sm"
                      type="button"
                      disabled={busy || !offer.acceptable}
                      title={
                        offer.acceptable
                          ? "Sign the acceptance with this wallet's key"
                          : "This offer is expired or still waiting on the registrar"
                      }
                      onClick={() => onAccept(offer.contract_id)}
                    >
                      Accept
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="empty t-sm">
          {offers
            ? "Nothing waiting. A transfer sent to this party shows up here."
            : "Offers become readable once at least one host has the party."}
        </p>
      )}
    </Card>
  );
}
