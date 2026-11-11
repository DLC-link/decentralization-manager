//! Request helpers shared by the commands.

use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;

use super::Outcome;
use crate::api::DecmanClient;

/// `POST /network-config` takes a bare peer array. Accept the
/// `{"peers": [...]}` object that `GET /network-config` returns too.
pub fn peer_list(body: Value) -> Value {
    match body {
        Value::Object(mut object) if object.contains_key("peers") => {
            object.remove("peers").unwrap_or(Value::Null)
        }
        other => other,
    }
}

/// `value` as one URL path segment. Characters outside the unreserved set
/// and `:` are percent-encoded, so a name cannot reach another route.
///
/// # Errors
///
/// Returns an error for an empty value, `.`, or `..`.
pub fn segment(value: &str) -> Result<String> {
    if value.is_empty() || value == "." || value == ".." {
        bail!("{value:?} is not a valid name");
    }
    Ok(value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect())
}

pub fn get(client: &mut DecmanClient, path: &str, query: &[(&str, String)]) -> Result<Outcome> {
    let query: Vec<(String, String)> = query
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    Ok(Outcome::ok(client.request(
        Method::GET,
        path,
        &query,
        None,
    )?))
}

pub fn post(client: &mut DecmanClient, path: &str, body: Option<&Value>) -> Result<Outcome> {
    Ok(Outcome::ok(client.request(
        Method::POST,
        path,
        &[],
        body,
    )?))
}

/// Push optional query parameters onto `query`.
pub fn optional(query: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<String>) {
    if let Some(value) = value {
        query.push((key, value));
    }
}

/// A request body from one of the server's request types, so that a changed
/// server type breaks the CLI build.
pub fn to_body(request: &impl Serialize) -> Result<Value> {
    serde_json::to_value(request).context("failed to encode the request body")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn segment_keeps_party_ids_and_encodes_separators() {
        assert_eq!(segment("alpha::1220aa").unwrap(), "alpha::1220aa");
        assert_eq!(segment("../kick").unwrap(), "..%2Fkick");
        assert_eq!(segment("a b?c#d").unwrap(), "a%20b%3Fc%23d");
        assert!(segment("..").is_err());
        assert!(segment(".").is_err());
        assert!(segment("").is_err());
    }

    #[test]
    fn peer_list_unwraps_the_get_response() {
        assert_eq!(
            peer_list(json!({ "peers": [{ "name": "a" }] })),
            json!([{ "name": "a" }])
        );
        assert_eq!(peer_list(json!([1])), json!([1]));
    }
}
