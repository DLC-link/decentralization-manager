import { useCallback, useEffect, useMemo, useState } from "react";
import { API_BASE, TEMPLATE_AMULET } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import { usePartyList } from "../../../hooks/usePartyList";
import {
  type AmuletOption,
  offerDepositUsd,
  ownedAmulets,
} from "../../../paidCredential";
import type { FetchContractsByTemplate } from "./useContractsByTemplate";
import type { ProposalFields } from "./useProposalFields";
import type {
  CredentialOffersResponse,
  OpenMiningRoundInfo,
  UserServiceInfo,
} from "../../../types";

interface CredentialLookupsArgs {
  partyId: string;
  fields: ProposalFields;
  userServices: UserServiceInfo[];
  fetchContractsByTemplate: FetchContractsByTemplate;
}

/**
 * The lookups of the credential forms: the credential offers, the Amulets
 * for a paid deposit and the open mining round, with their prefills.
 */
export function useCredentialLookups({
  partyId,
  fields,
  userServices,
  fetchContractsByTemplate,
}: CredentialLookupsArgs) {
  const {
    proposalType,
    proposalUserServiceCid,
    setProposalUserServiceCid,
    proposalCredentialOfferCid,
    setProposalCredentialOfferCid,
    setProposalOpenMiningRoundCid,
  } = fields;

  // The newest open round as scan lists it, or why the node could not read it
  const [openMiningRound, setOpenMiningRound] = useState<OpenMiningRoundInfo | null>(null);
  const [openMiningRoundError, setOpenMiningRoundError] = useState("");
  const [openMiningRoundLoading, setOpenMiningRoundLoading] = useState(false);
  // Pending CredentialOffer contracts visible to this party. Powers the
  // CredentialOffer dropdowns on the Accept Free Credential forms.
  const {
    items: credentialOffers,
    loading: credentialOffersLoading,
    load: fetchCredentialOffers,
  } = usePartyList<CredentialOffersResponse>({
    path: `${API_BASE}/credential-offers`,
    partyId,
    field: "credential_offers",
    what: "credential offers",
  });
  // Amulets this party owns. Powers the deposit picker on the Accept Paid
  // Credential form.
  const [ownAmulets, setOwnAmulets] = useState<AmuletOption[]>([]);
  const [ownAmuletsLoading, setOwnAmuletsLoading] = useState(false);

  useEffect(() => {
    if (
      proposalType === "accept_free_credential" ||
      proposalType === "accept_paid_credential"
    ) {
      fetchCredentialOffers();
    }
  }, [proposalType, fetchCredentialOffers]);

  // Offers this party can take via the Free direction: it is the holder and
  // the offer carries no billing params (`CredentialOffer_AcceptFree` rejects
  // billed offers).
  const acceptableCredentialOffers = useMemo(
    () => credentialOffers.filter((o) => o.is_free && o.holder === partyId),
    [credentialOffers, partyId],
  );

  // Offers this party can take via the Paid direction: it is the holder and
  // the offer carries billing params.
  const acceptablePaidCredentialOffers = useMemo(
    () => credentialOffers.filter((o) => !o.is_free && o.holder === partyId),
    [credentialOffers, partyId],
  );

  // The listed paid offer the Accept Paid Credential form names. A pasted cid
  // of an unlisted offer leaves it undefined.
  const chosenPaidOffer = useMemo(
    () =>
      acceptablePaidCredentialOffers.find((o) => o.contract_id === proposalCredentialOfferCid),
    [acceptablePaidCredentialOffers, proposalCredentialOfferCid],
  );
  const chosenDepositUsd = chosenPaidOffer && offerDepositUsd(chosenPaidOffer);

  // Prefill the credential proposal form's UserService once the list arrives —
  // parties typically have exactly one.
  useEffect(() => {
    if (
      (proposalType === "offer_free_credential" ||
        proposalType === "accept_free_credential" ||
        proposalType === "offer_paid_credential" ||
        proposalType === "accept_paid_credential") &&
      !proposalUserServiceCid &&
      userServices.length > 0
    ) {
      setProposalUserServiceCid(userServices[0].contract_id);
    }
  }, [proposalType, userServices, proposalUserServiceCid, setProposalUserServiceCid]);

  // Prefill the CredentialOffer cid when there's exactly one candidate. With
  // several pending offers the operator has to pick deliberately.
  useEffect(() => {
    if (acceptableCredentialOffers.length !== 1) {
      return;
    }
    const offerCid = acceptableCredentialOffers[0].contract_id;
    if (proposalType === "accept_free_credential" && !proposalCredentialOfferCid) {
      setProposalCredentialOfferCid(offerCid);
    }
  }, [
    proposalType,
    acceptableCredentialOffers,
    proposalCredentialOfferCid,
    setProposalCredentialOfferCid,
  ]);

  // The same prefill for the Accept Paid Credential form.
  useEffect(() => {
    if (
      proposalType === "accept_paid_credential" &&
      acceptablePaidCredentialOffers.length === 1 &&
      !proposalCredentialOfferCid
    ) {
      setProposalCredentialOfferCid(acceptablePaidCredentialOffers[0].contract_id);
    }
  }, [
    proposalType,
    acceptablePaidCredentialOffers,
    proposalCredentialOfferCid,
    setProposalCredentialOfferCid,
  ]);

  // Accept Paid Credential locks Amulets this party owns as the deposit.
  const fetchOwnAmulets = useCallback(async () => {
    setOwnAmuletsLoading(true);
    try {
      const contracts = await fetchContractsByTemplate(TEMPLATE_AMULET, { includePayload: true });
      setOwnAmulets(ownedAmulets(contracts, partyId));
    } catch (e) {
      console.error("Failed to fetch Amulets:", e);
    } finally {
      setOwnAmuletsLoading(false);
    }
  }, [fetchContractsByTemplate, partyId]);

  useEffect(() => {
    if (proposalType === "accept_paid_credential") {
      fetchOwnAmulets();
    }
  }, [proposalType, fetchOwnAmulets]);

  // Read the newest open mining round from scan for the Accept Paid
  // Credential form. Without DECPM_SCAN_URL the node answers 422, and the
  // form shows its message.
  const fetchOpenMiningRound = useCallback(async () => {
    setOpenMiningRoundLoading(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/open-mining-round`);
      const data = await res.json();
      if (res.ok) {
        const round = data as OpenMiningRoundInfo;
        setOpenMiningRound(round);
        setOpenMiningRoundError("");
        setProposalOpenMiningRoundCid(round.contract_id);
      } else {
        setOpenMiningRound(null);
        setOpenMiningRoundError(data.error || "Failed to read the open mining round");
      }
    } catch (e) {
      setOpenMiningRound(null);
      setOpenMiningRoundError(e instanceof Error ? e.message : String(e));
    } finally {
      setOpenMiningRoundLoading(false);
    }
  }, [setProposalOpenMiningRoundCid]);

  useEffect(() => {
    if (proposalType === "accept_paid_credential") {
      fetchOpenMiningRound();
    }
  }, [proposalType, fetchOpenMiningRound]);

  return {
    openMiningRound,
    setOpenMiningRound,
    openMiningRoundError,
    setOpenMiningRoundError,
    openMiningRoundLoading,
    setOpenMiningRoundLoading,
    credentialOffers,
    credentialOffersLoading,
    fetchCredentialOffers,
    ownAmulets,
    setOwnAmulets,
    ownAmuletsLoading,
    setOwnAmuletsLoading,
    acceptableCredentialOffers,
    acceptablePaidCredentialOffers,
    chosenPaidOffer,
    chosenDepositUsd,
    fetchOwnAmulets,
    fetchOpenMiningRound,
  };
}
