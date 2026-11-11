import { useCallback, useEffect, useState } from "react";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import type { ProposalFields } from "./useProposalFields";
import type {
  ProviderServiceInfo,
  ProviderServicesResponse,
  UserServiceInfo,
  UserServicesResponse,
} from "../../../types";

/**
 * The ProviderService and UserService lists of the party, read for the
 * service request, Setup Utility, credential and registrar forms.
 */
export function useServiceLists(partyId: string, fields: ProposalFields) {
  const {
    proposalType,
    setProposalProvider,
    setProposalUser,
  } = fields;

  // Available services from ACS
  const [providerServices, setProviderServices] = useState<
    ProviderServiceInfo[]
  >([]);
  const [userServices, setUserServices] = useState<UserServiceInfo[]>([]);
  const [servicesLoading, setServicesLoading] = useState(false);

  // Fetch available services from ACS
  const fetchServices = useCallback(async () => {
    setServicesLoading(true);
    try {
      const [providerRes, userRes] = await Promise.all([
        authenticatedFetch(
          `${API_BASE}/services/provider?party_id=${encodeURIComponent(partyId)}`,
        ),
        authenticatedFetch(
          `${API_BASE}/services/user?party_id=${encodeURIComponent(partyId)}`,
        ),
      ]);

      if (providerRes.ok) {
        const response: ProviderServicesResponse = await providerRes.json();
        setProviderServices(response.services);
      }

      if (userRes.ok) {
        const response: UserServicesResponse = await userRes.json();
        setUserServices(response.services);
      }
    } catch (e) {
      console.error("Failed to fetch services:", e);
    } finally {
      setServicesLoading(false);
    }
  }, [partyId]);

  // Fetch services when a proposal type that creates a service-request is
  // selected — used to detect when the corresponding service already exists,
  // so the form can warn that the proposal would be a no-op.
  // SetupUtility additionally needs the ProviderService list to populate its
  // dropdown of available services to wire the utility setup against.
  // The credential proposal forms need the UserService list to prefill their
  // UserService dropdown.
  useEffect(() => {
    if (
      proposalType === "create_user_service_request" ||
      proposalType === "create_provider_service_request" ||
      proposalType === "setup_utility" ||
      proposalType === "offer_free_credential" ||
      proposalType === "accept_free_credential" ||
      proposalType === "offer_paid_credential" ||
      proposalType === "accept_paid_credential" ||
      proposalType === "create_provider_configuration" ||
      proposalType === "onboard_registrar"
    ) {
      fetchServices();
    }
  }, [proposalType, fetchServices]);

  // The CreateUserServiceRequest / CreateProviderServiceRequest proposals
  // always use the dec party itself as the user / provider — the field
  // exists because the Daml choice still asks for it, but every operator
  // ends up typing the same value. Seed it once when the form opens.
  useEffect(() => {
    if (proposalType === "create_user_service_request") {
      setProposalUser(partyId);
    } else if (proposalType === "create_provider_service_request") {
      setProposalProvider(partyId);
    }
  }, [proposalType, partyId, setProposalUser, setProposalProvider]);

  return {
    providerServices,
    setProviderServices,
    userServices,
    setUserServices,
    servicesLoading,
    setServicesLoading,
    fetchServices,
  };
}
