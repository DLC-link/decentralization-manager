//! The error answer of an HTTP handler: a status code with an
//! `ErrorResponse` body.

use actix_web::HttpResponse;

use crate::server::types::ErrorResponse;

/// A handler's error answer. Each variant is one status code, and its text
/// becomes the body's `error` field.
#[derive(Debug, PartialEq, thiserror::Error)]
pub(crate) enum ApiError {
    /// 400: the request is wrong.
    #[error("{0}")]
    BadRequest(String),
    /// 409: the request conflicts with the node's state.
    #[error("{0}")]
    Conflict(String),
    /// 500: the node failed.
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    /// The answer: the variant's status code with `{"error": <text>}`.
    pub(crate) fn into_response(self) -> HttpResponse {
        match self {
            Self::BadRequest(error) => HttpResponse::BadRequest().json(ErrorResponse { error }),
            Self::Conflict(error) => HttpResponse::Conflict().json(ErrorResponse { error }),
            Self::Internal(error) => {
                HttpResponse::InternalServerError().json(ErrorResponse { error })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::{body::to_bytes, http::StatusCode};

    use super::*;

    /// Each variant answers with its status code and a JSON body that holds
    /// only the text, exactly as the hand-built answers did.
    #[actix_web::test]
    async fn each_variant_answers_with_its_status_and_the_text() -> anyhow::Result<()> {
        let cases = [
            (
                ApiError::BadRequest("bad \"one\"".into()),
                StatusCode::BAD_REQUEST,
            ),
            (ApiError::Conflict("busy".into()), StatusCode::CONFLICT),
            (
                ApiError::Internal("broke".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            let text = error.to_string();
            let response = error.into_response();
            assert_eq!(response.status(), status);
            assert_eq!(
                response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
                Some("application/json")
            );
            let body = to_bytes(response.into_body())
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            assert_eq!(body, serde_json::to_vec(&ErrorResponse { error: text })?);
        }
        Ok(())
    }
}
