import type { AcsView } from "../types";
import { Card } from "./ui";

/** The party's active contracts, read from whichever host answers first. */
export function Contracts({
  acs,
  busy,
  onRefresh,
}: {
  acs: AcsView | null;
  busy: boolean;
  onRefresh: () => void;
}) {
  return (
    <Card
      title="Active contracts"
        eyebrow={acs ? `served by ${acs.served_by}` : "the party's own ledger view"}
        aside={
          <button className="btn-secondary btn-sm" type="button" onClick={onRefresh} disabled={busy}>
            ⟳ Refresh
          </button>
        }
      >
        {acs && acs.contracts.length > 0 ? (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>Contract id</th>
                  <th>Template</th>
                  <th>Payload</th>
                </tr>
              </thead>
              <tbody>
                {acs.contracts.map((contract) => (
                  <tr key={contract.contract_id}>
                    <td>{contract.contract_id}</td>
                    <td>{contract.template_id}</td>
                    <td>{JSON.stringify(contract.create_arguments)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <p className="empty t-sm">
            {acs
              ? "No active contracts yet."
              : "The ledger becomes readable once at least one host has the party."}
          </p>
        )}
    </Card>
  );
}
