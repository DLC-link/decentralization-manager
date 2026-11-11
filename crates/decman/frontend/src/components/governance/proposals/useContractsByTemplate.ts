import { useCallback } from "react";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import type { ContractQueryResponse } from "../../../types";

/**
 * Read the party's contracts of one template through /contracts/query. The
 * credential and instrument lookups share it.
 */
export function useContractsByTemplate(partyId: string) {
  // Fetch contracts by template (returns CID + blob)
  const fetchContractsByTemplate = useCallback(
    async (
      template: {
        package_ref: string;
        module: string;
        entity: string;
        interface?: boolean;
      },
      options?: { activeOnly?: boolean; includePayload?: boolean },
    ) => {
      const params = new URLSearchParams({
        party_id: partyId,
        package_id: template.package_ref,
        module_name: template.module,
        entity_name: template.entity,
      });
      if (template.interface) params.set("interface", "true");
      if (options?.activeOnly) params.set("active_only", "true");
      if (options?.includePayload) params.set("include_payload", "true");
      const res = await authenticatedFetch(`${API_BASE}/contracts/query?${params}`);
      if (res.ok) {
        const data: ContractQueryResponse = await res.json();
        return data.contracts;
      }
      return [];
    },
    [partyId],
  );
  return fetchContractsByTemplate;
}

export type FetchContractsByTemplate = ReturnType<typeof useContractsByTemplate>;
