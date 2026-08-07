import type { HostReport, HostView } from "../types";
import { StatusBadge } from "./ui";

/** The hosting set, with each host's own view of the party once there is one. */
export function HostList({
  hosts,
  reports,
}: {
  hosts: HostView[];
  reports: HostReport[] | null;
}) {
  const reportFor = (baseUrl: string) => reports?.find((r) => r.base_url === baseUrl) ?? null;

  return (
    <div>
      {hosts.map((host) => {
        const report = reportFor(host.base_url);
        return (
          <div className="host" key={host.base_url}>
            <div className="host-id">
              <div className="url">{host.base_url}</div>
              <div className="pid">{host.participant_id}</div>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
              <span className="host-role">co-validator</span>
              {report ? (
                <StatusBadge status={report.status} error={report.error} />
              ) : null}
            </div>
          </div>
        );
      })}
    </div>
  );
}
