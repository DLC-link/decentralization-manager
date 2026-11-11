import { describe, expect, it } from "vitest";

import {
  buildCreateDelegatedBatchedMarkersProxy,
  buildCreateProviderServiceRequest,
  buildCreateUserServiceRequest,
  buildRequestDevNetFeaturedAppRight,
  buildSetEnableResultContracts,
  buildSetupUtility,
} from "./utilityOnboardingPayloads";

describe("buildSetupUtility", () => {
  it("sends no additional identifiers and both creation flags", () => {
    expect(
      buildSetupUtility({
        proposalProviderServiceCid: "ps-1",
        proposalOperator: "op::1",
        proposalInstrumentIdText: "cTM",
        proposalCreateTransferRule: false,
        proposalCreateAllocationFactory: true,
      }),
    ).toEqual({
      type: "setup_utility",
      provider_service_cid: "ps-1",
      operator: "op::1",
      instrument_id_text: "cTM",
      additional_identifiers: [],
      create_transfer_rule: false,
      create_allocation_factory: true,
    });
  });
});

describe("service requests", () => {
  it("names the operator and the provider or user", () => {
    expect(
      buildCreateProviderServiceRequest({ proposalOperator: "op::1", proposalProvider: "gov::1" }),
    ).toEqual({ type: "create_provider_service_request", operator: "op::1", provider: "gov::1" });
    expect(
      buildCreateUserServiceRequest({ proposalOperator: "op::1", proposalUser: "gov::1" }),
    ).toEqual({ type: "create_user_service_request", operator: "op::1", user: "gov::1" });
  });
});

describe("buildSetEnableResultContracts", () => {
  it("maps the three menu choices to true, false and unset", () => {
    const build = (choice: "true" | "false" | "clear") =>
      buildSetEnableResultContracts({
        proposalRegistrarServiceCid: "rs-1",
        proposalEnableResultContracts: choice,
      });
    expect(build("true")).toEqual({
      type: "set_enable_result_contracts",
      registrar_service_cid: "rs-1",
      enable_result_contracts: true,
    });
    expect(build("false")).toMatchObject({ enable_result_contracts: false });
    expect(build("clear")).toMatchObject({ enable_result_contracts: undefined });
  });
});

describe("single-field proposals", () => {
  it("sends the operator of the markers proxy", () => {
    expect(buildCreateDelegatedBatchedMarkersProxy({ proposalOperator: "op::1" })).toEqual({
      type: "create_delegated_batched_markers_proxy",
      operator: "op::1",
    });
  });

  it("trims the amulet rules cid of the featured app right request", () => {
    expect(buildRequestDevNetFeaturedAppRight({ amuletRulesCid: " ar-1 " })).toEqual({
      type: "request_dev_net_featured_app_right",
      amulet_rules_cid: "ar-1",
    });
  });
});
