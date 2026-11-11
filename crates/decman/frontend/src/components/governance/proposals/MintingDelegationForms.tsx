import { HelpTextField } from "../fields/HelpTextField";
import type { ProposalState } from "./useProposalState";

interface Props {
  state: ProposalState;
}

/** The Setup Minting Delegation and Accept External Party Setup forms. */
export const MintingDelegationForms = ({ state }: Props) => {
  const {
    proposalType,
    proposalExpectedDso,
    setProposalExpectedDso,
    proposalDelegate,
    setProposalDelegate,
    proposalDelegationExpiresAt,
    setProposalDelegationExpiresAt,
    proposalAmuletMergeLimit,
    setProposalAmuletMergeLimit,
    proposalDescription,
    setProposalDescription,
    proposalExternalPartySetupCid,
    setProposalExternalPartySetupCid,
  } = state;

  return (
    <>
      {proposalType === "setup_minting_delegation" && (
        <>
          <HelpTextField
            size="small"
            label="Delegate Party"
            value={proposalDelegate}
            onChange={(e) => setProposalDelegate(e.target.value)}
            fullWidth
            required
            help="Party id of the validator node operator authorized to mint the decentralized party's reward coupons. It accepts the resulting MintingDelegationProposal out-of-band via the wallet API."
          />
          <HelpTextField
            size="small"
            label="DSO Party"
            value={proposalExpectedDso}
            onChange={(e) => setProposalExpectedDso(e.target.value)}
            fullWidth
            required
            help="Party id of the Splice DSO; the delegation's mint transfers verify the AmuletRules contract belongs to this DSO."
          />
          <HelpTextField
            size="small"
            label="Expires At"
            type="datetime-local"
            value={proposalDelegationExpiresAt}
            onChange={(e) =>
              setProposalDelegationExpiresAt(e.target.value)
            }
            fullWidth
            required
            slotProps={{
              inputLabel: { shrink: true },
            }}
            help="When the delegation stops being valid. There is no auto-renewal — a new proposal must be voted in before this time to keep collecting rewards."
          />
          <HelpTextField
            size="small"
            label="Amulet Merge Limit"
            type="number"
            value={proposalAmuletMergeLimit}
            onChange={(e) => setProposalAmuletMergeLimit(e.target.value)}
            fullWidth
            required
            help="Number of amulet contracts to keep after auto-merging; the delegate merges contracts once there are strictly more than this number. Must be positive."
          />
          <HelpTextField
            size="small"
            label="Description"
            value={proposalDescription}
            onChange={(e) => setProposalDescription(e.target.value)}
            fullWidth
            required
            help="Free-form note recorded on the proposal, e.g. why this delegation is being set up."
          />
        </>
      )}

      {proposalType === "accept_external_party_setup" && (
        <HelpTextField
          size="small"
          label="External Party Setup Proposal Contract Id"
          value={proposalExternalPartySetupCid}
          onChange={(e) =>
            setProposalExternalPartySetupCid(e.target.value)
          }
          fullWidth
          required
          help="Contract id of the ExternalPartySetupProposal the validator operator created (via POST /api/validator/v0/admin/external-party/setup-proposal). Accepting it creates the decentralized party's ValidatorRight + TransferPreapproval, unblocking reward collection."
        />
      )}
    </>
  );
};
