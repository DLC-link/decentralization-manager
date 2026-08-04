import { useState } from "react";

import type { AcsView, TemplateId } from "../types";
import { Card, Notice } from "./ui";

const ARGUMENT_PLACEHOLDER = `{
  "owner": "alice::1220…",
  "amount": 1
}`;

/** The party's active contracts, plus a form that creates one: prepared on a
 *  host, signed in the wallet, executed back on the host. */
export function Contracts({
  acs,
  busy,
  onRefresh,
  onCreate,
}: {
  acs: AcsView | null;
  busy: boolean;
  onRefresh: () => void;
  onCreate: (templateId: TemplateId, createArguments: unknown) => void;
}) {
  const [packageId, setPackageId] = useState("");
  const [moduleName, setModuleName] = useState("");
  const [entityName, setEntityName] = useState("");
  const [argumentsText, setArgumentsText] = useState("");
  const [parseError, setParseError] = useState<string | null>(null);

  const submit = () => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(argumentsText || "{}");
    } catch (e) {
      setParseError(e instanceof Error ? e.message : "invalid JSON");
      return;
    }
    setParseError(null);
    onCreate(
      { package_id: packageId.trim(), module_name: moduleName.trim(), entity_name: entityName.trim() },
      parsed,
    );
  };

  return (
    <>
      <Card
        title="Active contracts"
        eyebrow={acs ? `served by ${acs.served_by}` : "the party's own ledger view"}
        aside={
          <button className="btn-secondary btn-sm" type="button" onClick={onRefresh} disabled={busy}>
            ⟳ Refresh
          </button>
        }
      >
        {acs && acs.contracts.length > 0 ? (
          <div className="table-scroll">
            <table>
              <thead>
                <tr>
                  <th>Contract id</th>
                  <th>Template</th>
                  <th>Payload</th>
                </tr>
              </thead>
              <tbody>
                {acs.contracts.map((contract) => (
                  <tr key={contract.contract_id}>
                    <td>{contract.contract_id}</td>
                    <td>{contract.template_id}</td>
                    <td>{JSON.stringify(contract.create_arguments)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <p className="empty t-sm">
            {acs
              ? "No active contracts yet."
              : "The ledger becomes readable once at least one host has the party."}
          </p>
        )}
      </Card>

      <Card title="Create a contract" eyebrow="prepare · sign here · execute">
        {parseError ? <Notice kind="error">Arguments: {parseError}</Notice> : null}
        <div className="form-row">
          <label className="field">
            <span>Package id</span>
            <input
              value={packageId}
              onChange={(e) => setPackageId(e.target.value)}
              placeholder="a1b2c3…"
              spellCheck={false}
            />
          </label>
          <label className="field">
            <span>Module</span>
            <input
              value={moduleName}
              onChange={(e) => setModuleName(e.target.value)}
              placeholder="MyModule"
              spellCheck={false}
            />
          </label>
          <label className="field">
            <span>Entity</span>
            <input
              value={entityName}
              onChange={(e) => setEntityName(e.target.value)}
              placeholder="MyTemplate"
              spellCheck={false}
            />
          </label>
        </div>
        <label className="field" style={{ marginTop: 16 }}>
          <span>Create arguments (JSON)</span>
          <textarea
            value={argumentsText}
            onChange={(e) => setArgumentsText(e.target.value)}
            placeholder={ARGUMENT_PLACEHOLDER}
            spellCheck={false}
          />
        </label>
        <p className="field-hint">
          Field types are inferred from the JSON: a string that parses as a party id becomes a
          Party, other strings become Text, whole numbers become Int64. Variants, enums, maps and
          dates are not expressible this way.
        </p>
        <button
          className="btn-primary"
          type="button"
          style={{ marginTop: 16 }}
          onClick={submit}
          disabled={busy || !packageId.trim() || !moduleName.trim() || !entityName.trim()}
        >
          {busy ? "Submitting…" : "Sign and submit"}
        </button>
      </Card>
    </>
  );
}
