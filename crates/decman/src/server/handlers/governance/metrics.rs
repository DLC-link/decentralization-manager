//! Governance action metrics.

use std::sync::LazyLock;

use prometheus::IntCounterVec;

use crate::error::Result;

static ACTIONS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    prometheus::register_int_counter_vec!(
        "decman_governance_actions_total",
        "Governance action attempts by step and outcome. error covers failures before and during ledger submission, not only ledger rejections. A request refused up front, such as by validation, does not count.",
        &["step", "outcome"]
    )
    .expect("metric name is a unique literal")
});

/// A governance write this node attempts for its member party.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActionStep {
    Propose,
    Confirm,
    Execute,
    Expire,
    CancelConfirmation,
    CancelProposal,
}

impl ActionStep {
    const ALL: [Self; 6] = [
        Self::Propose,
        Self::Confirm,
        Self::Execute,
        Self::Expire,
        Self::CancelConfirmation,
        Self::CancelProposal,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Propose => "propose",
            Self::Confirm => "confirm",
            Self::Execute => "execute",
            Self::Expire => "expire",
            Self::CancelConfirmation => "cancel_confirmation",
            Self::CancelProposal => "cancel_proposal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActionOutcome {
    Ok,
    Error,
}

impl ActionOutcome {
    const ALL: [Self; 2] = [Self::Ok, Self::Error];

    pub(super) fn of<T>(result: &Result<T>) -> Self {
        if result.is_ok() {
            Self::Ok
        } else {
            Self::Error
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

/// Exports every step and outcome at zero, so a rate query reads 0 rather than
/// nothing before the first attempt.
pub(crate) fn register_metrics() {
    for step in ActionStep::ALL {
        for outcome in ActionOutcome::ALL {
            ACTIONS
                .with_label_values(&[step.label(), outcome.label()])
                .inc_by(0);
        }
    }
}

/// Counts one attempt. An attempt starts at the `execute_*` call for a choice
/// step, and at the Ledger API connect for propose. Its `error` can come before
/// the submission itself: a failed connect, an unconfigured package, or a blob
/// that does not decode. A request the handler refuses before that point is a
/// caller error and does not count.
pub(super) fn count_action(step: ActionStep, outcome: ActionOutcome) {
    ACTIONS
        .with_label_values(&[step.label(), outcome.label()])
        .inc();
}

#[cfg(test)]
mod action_metrics_tests {
    use std::sync::Arc;

    use actix_web::{
        App,
        http::{StatusCode, header::AUTHORIZATION},
        test as actix_test, web,
    };
    use anyhow::Context;

    use super::{
        super::{
            actions::{
                cancel_confirmation, cancel_proposal, confirm_action, execute_action,
                expire_confirmation,
            },
            propose::propose_action,
        },
        *,
    };
    use crate::{
        auth::{MockAuthRegistry, WorkflowAuth},
        canton_id::CantonId,
        server::{AppState, middleware::AuthMiddleware},
    };

    fn party_id() -> Result<CantonId> {
        CantonId::parse(&format!("p::{}", "0".repeat(68)))
    }

    fn count(step: ActionStep, outcome: ActionOutcome) -> u64 {
        ACTIONS
            .with_label_values(&[step.label(), outcome.label()])
            .get()
    }

    /// Mock credentials and a Ledger API on port 1, where nothing listens, so
    /// every submission reaches the ledger path and fails there.
    async fn state_with_ledger_down() -> Result<web::Data<AppState>> {
        let mut state = AppState::for_test(None).await?.into_inner();
        Arc::get_mut(&mut state)
            .context("a fresh test state has one owner")?
            .config
            .canton
            .ledger_api_port = 1;
        let mock = MockAuthRegistry::new(state.party_credentials.clone());
        *state.auth.write().await = Some(WorkflowAuth::Mock(Arc::new(mock)));
        Ok(web::Data::from(state))
    }

    #[test]
    fn labels_are_the_strings_dashboards_query() {
        assert_eq!(
            ActionStep::ALL.map(ActionStep::label),
            [
                "propose",
                "confirm",
                "execute",
                "expire",
                "cancel_confirmation",
                "cancel_proposal"
            ]
        );
        assert_eq!(
            ActionOutcome::ALL.map(ActionOutcome::label),
            ["ok", "error"]
        );
    }

    #[test]
    fn every_step_and_outcome_is_exported_before_its_first_submission() -> Result<()> {
        register_metrics();
        let family = prometheus::gather()
            .into_iter()
            .find(|f| f.name() == "decman_governance_actions_total")
            .context("the family is not registered")?;
        let series: Vec<Vec<&str>> = family
            .get_metric()
            .iter()
            .map(|m| m.get_label().iter().map(|l| l.value()).collect())
            .collect();
        for step in ActionStep::ALL {
            for outcome in ActionOutcome::ALL {
                assert!(
                    series.iter().any(|labels| labels.contains(&step.label())
                        && labels.contains(&outcome.label())),
                    "no series for {step:?}/{outcome:?}"
                );
            }
        }
        Ok(())
    }

    // The handler tests below read and move the same series, so they take
    // turns: every test in this binary shares one registry and runs in parallel.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Posts `body` as an admin through all six governance write handlers.
    async fn post(uri: &str, body: serde_json::Value) -> Result<StatusCode> {
        let data = state_with_ledger_down().await?;
        // The middleware attaches the principal that `require_admin` checks.
        let app = actix_test::init_service(
            App::new()
                .app_data(data)
                .wrap(AuthMiddleware)
                .service(propose_action)
                .service(confirm_action)
                .service(execute_action)
                .service(expire_confirmation)
                .service(cancel_confirmation)
                .service(cancel_proposal),
        )
        .await;
        let request = actix_test::TestRequest::post()
            .uri(uri)
            .insert_header((AUTHORIZATION, "Bearer any-token"))
            .set_json(body)
            .to_request();
        Ok(actix_test::call_service(&app, request).await.status())
    }

    /// How far a step's `error` and `ok` series moved while `uri` was posted.
    async fn moved(step: ActionStep, uri: &str, body: serde_json::Value) -> Result<(u64, u64)> {
        let error_before = count(step, ActionOutcome::Error);
        let ok_before = count(step, ActionOutcome::Ok);
        let status = post(uri, body).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{step:?}");
        Ok((
            count(step, ActionOutcome::Error) - error_before,
            count(step, ActionOutcome::Ok) - ok_before,
        ))
    }

    #[actix_web::test]
    async fn every_step_counts_an_attempt_the_ledger_never_answered_as_one_error() -> Result<()> {
        let _serial = SERIAL.lock().await;
        let party = party_id()?.to_string();
        let action = serde_json::json!({ "type": "governance_set_threshold", "new_threshold": 2 });
        let cases = [
            (
                ActionStep::Propose,
                "/governance/propose",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "proposal": { "type": "generic_vote", "description": "metrics" },
                }),
            ),
            (
                ActionStep::Confirm,
                "/governance/confirm",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": action,
                }),
            ),
            (
                ActionStep::Execute,
                "/governance/execute",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": action,
                    "confirmation_cids": ["00c0ffee"],
                }),
            ),
            (
                ActionStep::Expire,
                "/governance/expire",
                serde_json::json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "confirmation_cid": "00c0ffee",
                }),
            ),
            (
                ActionStep::CancelConfirmation,
                "/governance/cancel",
                serde_json::json!({ "party_id": party, "confirmation_cid": "00c0ffee" }),
            ),
            (
                ActionStep::CancelProposal,
                "/governance/cancel-proposal",
                serde_json::json!({ "party_id": party, "proposal_cid": "00p" }),
            ),
        ];
        for (step, uri, body) in cases {
            assert_eq!(moved(step, uri, body).await?, (1, 0), "{step:?}");
        }
        Ok(())
    }

    #[actix_web::test]
    async fn a_request_refused_before_submission_does_not_count() -> Result<()> {
        let _serial = SERIAL.lock().await;
        let step = ActionStep::Confirm;
        let error_before = count(step, ActionOutcome::Error);
        let ok_before = count(step, ActionOutcome::Ok);

        // A threshold of 0 fails the inline validation, which runs first.
        let status = post(
            "/governance/confirm",
            serde_json::json!({
                "party_id": party_id()?.to_string(),
                "rules_contract_id": "00rules",
                "action": { "type": "governance_set_threshold", "new_threshold": 0 },
            }),
        )
        .await?;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(count(step, ActionOutcome::Error), error_before);
        assert_eq!(count(step, ActionOutcome::Ok), ok_before);
        Ok(())
    }
}
