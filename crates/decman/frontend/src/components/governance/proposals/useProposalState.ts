import { useEffect } from "react";
import { beneficiariesProblem } from "../../../rewardBeneficiaries";
import { useContractsByTemplate } from "./useContractsByTemplate";
import { useProposalFields } from "./useProposalFields";
import { useServiceLists } from "./useServiceLists";
import { useCredentialLookups } from "./useCredentialLookups";
import { useInstrumentLookups } from "./useInstrumentLookups";
import { useTransferLookups } from "./useTransferLookups";
import { useNetworkInfo } from "./useNetworkInfo";
import { useActiveDelegations } from "./useActiveDelegations";
import { usePreapprovalCounts } from "./usePreapprovalCounts";

interface ProposalStateArgs {
  partyId: string;
  defaultOperatorParty?: string;
}

/**
 * The fields, lookup lists and prefill effects of the new-proposal form.
 * The section calls it in both views, so its fetches start when the
 * section mounts, as they did before the form had its own component.
 */
const useProposalState = ({ partyId, defaultOperatorParty }: ProposalStateArgs) => {
  // The hooks run in this order so that the requests of one proposal type
  // go out in the same order as before the split.
  const fields = useProposalFields({ partyId, defaultOperatorParty });
  const fetchContractsByTemplate = useContractsByTemplate(partyId);
  const services = useServiceLists(partyId, fields);
  const credentials = useCredentialLookups({
    partyId,
    fields,
    userServices: services.userServices,
    fetchContractsByTemplate,
  });
  const instruments = useInstrumentLookups({ partyId, fields, fetchContractsByTemplate });
  const transfer = useTransferLookups(partyId, fields);
  const network = useNetworkInfo(fields);
  const delegations = useActiveDelegations(partyId, fields);
  const preapprovals = usePreapprovalCounts(partyId, fields);

  const {
    proposalType,
    proposalInstrumentConfigurationCid,
    proposalClearBeneficiaries,
    proposalBeneficiaries,
    proposalBlockListShardCids,
    setProposalBlockListShardCids,
    blockListShardsEdited,
  } = fields;

  const { configurationToUpdate, availableInstruments, availableBlockListShards } = instruments;

  // A beneficiary list the template would reject: submitting it would only
  // fail after the committee voted, so the form refuses it up front.
  const beneficiaryProblem =
    proposalType === "set_provider_app_reward_beneficiaries"
      ? beneficiariesProblem(proposalBeneficiaries, proposalClearBeneficiaries)
      : null;

  // The update can send only an unset or false block list flag (#566), so on
  // a configuration that checks a block list it would turn the check off.
  const blockListProblem =
    proposalType === "update_instrument_configuration" &&
    configurationToUpdate?.check_block_list === true
      ? "This configuration checks a block list. DecMan cannot update it until #566, because not every mint, burn and transfer sends block list shards yet."
      : null;

  // With the block list check on, the registry refuses an accept that
  // carries no shards. It fetches them against the instrument's registrar.
  const acceptInstrument = availableInstruments.find(
    (inst) => inst.contract_id === proposalInstrumentConfigurationCid,
  );
  const acceptNeedsBlockListShards =
    (proposalType === "accept_mint_request" || proposalType === "accept_burn_request") &&
    acceptInstrument?.configuration?.check_block_list === true;
  const registrarBlockListShards = (registrar: string | undefined) =>
    availableBlockListShards.filter((shard) => shard.registrar === registrar);
  const acceptBlockListShardCids = acceptNeedsBlockListShards ? proposalBlockListShardCids : [];
  // The shard list can arrive after the request is picked, when the prefill
  // found none. Fill in the registrar's shards once they arrive, unless the
  // operator has already chosen shards by hand.
  const acceptRegistrar = acceptInstrument?.instrument_admin;
  useEffect(() => {
    if (!acceptNeedsBlockListShards || blockListShardsEdited) return;
    if (proposalBlockListShardCids.length > 0) return;
    const shards = availableBlockListShards.filter((shard) => shard.registrar === acceptRegistrar);
    if (shards.length > 0) {
      setProposalBlockListShardCids(shards.map((shard) => shard.contract_id));
    }
  }, [
    acceptNeedsBlockListShards,
    acceptRegistrar,
    availableBlockListShards,
    blockListShardsEdited,
    proposalBlockListShardCids.length,
    setProposalBlockListShardCids,
  ]);

  return {
    partyId,
    ...fields,
    ...services,
    ...credentials,
    ...instruments,
    ...transfer,
    ...network,
    ...delegations,
    ...preapprovals,
    beneficiaryProblem,
    blockListProblem,
    acceptInstrument,
    acceptNeedsBlockListShards,
    registrarBlockListShards,
    acceptBlockListShardCids,
    acceptRegistrar,
  };
};

type ProposalState = ReturnType<typeof useProposalState>;

export { useProposalState };
export type { ProposalState };
