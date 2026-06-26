//! A small data-driven form engine for composing governance actions and
//! proposals. Each action / proposal variant is described by a list of
//! [`ComposerField`]s; [`build_payload`] assembles the typed JSON body the
//! decman API expects (the `{ "type": ..., ... }` internally-tagged shape).
//!
//! This keeps the 30-odd variants as data rather than bespoke forms, while
//! still supporting nested objects (dot-path keys, e.g. `instrument_id.admin`)
//! and arrays of records (comma-separated rows, e.g. beneficiaries).

use serde_json::{Map, Value};

/// What kind of input a [`ComposerField`] collects.
pub enum FieldKind {
    /// Free text (party id, contract id, description, decimal amount, …).
    Text,
    /// A whole number, serialized as a JSON integer.
    Int,
    /// A boolean toggle (← / → flips it).
    Bool,
    /// One choice from a fixed set (← / → cycles). Option values `"true"` /
    /// `"false"` serialize as booleans, `"null"` / `"clear"` as JSON null.
    Select(Vec<SelectOption>),
    /// Newline-separated plain strings → a JSON array of strings.
    List,
    /// Newline-separated rows, each split on commas into `columns` → a JSON
    /// array of `{ column: value }` objects.
    Rows(Vec<&'static str>),
}

/// One option in a [`FieldKind::Select`].
pub struct SelectOption {
    pub value: &'static str,
    pub label: &'static str,
}

impl SelectOption {
    /// Build a select option.
    pub fn new(value: &'static str, label: &'static str) -> Self {
        Self { value, label }
    }
}

/// One input in a composer form.
pub struct ComposerField {
    /// JSON key. May contain `.` to build nested objects (`a.b` → `{a:{b:..}}`).
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    /// Current edit buffer. For `Bool`/`Select` this holds the chosen value.
    pub value: String,
    /// When true, an empty value is omitted from the payload instead of erroring.
    pub optional: bool,
    pub help: &'static str,
}

/// Where a composed form is submitted.
pub enum ComposerSubmit {
    /// `POST /governance/confirm` (a brand-new off-chain action).
    Confirm,
    /// `POST /governance/propose`.
    Propose,
}

/// A full composer form for one action / proposal variant.
pub struct Composer {
    pub title: String,
    /// The internally-tagged `"type"` discriminant.
    pub action_type: &'static str,
    pub submit: ComposerSubmit,
    pub party_id: String,
    pub party_name: String,
    /// The party-level governance type (`core_self` / `vault`), used for the
    /// confirm call and to refresh the approvals overlay afterwards.
    pub governance_type: String,
    pub rules_contract_id: String,
    pub fields: Vec<ComposerField>,
    /// Index of the focused field; `fields.len()` is the virtual submit row.
    pub cursor: usize,
}

/// Prefill context shared by every form.
pub struct ComposerContext {
    pub party_id: String,
    /// The operator party (from `/operator-info`); empty if unknown.
    pub operator_party: String,
    /// The current governance threshold, used to prefill threshold fields.
    pub default_threshold: i64,
}

/// One entry in the action / proposal type picker.
pub struct TypeOption {
    pub key: &'static str,
    pub label: &'static str,
}

const fn opt(key: &'static str, label: &'static str) -> TypeOption {
    TypeOption { key, label }
}

/// The governance action types offered for a party's governance type, in the
/// same order and split the web frontend uses (`core_self` shows the four
/// self-management actions; `vault` shows the rest).
pub fn action_types(governance_type: &str) -> Vec<TypeOption> {
    if governance_type == "core_self" {
        vec![
            opt("governance_add_member", "Add governance member"),
            opt("governance_remove_member", "Remove governance member"),
            opt("governance_set_threshold", "Set governance threshold"),
            opt("governance_set_timeout", "Set governance timeout"),
        ]
    } else {
        vec![
            opt("vault_pause", "Pause vault"),
            opt("vault_unpause", "Unpause vault"),
            opt("vault_update_limits", "Update vault limits"),
            opt("vault_update_backend", "Update vault backend"),
            opt("vault_update_far_beneficiaries", "Update FAR beneficiaries"),
            opt("vault_deployment", "Deploy vault"),
            opt("yield_epoch_deployment", "Deploy yield epoch"),
            opt(
                "processor_deployment_request",
                "Request processor deployment",
            ),
            opt("utility_create_provider_request", "Create provider service"),
            opt("utility_create_user_request", "Create user service"),
            opt("utility_setup", "Setup utility"),
            opt(
                "utility_accept_holder_service_request",
                "Accept holder service",
            ),
            opt("credential_offer_free", "Offer free credential"),
            opt("credential_accept_free", "Accept free credential"),
            opt("dev_net_feature_app", "DevNet: feature app"),
        ]
    }
}

/// The proposal types offered on a `core_self` party, grouped as in the web
/// frontend (`offer_paid_credential` is omitted — the API form is unimplemented).
pub fn proposal_types() -> Vec<TypeOption> {
    vec![
        opt("generic_vote", "Generic vote"),
        opt("setup_cc_preapproval", "Setup CC preapproval"),
        opt("setup_token_preapproval", "Setup token preapproval"),
        opt("transfer", "Transfer"),
        opt("accept_transfer", "Accept transfer"),
        opt("create_user_service_request", "Create user service request"),
        opt(
            "create_provider_service_request",
            "Create provider service request",
        ),
        opt("setup_utility", "Setup utility"),
        opt(
            "set_provider_app_reward_beneficiaries",
            "Set provider app reward beneficiaries",
        ),
        opt("set_enable_result_contracts", "Set enable result contracts"),
        opt(
            "create_delegated_batched_markers_proxy",
            "Create delegated batched markers proxy",
        ),
        opt("mint", "Mint"),
        opt("burn", "Burn"),
        opt("accept_mint_request", "Accept mint request"),
        opt("accept_burn_request", "Accept burn request"),
        opt("offer_free_credential", "Offer free credential"),
        opt("accept_free_credential", "Accept free credential"),
    ]
}

/// Field constructors.
fn text(key: &'static str, label: &'static str) -> ComposerField {
    field(key, label, FieldKind::Text, String::new(), false, "")
}

fn text_value(key: &'static str, label: &'static str, value: String) -> ComposerField {
    field(key, label, FieldKind::Text, value, false, "")
}

fn text_opt(key: &'static str, label: &'static str) -> ComposerField {
    field(key, label, FieldKind::Text, String::new(), true, "")
}

fn int_value(
    key: &'static str,
    label: &'static str,
    value: i64,
    help: &'static str,
) -> ComposerField {
    field(key, label, FieldKind::Int, value.to_string(), false, help)
}

fn int_opt(key: &'static str, label: &'static str, help: &'static str) -> ComposerField {
    field(key, label, FieldKind::Int, String::new(), true, help)
}

fn boolean(key: &'static str, label: &'static str, default: bool) -> ComposerField {
    let value = if default { "true" } else { "false" }.to_owned();
    field(key, label, FieldKind::Bool, value, false, "")
}

fn select(
    key: &'static str,
    label: &'static str,
    options: Vec<SelectOption>,
    default: &'static str,
) -> ComposerField {
    field(
        key,
        label,
        FieldKind::Select(options),
        default.to_owned(),
        false,
        "",
    )
}

fn rows(key: &'static str, label: &'static str, columns: Vec<&'static str>) -> ComposerField {
    field(
        key,
        label,
        FieldKind::Rows(columns),
        String::new(),
        true,
        "one per line",
    )
}

/// A rows field whose JSON key the API requires: blank input serializes as `[]`
/// (an absent key would fail server-side deserialization).
fn rows_required(
    key: &'static str,
    label: &'static str,
    columns: Vec<&'static str>,
) -> ComposerField {
    field(
        key,
        label,
        FieldKind::Rows(columns),
        String::new(),
        false,
        "one per line",
    )
}

fn list_opt(key: &'static str, label: &'static str) -> ComposerField {
    field(
        key,
        label,
        FieldKind::List,
        String::new(),
        true,
        "one per line",
    )
}

/// A list field whose JSON key the API requires: blank input serializes as `[]`.
fn list_required(key: &'static str, label: &'static str) -> ComposerField {
    field(
        key,
        label,
        FieldKind::List,
        String::new(),
        false,
        "one per line",
    )
}

fn field(
    key: &'static str,
    label: &'static str,
    kind: FieldKind,
    value: String,
    optional: bool,
    help: &'static str,
) -> ComposerField {
    ComposerField {
        key,
        label,
        kind,
        value,
        optional,
        help,
    }
}

/// Instrument-id pair fields (`instrument_id.admin` + `instrument_id.id`),
/// prefilling the admin with `admin_default`.
fn instrument_id(prefix: &'static str, admin_default: String) -> Vec<ComposerField> {
    vec![
        text_value(
            match prefix {
                "asset_instrument_id" => "asset_instrument_id.admin",
                _ => "instrument_id.admin",
            },
            "Instrument admin",
            admin_default,
        ),
        text(
            match prefix {
                "asset_instrument_id" => "asset_instrument_id.id",
                _ => "instrument_id.id",
            },
            "Instrument id",
        ),
    ]
}

/// The fields for a governance action variant. Unknown types yield an empty
/// form (the type tag alone is sent).
pub fn fields_for_action(action_type: &str, ctx: &ComposerContext) -> Vec<ComposerField> {
    let threshold = ctx.default_threshold.max(1);
    match action_type {
        "governance_add_member" => vec![
            text("member", "Member party id"),
            int_value("new_threshold", "New threshold", threshold, ""),
        ],
        "governance_remove_member" => vec![
            text("member", "Member party id"),
            int_value("new_threshold", "New threshold", threshold, ""),
        ],
        "governance_set_threshold" => {
            vec![int_value("new_threshold", "New threshold", threshold, "")]
        }
        "governance_set_timeout" => vec![int_value(
            "new_timeout_microseconds",
            "Timeout (microseconds)",
            3_600_000_000,
            "1 hour = 3,600,000,000 µs",
        )],
        "vault_pause" | "vault_unpause" => vec![text("vault_id", "Vault contract id")],
        "vault_update_limits" => vec![
            text("vault_id", "Vault contract id"),
            text_opt("new_limits.max_total_deposit", "Max total deposit"),
            text_opt("new_limits.min_deposit_amount", "Min deposit amount"),
            text_opt("new_limits.min_withdrawal_amount", "Min withdrawal amount"),
        ],
        "vault_update_backend" => vec![
            text("vault_id", "Vault contract id"),
            text("new_backend_signatory", "New backend signatory"),
        ],
        "vault_update_far_beneficiaries" => vec![
            text("vault_id", "Vault contract id"),
            rows_required(
                "new_beneficiaries",
                "Beneficiaries (party,weight)",
                vec!["beneficiary", "weight"],
            ),
        ],
        "vault_deployment" => {
            let mut fields = vec![
                text("vault_rules_cid", "Vault rules cid"),
                text("vault_name", "Vault name"),
                text("share_symbol", "Share symbol"),
            ];
            fields.extend(instrument_id("asset_instrument_id", String::new()));
            fields.extend([
                text_opt("limits.max_total_deposit", "Max total deposit"),
                text_opt("limits.min_deposit_amount", "Min deposit amount"),
                text_opt("limits.min_withdrawal_amount", "Min withdrawal amount"),
                text("vault_backend_signatory", "Backend signatory"),
                text("allocation_factory_cid", "Allocation factory cid"),
                text("registrar_service_cid", "Registrar service cid"),
            ]);
            fields
        }
        "yield_epoch_deployment" => {
            let mut fields = vec![
                text("vault_rules_cid", "Vault rules cid"),
                text("vault_cid", "Vault contract id"),
            ];
            fields.extend(instrument_id("asset_instrument_id", String::new()));
            fields.push(text("vault_backend_signatory", "Backend signatory"));
            fields
        }
        "processor_deployment_request" => vec![
            text("vault_processor_rules_cid", "Processor rules cid"),
            text("vault_backend_signatory", "Backend signatory"),
            text("allocation_factory_cid", "Burn/mint factory cid"),
            list_required("initial_supported_vaults", "Initial supported vault cids"),
        ],
        "utility_create_provider_request" | "utility_create_user_request" => {
            vec![text_value(
                "operator",
                "Operator party",
                ctx.operator_party.clone(),
            )]
        }
        "utility_setup" => vec![
            text_value("operator", "Operator party", ctx.operator_party.clone()),
            text("provider_service_cid", "Provider service cid"),
            text("user_service_cid", "User service cid"),
        ],
        "utility_accept_holder_service_request" => vec![
            text_value("operator", "Operator party", ctx.operator_party.clone()),
            text("provider_service_cid", "Provider service cid"),
            text("holder_service_request_cid", "Holder service request cid"),
            text("holder", "Holder party"),
        ],
        "credential_offer_free" => vec![
            text_value("operator", "Operator party", ctx.operator_party.clone()),
            text("user_service_cid", "User service cid"),
            text("holder", "Holder party"),
            text("id", "Credential id"),
            text("description", "Description"),
            rows_required(
                "claims",
                "Claims (subject,property,value)",
                vec!["subject", "property", "value"],
            ),
        ],
        "credential_accept_free" => vec![
            text_value("operator", "Operator party", ctx.operator_party.clone()),
            text("user_service_cid", "User service cid"),
            text("credential_offer_cid", "Credential offer cid"),
        ],
        "dev_net_feature_app" => vec![text("amulet_rules_cid", "Amulet rules cid")],
        _ => Vec::new(),
    }
}

/// The fields for a proposal variant.
pub fn fields_for_proposal(proposal_type: &str, ctx: &ComposerContext) -> Vec<ComposerField> {
    let party = ctx.party_id.clone();
    let operator = || text_value("operator", "Operator party", ctx.operator_party.clone());
    match proposal_type {
        "generic_vote" => vec![text("description", "Vote description")],
        "setup_cc_preapproval" => vec![
            text("provider", "Provider party"),
            text("expected_dso", "Expected DSO party"),
        ],
        "setup_token_preapproval" => vec![
            operator(),
            text("instrument_admin", "Instrument admin"),
            rows("instrument_allowances", "Allowance ids", vec!["id"]),
        ],
        "transfer" => {
            let mut fields = vec![
                text("transfer_factory_cid", "Transfer factory cid"),
                text("expected_admin", "Expected admin"),
                text("receiver", "Receiver party"),
                text("amount", "Amount"),
            ];
            fields.extend(instrument_id("instrument_id", String::new()));
            fields.push(list_opt("input_holding_cids", "Input holding cids"));
            fields.push(int_opt(
                "validity_window_hours",
                "Validity window (hours)",
                "default 24",
            ));
            fields
        }
        "accept_transfer" => vec![text("transfer_instruction_cid", "Transfer instruction cid")],
        "create_user_service_request" => vec![operator(), text_value("user", "User party", party)],
        "create_provider_service_request" => {
            vec![operator(), text_value("provider", "Provider party", party)]
        }
        "setup_utility" => vec![
            text("provider_service_cid", "Provider service cid"),
            operator(),
            text("instrument_id_text", "Instrument id"),
            boolean("create_transfer_rule", "Create transfer rule", true),
            boolean(
                "create_allocation_factory",
                "Create allocation factory",
                true,
            ),
        ],
        "set_provider_app_reward_beneficiaries" => vec![
            text("instrument_configuration_cid", "Instrument config cid"),
            rows(
                "provider_app_reward_beneficiaries",
                "Beneficiaries (party,weight) — empty clears",
                vec!["beneficiary", "weight"],
            ),
        ],
        "set_enable_result_contracts" => vec![
            text("registrar_service_cid", "Registrar service cid"),
            select(
                "enable_result_contracts",
                "Enable result contracts",
                vec![
                    SelectOption::new("true", "Enable"),
                    SelectOption::new("false", "Disable"),
                    SelectOption::new("clear", "Clear (None)"),
                ],
                "true",
            ),
        ],
        "create_delegated_batched_markers_proxy" => vec![operator()],
        "mint" => {
            let mut fields = vec![text("allocation_factory_cid", "Allocation factory cid")];
            fields.extend(instrument_id("instrument_id", party));
            fields.extend([
                text("instrument_configuration_cid", "Instrument config cid"),
                text("recipient", "Recipient party"),
                text("amount", "Amount"),
                text("description", "Description"),
            ]);
            fields
        }
        "burn" => {
            let mut fields = vec![text("allocation_factory_cid", "Allocation factory cid")];
            fields.extend(instrument_id("instrument_id", party));
            fields.extend([
                text("instrument_configuration_cid", "Instrument config cid"),
                text("holder", "Holder party"),
                text("amount", "Amount"),
                text("description", "Description"),
            ]);
            fields
        }
        "accept_mint_request" => vec![
            text("mint_request_cid", "Mint request cid"),
            text("instrument_configuration_cid", "Instrument config cid"),
            text("description", "Description"),
        ],
        "accept_burn_request" => vec![
            text("burn_request_cid", "Burn request cid"),
            text("instrument_configuration_cid", "Instrument config cid"),
            text("description", "Description"),
        ],
        "offer_free_credential" => vec![
            text("user_service_cid", "User service cid"),
            text("holder", "Holder party"),
            text("id", "Credential id"),
            text("description", "Description"),
            rows_required(
                "claims",
                "Claims (subject,property,value)",
                vec!["subject", "property", "value"],
            ),
        ],
        "accept_free_credential" => vec![
            text("user_service_cid", "User service cid"),
            text("credential_offer_cid", "Credential offer cid"),
        ],
        _ => Vec::new(),
    }
}

/// Build the typed JSON payload for a composed form, or a human error message
/// describing the first invalid / missing required field.
pub fn build_payload(composer: &Composer) -> Result<Value, String> {
    let mut root = Map::new();
    root.insert(
        "type".to_owned(),
        Value::String(composer.action_type.to_owned()),
    );
    // A nested-object key (e.g. `limits`, `instrument_id`) is always emitted
    // when the form declares any child for it, even if all children are blank —
    // the matching server fields are required, so an absent key fails to parse.
    for field in &composer.fields {
        if let Some((parent, _)) = field.key.split_once('.') {
            root.entry(parent.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
        }
    }
    for field in &composer.fields {
        if let Some(value) = field_value(field)? {
            insert_path(&mut root, field.key, value);
        }
    }
    Ok(Value::Object(root))
}

/// The typed JSON value for a field, or `None` when an optional field is empty.
fn field_value(field: &ComposerField) -> Result<Option<Value>, String> {
    let trimmed = field.value.trim();
    let require = || -> Result<(), String> {
        if field.optional || !trimmed.is_empty() {
            Ok(())
        } else {
            Err(format!("{} is required", field.label))
        }
    };
    match &field.kind {
        FieldKind::Text => {
            require()?;
            Ok((!trimmed.is_empty()).then(|| Value::String(trimmed.to_owned())))
        }
        FieldKind::Int => {
            require()?;
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<i64>()
                .map(|n| Some(Value::from(n)))
                .map_err(|_| format!("{} must be a whole number", field.label))
        }
        FieldKind::Bool => Ok(Some(Value::Bool(trimmed == "true"))),
        FieldKind::Select(_) => {
            require()?;
            Ok(match trimmed {
                "" => None,
                "null" | "clear" => Some(Value::Null),
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                other => Some(Value::String(other.to_owned())),
            })
        }
        FieldKind::List => {
            let items: Vec<Value> = trimmed
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| Value::String(line.to_owned()))
                .collect();
            if items.is_empty() && field.optional {
                Ok(None)
            } else {
                Ok(Some(Value::Array(items)))
            }
        }
        FieldKind::Rows(columns) => {
            let mut out = Vec::new();
            for line in trimmed.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split(',').map(str::trim).collect();
                if parts.len() != columns.len() {
                    return Err(format!(
                        "{}: each row needs {} comma-separated values ({})",
                        field.label,
                        columns.len(),
                        columns.join(",")
                    ));
                }
                let mut obj = Map::new();
                for (column, value) in columns.iter().zip(parts) {
                    obj.insert((*column).to_owned(), Value::String(value.to_owned()));
                }
                out.push(Value::Object(obj));
            }
            if out.is_empty() && field.optional {
                Ok(None)
            } else {
                Ok(Some(Value::Array(out)))
            }
        }
    }
}

/// Insert `value` at a dot-separated `path`, creating intermediate objects.
fn insert_path(root: &mut Map<String, Value>, path: &str, value: Value) {
    let mut parts = path.split('.').peekable();
    let mut current = root;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            current.insert(part.to_owned(), value);
            return;
        }
        let entry = current
            .entry(part.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        match entry.as_object_mut() {
            Some(next) => current = next,
            None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composer(action_type: &'static str, fields: Vec<ComposerField>) -> Composer {
        Composer {
            title: "t".to_owned(),
            action_type,
            submit: ComposerSubmit::Propose,
            party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            governance_type: "core_self".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            fields,
            cursor: 0,
        }
    }

    /// Build a payload or fail the test with the error message.
    fn built(action_type: &'static str, fields: Vec<ComposerField>) -> Value {
        match build_payload(&composer(action_type, fields)) {
            Ok(payload) => payload,
            Err(error) => panic!("build_payload failed: {error}"),
        }
    }

    #[test]
    fn build_payload_flat_fields_typed() {
        let fields = vec![
            text_value("member", "Member", "m::1220".to_owned()),
            int_value("new_threshold", "Threshold", 3, ""),
        ];
        let payload = built("governance_add_member", fields);
        assert_eq!(payload["type"], "governance_add_member");
        assert_eq!(payload["member"], "m::1220");
        assert_eq!(payload["new_threshold"], 3);
    }

    #[test]
    fn build_payload_nests_dot_paths_and_omits_optional() {
        let fields = vec![
            text_value("instrument_id.admin", "Admin", "adm::1220".to_owned()),
            text_value("instrument_id.id", "Id", "CBTC".to_owned()),
            text_opt("limits.max_total_deposit", "Max"),
        ];
        let payload = built("transfer", fields);
        assert_eq!(payload["instrument_id"]["admin"], "adm::1220");
        assert_eq!(payload["instrument_id"]["id"], "CBTC");
        // A declared nested-object parent is always present (required server
        // field), but its blank optional child is omitted within it.
        assert_eq!(payload["limits"], serde_json::json!({}));
        assert!(payload["limits"].get("max_total_deposit").is_none());
    }

    #[test]
    fn build_payload_required_array_is_empty_not_absent() {
        // A required Vec field with blank input must serialize as `[]`, never absent.
        let field = rows_required("new_beneficiaries", "Beneficiaries", vec!["beneficiary"]);
        let payload = built("vault_update_far_beneficiaries", vec![field]);
        assert_eq!(payload["new_beneficiaries"], serde_json::json!([]));

        let field = list_required("initial_supported_vaults", "Vaults");
        let payload = built("processor_deployment_request", vec![field]);
        assert_eq!(payload["initial_supported_vaults"], serde_json::json!([]));
    }

    #[test]
    fn build_payload_rows_become_array_of_objects() {
        let mut field = rows(
            "new_beneficiaries",
            "Beneficiaries",
            vec!["beneficiary", "weight"],
        );
        field.value = "alice::1220,0.6\nbob::1220,0.4".to_owned();
        let payload = built("vault_update_far_beneficiaries", vec![field]);
        let Some(beneficiaries) = payload["new_beneficiaries"].as_array() else {
            panic!("new_beneficiaries is not an array");
        };
        assert_eq!(beneficiaries.len(), 2);
        assert_eq!(beneficiaries[0]["beneficiary"], "alice::1220");
        assert_eq!(beneficiaries[0]["weight"], "0.6");
    }

    #[test]
    fn build_payload_rejects_missing_required() {
        let fields = vec![text("member", "Member party id")];
        match build_payload(&composer("governance_add_member", fields)) {
            Err(error) => assert!(error.contains("Member party id is required")),
            Ok(_) => panic!("expected a missing-required error"),
        }
    }

    #[test]
    fn build_payload_rows_wrong_column_count_errors() {
        let mut field = rows("claims", "Claims", vec!["subject", "property", "value"]);
        field.value = "only,two".to_owned();
        match build_payload(&composer("offer_free_credential", vec![field])) {
            Err(error) => assert!(error.contains("each row needs 3")),
            Ok(_) => panic!("expected a column-count error"),
        }
    }

    #[test]
    fn action_types_split_by_governance_type() {
        let core = action_types("core_self");
        assert_eq!(core.len(), 4);
        assert!(
            core.iter()
                .all(|option| option.key.starts_with("governance_"))
        );

        let vault = action_types("vault");
        assert!(vault.iter().any(|option| option.key == "vault_pause"));
        assert!(
            !vault
                .iter()
                .any(|option| option.key.starts_with("governance_"))
        );

        // The unimplemented paid-credential proposal is not offered.
        assert!(
            !proposal_types()
                .iter()
                .any(|option| option.key == "offer_paid_credential")
        );
    }

    #[test]
    fn fields_for_action_prefills_threshold_and_operator() {
        let ctx = ComposerContext {
            party_id: "dec::1".to_owned(),
            operator_party: "op::1".to_owned(),
            default_threshold: 5,
        };
        let fields = fields_for_action("governance_set_threshold", &ctx);
        match fields.iter().find(|field| field.key == "new_threshold") {
            Some(field) => assert_eq!(field.value, "5"),
            None => panic!("new_threshold field missing"),
        }

        let setup = fields_for_action("utility_setup", &ctx);
        match setup.iter().find(|field| field.key == "operator") {
            Some(field) => assert_eq!(field.value, "op::1"),
            None => panic!("operator field missing"),
        }
    }

    #[test]
    fn select_value_maps_to_bool_and_null() {
        let mut field = select(
            "enable_result_contracts",
            "Enable",
            vec![
                SelectOption::new("true", "Enable"),
                SelectOption::new("clear", "Clear"),
            ],
            "true",
        );
        field.value = "clear".to_owned();
        assert_eq!(field_value(&field), Ok(Some(Value::Null)));
        field.value = "true".to_owned();
        assert_eq!(field_value(&field), Ok(Some(Value::Bool(true))));
    }
}
