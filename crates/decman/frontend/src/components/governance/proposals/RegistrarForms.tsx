import { Checkbox, FormControlLabel } from "@mui/material";
import { TextHelp } from "../../FieldHelp";
import { ContractSelect } from "../fields/ContractSelect";
import { HelpTextField } from "../fields/HelpTextField";
import { RequirementRows } from "../fields/RequirementRows";
import type { ProposalState } from "./useProposalState";

interface Props {
  state: ProposalState;
}

/**
 * The Create Provider Configuration, Create Registrar Service Request and
 * Onboard Registrar forms.
 */
export const RegistrarForms = ({ state }: Props) => {
  const {
    partyId,
    proposalType,
    proposalProvider,
    setProposalProvider,
    proposalOperator,
    setProposalOperator,
    proposalProviderServiceCid,
    setProposalProviderServiceCid,
    proposalCreateTransferRule,
    setProposalCreateTransferRule,
    proposalCreateAllocationFactory,
    setProposalCreateAllocationFactory,
    proposalRegistrarServiceRequestCid,
    setProposalRegistrarServiceRequestCid,
    proposalProviderConfigurationCid,
    setProposalProviderConfigurationCid,
    proposalRegistrarRequirements,
    setProposalRegistrarRequirements,
    proposalHolderRequirements,
    setProposalHolderRequirements,
    providerServices,
    registrarServiceRequestsLoading,
    providerConfigurations,
    providerConfigurationsLoading,
    servicesLoading,
    acceptableRegistrarServiceRequests,
  } = state;

  return (
    <>
      {proposalType === "create_provider_configuration" && (
        <>
          <ContractSelect
            label="ProviderService"
            help="ProviderService contract of this provider decparty. Its provider must be the governance party."
            value={proposalProviderServiceCid}
            onChange={setProposalProviderServiceCid}
            items={providerServices}
            loading={servicesLoading}
            loadingText="Loading services…"
            emptyText='No ProviderService found — run "Create Provider Service Request" first'
          />
          <RequirementRows
            label="Registrar Requirements"
            help="Credential requirements a registrar must meet before the provider onboards it. Rows whose issuer is this governance party are minted automatically during Onboard Registrar."
            rows={proposalRegistrarRequirements}
            setRows={setProposalRegistrarRequirements}
            defaultIssuer={partyId}
          />
          <RequirementRows
            label="Holder Requirements"
            help="Credential requirements a token holder must meet on this provider's utility."
            rows={proposalHolderRequirements}
            setRows={setProposalHolderRequirements}
            defaultIssuer={partyId}
          />
        </>
      )}

      {proposalType === "create_registrar_service_request" && (
        <>
          <HelpTextField
            size="small"
            label="Operator Party"
            value={proposalOperator}
            onChange={(e) => setProposalOperator(e.target.value)}
            fullWidth
            required
            help="Party id of the utility operator the registrar service runs under."
          />
          <HelpTextField
            size="small"
            label="Provider Party"
            value={proposalProvider}
            onChange={(e) => setProposalProvider(e.target.value)}
            fullWidth
            required
            help="Party id of the provider decparty this request asks for registrar service. The provider accepts via Onboard Registrar."
          />
          <FormControlLabel
            control={<Checkbox size="small" checked={proposalCreateTransferRule} onChange={(e) => setProposalCreateTransferRule(e.target.checked)} />}
            label={
              <TextHelp text="Also create a TransferRule contract during the accept, so holders can transfer this registrar's tokens without per-transfer governance.">
                Create TransferRule
              </TextHelp>
            }
          />
          <FormControlLabel
            control={<Checkbox size="small" checked={proposalCreateAllocationFactory} onChange={(e) => setProposalCreateAllocationFactory(e.target.checked)} />}
            label={
              <TextHelp text="Also create an AllocationFactory contract during the accept, so this registrar's tokens can be allocated by external apps.">
                Create AllocationFactory
              </TextHelp>
            }
          />
        </>
      )}

      {proposalType === "onboard_registrar" && (
        <>
          <ContractSelect
            label="ProviderService"
            help="ProviderService contract of this provider decparty, used to accept the request."
            value={proposalProviderServiceCid}
            onChange={setProposalProviderServiceCid}
            items={providerServices}
            loading={servicesLoading}
            loadingText="Loading services…"
            emptyText='No ProviderService found — run "Create Provider Service Request" first'
          />
          <ContractSelect
            label="RegistrarServiceRequest"
            help="Pending RegistrarServiceRequest to accept. The request names the registrar to onboard; its provider must be this governance party."
            value={proposalRegistrarServiceRequestCid}
            onChange={setProposalRegistrarServiceRequestCid}
            items={acceptableRegistrarServiceRequests}
            loading={registrarServiceRequestsLoading}
            loadingText="Loading requests…"
            emptyText='No pending requests for this provider — the registrar runs "Create Registrar Service Request" first'
            rowLabel={(req) => <>{req.registrar.split("::")[0]} (…{req.contract_id.slice(-8)})</>}
          />
          <ContractSelect
            label="ProviderConfiguration"
            help="ProviderConfiguration holding the registrar requirements to mint and validate against."
            value={proposalProviderConfigurationCid}
            onChange={setProposalProviderConfigurationCid}
            items={providerConfigurations}
            loading={providerConfigurationsLoading}
            loadingText="Loading configurations…"
            emptyText='No ProviderConfiguration found — run "Create Provider Configuration" first'
          />
        </>
      )}
    </>
  );
};
