import { useEffect, useMemo, useState } from "react";
import { API_BASE, TEMPLATE_REGISTRAR_SERVICE } from "../../../constants";
import { usePartyList } from "../../../hooks/usePartyList";
import type { FetchContractsByTemplate } from "./useContractsByTemplate";
import type { ProposalFields } from "./useProposalFields";
import type {
  BlockListShardsResponse,
  BurnRequestsResponse,
  ContractWithBlob,
  CredentialsResponse,
  InstrumentsResponse,
  MintRequestsResponse,
  ProviderConfigurationsResponse,
  RegistrarServiceRequestsResponse,
  RegistrarServicesResponse,
} from "../../../types";

interface InstrumentLookupsArgs {
  partyId: string;
  fields: ProposalFields;
  fetchContractsByTemplate: FetchContractsByTemplate;
}

/**
 * The lookups of the registrar, instrument and token request forms: the
 * instruments, requests, credentials, block list shards and registrar
 * contracts the selected proposal type needs.
 */
export function useInstrumentLookups({
  partyId,
  fields,
  fetchContractsByTemplate,
}: InstrumentLookupsArgs) {
  const {
    proposalType,
    proposalInstrumentConfigurationCid,
  } = fields;

  const [registrarServiceContracts, setRegistrarServiceContracts] = useState<ContractWithBlob[]>([]);
  // Typed `MintRequest`/`BurnRequest` rows so the Accept dropdowns can show
  // holder → amount instrument (…cid) — mirroring the Accept Transfer UX —
  // instead of just the contract id slug.
  const {
    items: mintRequestContracts,
    loading: mintRequestsLoading,
    load: fetchOpenMintRequests,
  } = usePartyList<MintRequestsResponse>({
    path: `${API_BASE}/governance/mint-requests`,
    partyId,
    field: "mint_requests",
    what: "mint requests",
  });
  const {
    items: burnRequestContracts,
    loading: burnRequestsLoading,
    load: fetchOpenBurnRequests,
  } = usePartyList<BurnRequestsResponse>({
    path: `${API_BASE}/governance/burn-requests`,
    partyId,
    field: "burn_requests",
    what: "burn requests",
  });
  // `Credential` contracts visible to this party. Powers the issuer
  // credential picker on the Accept Mint/Burn Request forms.
  const {
    items: availableCredentials,
    loading: credentialsLoading,
    load: fetchCredentials,
  } = usePartyList<CredentialsResponse>({
    path: `${API_BASE}/credentials`,
    partyId,
    field: "credentials",
    what: "credentials",
  });
  // Block list shards (`StoredBlockListPartyShas`) visible to this party.
  // Power the shard picker on the Accept Mint/Burn Request forms.
  const {
    items: availableBlockListShards,
    loading: blockListShardsLoading,
    load: fetchBlockListShards,
  } = usePartyList<BlockListShardsResponse>({
    path: `${API_BASE}/block-list-shards`,
    partyId,
    field: "block_list_shards",
    what: "block list shards",
  });
  // Pending `RegistrarServiceRequest` and `ProviderConfiguration` contracts.
  // Power the pickers on the Onboard Registrar form.
  const {
    items: registrarServiceRequests,
    loading: registrarServiceRequestsLoading,
    load: fetchRegistrarServiceRequests,
  } = usePartyList<RegistrarServiceRequestsResponse>({
    path: `${API_BASE}/registrar-service-requests`,
    partyId,
    field: "registrar_service_requests",
    what: "registrar service requests",
  });
  // Typed `RegistrarService` contracts. Power the Provision Instrument
  // picker, which needs the registrar field to filter out services this
  // decparty co-signed as the provider.
  const {
    items: availableRegistrarServices,
    loading: registrarServicesLoading,
    load: fetchRegistrarServices,
  } = usePartyList<RegistrarServicesResponse>({
    path: `${API_BASE}/services/registrar`,
    partyId,
    field: "services",
    what: "registrar services",
  });
  const {
    items: providerConfigurations,
    loading: providerConfigurationsLoading,
    load: fetchProviderConfigurations,
  } = usePartyList<ProviderConfigurationsResponse>({
    path: `${API_BASE}/provider-configurations`,
    partyId,
    field: "provider_configurations",
    what: "provider configurations",
  });
  // InstrumentConfiguration contracts fetched from /instruments. Each one
  // represents a token the governance party can mint/burn against and exposes
  // its parsed instrument_admin + instrument_id, so we can drive a real
  // dropdown without the frontend having to decode contract blobs.
  const {
    items: availableInstruments,
    loading: instrumentsLoading,
    load: fetchInstruments,
  } = usePartyList<InstrumentsResponse>({
    path: `${API_BASE}/instruments`,
    partyId,
    field: "instruments",
    what: "instruments",
  });

  // Requests this provider decparty can accept via Onboard Registrar: only
  // those that name it as the provider. With the provider fixed, each row
  // needs only the registrar and the cid tail.
  const acceptableRegistrarServiceRequests = useMemo(
    () => registrarServiceRequests.filter((r) => r.provider === partyId),
    [registrarServiceRequests, partyId],
  );

  // Services this registrar decparty can provision instruments on. The
  // decparty also co-signs services as the provider side; those fail
  // ProvisionInstrument's registrar assertion, so hide them.
  const ownRegistrarServices = useMemo(
    () => availableRegistrarServices.filter((s) => s.registrar === partyId),
    [availableRegistrarServices, partyId],
  );

  // Instruments this registrar decparty administers. The decparty also
  // co-signs its registrars' instruments as the provider side; those fail
  // OnboardInstrumentIssuers' registrar assertion, so hide them. The
  // instrument admin is the configuration's registrar.
  const ownInstruments = useMemo(
    () => availableInstruments.filter((inst) => inst.instrument_admin === partyId),
    [availableInstruments, partyId],
  );

  const configurationToUpdate = ownInstruments.find(
    (inst) => inst.contract_id === proposalInstrumentConfigurationCid,
  )?.configuration;
  const configurationToUpdateUnreadable =
    proposalInstrumentConfigurationCid !== "" && configurationToUpdate == null;

  // Fetch instruments when a proposal type needs them: Mint/Burn (for
  // instrument_id + instrument_configuration_cid) and
  // set_provider_app_reward_beneficiaries (for instrument_configuration_cid).
  useEffect(() => {
    if (
      proposalType === "accept_mint_request" ||
      proposalType === "accept_burn_request" ||
      proposalType === "set_provider_app_reward_beneficiaries" ||
      proposalType === "onboard_instrument_issuers" ||
      proposalType === "update_instrument_configuration"
    ) {
      fetchInstruments();
    }
  }, [proposalType, fetchInstruments]);

  // Fetch the contract lists the selected proposal type needs.
  useEffect(() => {
    if (proposalType === "set_enable_result_contracts") {
      fetchContractsByTemplate(TEMPLATE_REGISTRAR_SERVICE).then(setRegistrarServiceContracts);
    }
    // Provision Instrument needs the typed rows to filter on the registrar
    // field, so it uses /services/registrar instead of the blob query.
    if (
      proposalType === "provision_instrument" ||
      proposalType === "update_instrument_configuration" ||
      proposalType === "create_transfer_rule"
    ) {
      fetchRegistrarServices();
    }
    if (proposalType === "accept_mint_request") {
      fetchOpenMintRequests();
    }
    if (proposalType === "accept_burn_request") {
      fetchOpenBurnRequests();
    }
    // The accept forms need the party's credentials for the issuer
    // credential picker; the Offboard form for its per-issuer credential
    // pickers.
    if (
      proposalType === "accept_mint_request" ||
      proposalType === "accept_burn_request" ||
      proposalType === "offboard_instrument_issuers"
    ) {
      fetchCredentials();
    }
    if (proposalType === "accept_mint_request" || proposalType === "accept_burn_request") {
      fetchBlockListShards();
    }
    // The Onboard Registrar pickers list the pending requests and the
    // provider's configurations.
    if (proposalType === "onboard_registrar") {
      fetchRegistrarServiceRequests();
      fetchProviderConfigurations();
    }
  }, [
    proposalType,
    fetchContractsByTemplate,
    fetchOpenMintRequests,
    fetchOpenBurnRequests,
    fetchCredentials,
    fetchBlockListShards,
    fetchRegistrarServiceRequests,
    fetchProviderConfigurations,
    fetchRegistrarServices,
  ]);

  return {
    registrarServiceContracts,
    setRegistrarServiceContracts,
    mintRequestContracts,
    mintRequestsLoading,
    fetchOpenMintRequests,
    burnRequestContracts,
    burnRequestsLoading,
    fetchOpenBurnRequests,
    availableCredentials,
    credentialsLoading,
    fetchCredentials,
    availableBlockListShards,
    blockListShardsLoading,
    fetchBlockListShards,
    registrarServiceRequests,
    registrarServiceRequestsLoading,
    fetchRegistrarServiceRequests,
    availableRegistrarServices,
    registrarServicesLoading,
    fetchRegistrarServices,
    providerConfigurations,
    providerConfigurationsLoading,
    fetchProviderConfigurations,
    availableInstruments,
    instrumentsLoading,
    fetchInstruments,
    acceptableRegistrarServiceRequests,
    ownRegistrarServices,
    ownInstruments,
    configurationToUpdate,
    configurationToUpdateUnreadable,
  };
}
