# Quantstamp audit — design acknowledgements

This document captures intentional design decisions in response to the Quantstamp audit (report dated 2026-05-22, commit `c3f578b`). The items below remain in the code as-is by deliberate choice — they are addressed through process and documentation rather than code changes.

## DLC-5 — `GovernanceRules` permits 1-of-1 configurations

**Auditor finding.** The `GovernanceRules` `ensure` clause accepts `members = {p}` and `threshold = 1` as a valid configuration. A single party can therefore unilaterally confirm and execute every privileged governance operation, bypassing the multi-signature requirement.

**Rationale for keeping 1-of-1 legal.** Single-signer `GovernanceRules` is intentionally supported for bootstrap and development environments — operators commonly start with a 1-of-1 configuration and add members through `SelfAction_AddMemberAndSetThreshold` votes as the governance group forms. Hard-coding `members >= 2 && threshold >= 2` would foreclose this bootstrap path on-chain.

This is an operational constraint, not an on-chain one. Production deployments are responsible for transitioning to a real quorum (members ≥ 2, threshold ≥ 2) before they go live. Operators that need an enforced lower bound can wrap the deployment behind their own admission process.
