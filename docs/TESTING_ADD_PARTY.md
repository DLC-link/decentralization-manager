# Testing Guide: Add Party to Decentralized Party

This guide provides manual testing steps to verify that adding a new participant to an existing decentralized party works correctly.

## Setup

Before testing, ensure you have:

- **2 existing participants** (A + B) with a decentralized party
- **1 new participant** (C) to add
- **At least 1 active contract** owned by the party (to test ACS sync)

## 1. Pre-Add Verification

| Check | How | Expected |
|-------|-----|----------|
| Party exists | UI shows party in governance section | Party visible with 2 members |
| Contracts exist | Check contracts page | At least 1 contract owned by party |
| New participant visible | Check peers list | C shows as available peer |

## 2. Execute Add Party

1. Open governance section
2. Click "Add Party" on the decentralized party
3. Select participant C from dropdown
4. Set threshold (e.g., 2 for majority of 3)
5. Click submit
6. Wait for workflow to complete (~2 min)

**Watch for:** Progress updates, no error messages

## 3. Post-Add Verification

| Check | How | Expected |
|-------|-----|----------|
| C appears in party members | Refresh governance UI | 3 members listed (A, B, C) |
| Threshold updated | Check party details | New threshold shown |
| C sees existing contracts | Log into C's UI, check contracts | Same contracts visible as A/B see |

## 4. Functional Tests

### Test A: New member can view contracts

1. Log into participant C's dashboard
2. Navigate to contracts
3. Verify party's contracts are visible

**Pass:** C sees the same contracts as A and B

### Test B: New member can sign transactions

1. Create a new contract requiring party signature
2. Check if C can sign/approve
3. Complete the transaction

**Pass:** Transaction succeeds with C's participation

### Test C: New member participates in governance

1. Try to kick a member (requires signatures)
2. Verify C's signature is requested
3. Complete with C signing

**Pass:** Governance action completes with C's signature

### Test D: Threshold is enforced

1. With threshold=2 and 3 members
2. Try an action with only 1 signature
3. Should fail/wait for more signatures
4. Add 2nd signature
5. Should succeed

**Pass:** Actions require exactly threshold signatures

## 5. Negative Tests

| Test | Action | Expected |
|------|--------|----------|
| Can't add same member twice | Try adding C again | Error: already a member |

## 6. Quick Smoke Test

If time is limited, verify these 3 things:

1. **C is in the member list** - UI shows 3 members
2. **C sees the contracts** - Log into C, contracts visible
3. **C can sign something** - Any action requiring party signature succeeds with C

## Common Failure Modes

| Symptom | Likely Cause |
|---------|--------------|
| C doesn't see contracts | ACS sync failed (check repair mode enabled) |
| C not in member list | Topology didn't propagate (wait 30s, refresh) |
| C can't sign | Keys not properly registered |
| Workflow stuck | One participant offline during P2P phase |

## Success Criteria

The add party flow **works** if:

- New member appears in party governance
- New member can see all existing contracts
- New member can participate in signing
- Threshold rules are enforced correctly
