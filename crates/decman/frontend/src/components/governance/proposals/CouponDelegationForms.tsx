import { Alert, Box, Button, MenuItem, TextField, Typography } from "@mui/material";
import { TextHelp, fieldHelpAdornment } from "../../FieldHelp";
import { HelpTextField } from "../fields/HelpTextField";
import {
  formatSplitShare,
  parseSplitWeights,
  splitFromWeights,
  splitRemainderRow,
  validateDelegationSplit,
} from "../rewardSplit";
import type { ProposalState } from "./useProposalState";
import type { CouponReassignmentDelegationSummary } from "../../../types";

interface Props {
  state: ProposalState;
}

/** The Setup and Revoke Coupon Reassignment Delegation forms. */
export const CouponDelegationForms = ({ state }: Props) => {
  const {
    proposalType,
    proposalDelegationDso,
    setProposalDelegationDso,
    proposalDelegationAssigners,
    setProposalDelegationAssigners,
    proposalDelegationSplit,
    setProposalDelegationSplit,
    proposalPriorDelegation,
    setProposalPriorDelegation,
    activeDelegations,
    activeDelegationLoading,
    activeDelegationError,
    proposalRevokeDelegationCid,
    setProposalRevokeDelegationCid,
  } = state;

  // One delegation field, three states: nothing to pick, one prefilled, or a
  // dropdown when the ledger holds several. Shared by the setup and revoke
  // forms, which differ only in wording. Several active is an anomaly the
  // singleton guard cannot prevent, so it is called out rather than hidden.
  const delegationPicker = (opts: {
    label: string;
    value: string;
    onChange: (v: string) => void;
    help: string;
    emptyText: string;
    /// What going ahead on a failed read costs, so the warning names the stake.
    blindRisk: string;
  }) => {
    const many = activeDelegations.length > 1;
    const one = activeDelegations.length === 1 ? activeDelegations[0] : null;
    const describe = (d: CouponReassignmentDelegationSummary) =>
      `${d.assigners.length} assigner${d.assigners.length === 1 ? "" : "s"}, ` +
      `${d.beneficiary_count} beneficiar${d.beneficiary_count === 1 ? "y" : "ies"}`;
    return (
      <>
        {many && (
          <Alert severity="warning" sx={{ mb: 1 }}>
            <Typography variant="caption" component="div">
              This party has <strong>{activeDelegations.length} active delegations</strong>.
              Only one should exist. The automation uses the newest, marked{" "}
              <em>in use</em> below; the others are inert but still exerciseable.
            </Typography>
          </Alert>
        )}
        <TextField
          label={opts.label}
          value={opts.value}
          onChange={(e) => opts.onChange(e.target.value)}
          fullWidth
          select={many}
          required={activeDelegations.length > 0}
          // Disabled only when the ledger genuinely holds none. After a failed
          // read we do not know, so leave it typeable — telling someone to fill
          // a field in by hand while disabling it is worse than either alone.
          disabled={activeDelegations.length === 0 && !activeDelegationError}
          error={!!activeDelegationError}
          helperText={
            activeDelegationLoading
              ? "Reading this party's current delegations…"
              : activeDelegationError
                ? `${activeDelegationError} Fill it in by hand or retry — ${opts.blindRisk}`
                : many
                  ? "Pick the delegation this vote acts on."
                  : one
                    ? `Prefilled with this party's active delegation (${describe(one)}).`
                    : opts.emptyText
          }
          slotProps={
            many
              ? undefined
              : {
                  input: {
                    sx: { fontFamily: "var(--font-mono)", fontSize: "0.8rem" },
                    endAdornment: fieldHelpAdornment(opts.help, `Help for ${opts.label}`),
                  },
                }
          }
        >
          {many &&
            activeDelegations.map((d, idx) => (
              <MenuItem key={d.cid} value={d.cid}>
                <Typography variant="caption" sx={{ fontFamily: "var(--font-mono)" }}>
                  {d.cid.slice(0, 24)}… — {describe(d)}
                  {idx === 0 ? " — in use" : ""}
                </Typography>
              </MenuItem>
            ))}
        </TextField>
      </>
    );
  };

  return (
    <>
      {proposalType === "revoke_coupon_reassignment_delegation" &&
        delegationPicker({
          label: "Delegation Contract ID",
          value: proposalRevokeDelegationCid,
          onChange: setProposalRevokeDelegationCid,
          help: "Contract id of the active CouponReassignmentDelegation to archive. It is read from the ledger, not typed. Reassignment stops for this party until a new delegation is voted in.",
          emptyText:
            "This party has no active delegation, so there is nothing to revoke.",
          blindRisk:
            "a wrong contract id fails at execute, after the vote.",
        })}

      {proposalType === "setup_coupon_reassignment_delegation" && (
        <>
          <Alert severity="info" sx={{ mb: 1 }}>
            <Typography variant="caption" component="div">
              The split below is <strong>baked into the delegation</strong>.
              Changing it later needs another vote. Two rules the ledger
              enforces exactly, and which reject a vote at execute:
            </Typography>
            <Typography variant="caption" component="ul" sx={{ pl: 2, mb: 0, mt: 0.5 }}>
              <li>
                Shares must sum to <strong>exactly 1.0</strong>, compared as
                exact Decimal — so an even 3-way split is not expressible as
                a repeated decimal. Enter <strong>whole-number weights</strong>{" "}
                and the exact percentages are derived; the rounding
                remainder goes to the largest weight, so a confirmer can
                reproduce the split from the weights alone.
              </li>
              <li>
                Nothing is implicitly left to this party. To keep a
                remainder, <strong>add this party as its own beneficiary</strong>.
              </li>
            </Typography>
          </Alert>
          <HelpTextField
            label="DSO Party"
            value={proposalDelegationDso}
            onChange={(e) => setProposalDelegationDso(e.target.value)}
            fullWidth
            required
            help="The DSO whose coupons this delegation may assign. Anyone can mint a coupon naming themselves DSO, so the automation ignores every coupon whose DSO is not this one. Getting it wrong silently assigns nothing."
          />
          {delegationPicker({
            label: "Replaces Delegation",
            value: proposalPriorDelegation,
            onChange: setProposalPriorDelegation,
            help: "Contract id of the delegation this one replaces — it is archived in the same transaction. It is read from the ledger, not typed: creating a second delegation stops assignment entirely, so leaving it blank while one is live is rejected with 409.",
            emptyText:
              "This party has no active delegation, so there is nothing to replace.",
            blindRisk:
              "leaving it blank while a delegation is live is rejected with 409.",
          })}
          <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
            <TextHelp text="Member parties allowed to run the reassignment. Any ONE of them suffices (1-of-n), so this is liveness, not a threshold. An assigner's participant must also host this governance party, or it cannot read the coupons.">
              Assigners (any one may reassign)
            </TextHelp>
          </Typography>
          {/* Same shape as a beneficiary row, without the weight: the
              party id gets the full width, Remove sits beneath it.
              Sharing the row truncated the id past its namespace. */}
          {proposalDelegationAssigners.map((a, idx) => (
            <Box key={idx} sx={{ mb: 2 }}>
              <TextField
                label={`Assigner ${idx + 1}`}
                value={a}
                onChange={(e) => {
                  const updated = [...proposalDelegationAssigners];
                  updated[idx] = e.target.value;
                  setProposalDelegationAssigners(updated);
                }}
                size="small"
                fullWidth
                slotProps={{
                  input: { sx: { fontFamily: "var(--font-mono)", fontSize: "0.8rem" } },
                }}
              />
              <Box sx={{ display: "flex", mt: 1 }}>
                <Button
                  size="small"
                  color="error"
                  onClick={() =>
                    setProposalDelegationAssigners(
                      proposalDelegationAssigners.filter((_, i) => i !== idx),
                    )
                  }
                >
                  Remove
                </Button>
              </Box>
            </Box>
          ))}
          <Box>
            <Button
              size="small"
              onClick={() =>
                setProposalDelegationAssigners([...proposalDelegationAssigners, ""])
              }
            >
              Add Assigner
            </Button>
          </Box>
          <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
            <TextHelp text="Who receives the reassigned coupons, and in what share. Enter whole-number weights — equal thirds is 1/1/1 — and the exact percentages are derived below. Each beneficiary mints its own coupons afterwards; this party does not mint for them.">
              Beneficiary split (party + weight)
            </TextHelp>
          </Typography>
          {(() => {
            const weights = parseSplitWeights(proposalDelegationSplit);
            const shares = weights ? splitFromWeights(weights) : null;
            const remainderRow = weights ? splitRemainderRow(weights) : -1;
            return (
              <>
                {/* Party id on its own full-width row, weight beneath it.
                    A party id is ~70 characters and only its prefix
                    distinguishes two parties in the same namespace, so
                    sharing the row with the weight hides the part that
                    matters. */}
                {proposalDelegationSplit.map((b, idx) => (
                  <Box key={idx} sx={{ mb: 2 }}>
                    <TextField
                      label={`Beneficiary Party ${idx + 1}`}
                      value={b.beneficiary}
                      onChange={(e) => {
                        const updated = [...proposalDelegationSplit];
                        updated[idx] = { ...b, beneficiary: e.target.value };
                        setProposalDelegationSplit(updated);
                      }}
                      size="small"
                      fullWidth
                      slotProps={{
                        input: { sx: { fontFamily: "var(--font-mono)", fontSize: "0.8rem" } },
                      }}
                    />
                    <Box
                      sx={{
                        display: "flex",
                        gap: 1,
                        mt: 1,
                        alignItems: "center",
                      }}
                    >
                      <HelpTextField
                        label="Weight"
                        value={b.weight}
                        onChange={(e) => {
                          const updated = [...proposalDelegationSplit];
                          updated[idx] = { ...b, weight: e.target.value };
                          setProposalDelegationSplit(updated);
                        }}
                        size="small"
                        sx={{ width: 110 }}
                        help="A whole number. Only the ratio matters: 1/1/1 is equal thirds, 80/20 is four to one. The exact percentage is derived."
                      />
                      <Typography
                        variant="caption"
                        sx={{ flex: 1, fontFamily: "var(--font-mono)" }}
                        color={shares ? "text.primary" : "text.disabled"}
                      >
                        {shares
                          ? `${formatSplitShare(shares[idx])}${idx === remainderRow && shares.length > 1 ? " ⟵ +rem" : ""}`
                          : "—"}
                      </Typography>
                      <Button
                        size="small"
                        color="error"
                        onClick={() =>
                          setProposalDelegationSplit(
                            proposalDelegationSplit.filter((_, i) => i !== idx),
                          )
                        }
                      >
                        Remove
                      </Button>
                    </Box>
                  </Box>
                ))}
                <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
                  <Button
                    size="small"
                    onClick={() =>
                      setProposalDelegationSplit([
                        ...proposalDelegationSplit,
                        { beneficiary: "", weight: "1" },
                      ])
                    }
                  >
                    Add Beneficiary
                  </Button>
                  {proposalDelegationSplit.length > 0 &&
                    (() => {
                      const err = validateDelegationSplit(proposalDelegationSplit);
                      return (
                        <Typography
                          variant="caption"
                          color={err ? "error.main" : "success.main"}
                        >
                          {err ??
                            `Sums to exactly 1.0${
                              shares && shares.length > 1
                                ? ` — the rounding remainder goes to the largest weight (row ${remainderRow + 1})`
                                : ""
                            }`}
                        </Typography>
                      );
                    })()}
                </Box>
              </>
            );
          })()}
        </>
      )}
    </>
  );
};
