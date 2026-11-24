import { describe, expect, it } from "vitest";

import {
  buildCreateProviderConfiguration,
  buildCreateRegistrarServiceRequest,
  buildOnboardRegistrar,
} from "./registrarPayloads";

describe("buildCreateProviderConfiguration", () => {
  it("turns both requirement lists into payload requirements", () => {
    expect(
      buildCreateProviderConfiguration({
        proposalProviderServiceCid: "ps-1",
        proposalRegistrarRequirements: [{ issuer: " gov::1 ", claimsText: "registrar,true" }],
        proposalHolderRequirements: [],
      }),
    ).toEqual({
      type: "create_provider_configuration",
      provider_service_cid: "ps-1",
      registrar_requirements: [
        { issuer: "gov::1", required_claims: [{ property: "registrar", value: "true" }] },
      ],
      holder_requirements: [],
    });
  });

  it("names the list of a row without an issuer", () => {
    expect(() =>
      buildCreateProviderConfiguration({
        proposalProviderServiceCid: "ps-1",
        proposalRegistrarRequirements: [],
        proposalHolderRequirements: [{ issuer: "", claimsText: "kyc,true" }],
      }),
    ).toThrow("Holder requirement 1: issuer party is required");
  });
});

describe("buildCreateRegistrarServiceRequest", () => {
  it("sends the operator, the provider and both creation flags", () => {
    expect(
      buildCreateRegistrarServiceRequest({
        proposalOperator: "op::1",
        proposalProvider: "prov::1",
        proposalCreateTransferRule: true,
        proposalCreateAllocationFactory: false,
      }),
    ).toEqual({
      type: "create_registrar_service_request",
      operator: "op::1",
      provider: "prov::1",
      create_transfer_rule: true,
      create_allocation_factory: false,
    });
  });
});

describe("buildOnboardRegistrar", () => {
  it("names the service, the request and the configuration", () => {
    expect(
      buildOnboardRegistrar({
        proposalProviderServiceCid: "ps-1",
        proposalRegistrarServiceRequestCid: "rsr-1",
        proposalProviderConfigurationCid: "pc-1",
      }),
    ).toEqual({
      type: "onboard_registrar",
      provider_service_cid: "ps-1",
      registrar_service_request_cid: "rsr-1",
      provider_configuration_cid: "pc-1",
    });
  });
});
