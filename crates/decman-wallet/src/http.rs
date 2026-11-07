//! JSON over HTTP for the Ledger API and registry clients.
//!
//! Errors carry the base URL, the method and the path, like the tenant
//! client's, so a wallet talking to several services can tell them apart.

use std::{
    net::{Ipv4Addr, Ipv6Addr},
    time::Duration,
};

use reqwest::{Client, RequestBuilder, Response, Url, redirect::Policy};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::error::{Error, Result};

/// How a Ledger API or registry client talks to its host.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// How long one call may take, from the request to the last byte of the
    /// answer. A prepare or an execute waits for Canton to interpret or
    /// commit a transaction. The default is 60 seconds, like the tenant
    /// client's.
    pub timeout: Duration,
    /// Whether the client may use plain `http` to a host that is not
    /// loopback. Off by default, because the bearer token and the prepared
    /// transaction would cross the network unencrypted.
    pub allow_plain_http: bool,
    /// The largest response body the client reads, in bytes. The default is
    /// 64 MiB.
    pub max_response_bytes: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            allow_plain_http: false,
            max_response_bytes: 64 * 1024 * 1024,
        }
    }
}

/// One base URL and the optional bearer token sent with every call to it.
#[derive(Clone)]
pub(crate) struct JsonHost {
    http: Client,
    base_url: String,
    token: Option<String>,
    max_response_bytes: usize,
}

impl JsonHost {
    /// # Errors
    /// [`Error::InvalidUrl`] for a base URL that is not `http` or `https`,
    /// [`Error::InsecureUrl`] for plain `http` that `options` does not allow,
    /// and [`Error::Client`] if the HTTP client cannot be built.
    pub(crate) fn with_options(
        base_url: impl Into<String>,
        token: Option<String>,
        options: ClientOptions,
    ) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        check_url(&base_url, options.allow_plain_http)?;
        // A followed redirect would resend the request, body and all, to a
        // URL that `check_url` never saw.
        let http = Client::builder()
            .timeout(options.timeout)
            .redirect(Policy::none())
            .build()
            .map_err(|source| Error::Client {
                host: base_url.clone(),
                source,
            })?;
        Ok(Self {
            http,
            base_url,
            token,
            max_response_bytes: options.max_response_bytes,
        })
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let request = self.http.get(format!("{base}{path}", base = self.base_url));
        let response = self.send("GET", path, request).await?;
        self.read_json("GET", path, response).await
    }

    pub(crate) async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let request = self
            .http
            .post(format!("{base}{path}", base = self.base_url))
            .json(body);
        let response = self.send("POST", path, request).await?;
        self.read_json("POST", path, response).await
    }

    /// POST to an endpoint that streams its answer. The body is a JSON array,
    /// newline-delimited JSON objects, or empty when there is nothing to send.
    pub(crate) async fn post_stream<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<Vec<T>> {
        let request = self
            .http
            .post(format!("{base}{path}", base = self.base_url))
            .json(body);
        let response = self.send("POST", path, request).await?;
        let response = self.check_status("POST", path, response).await?;
        let body = self.read_body("POST", path, response).await?;
        parse_stream(&String::from_utf8_lossy(&body)).map_err(|source| Error::Json {
            host: self.base_url.clone(),
            method: "POST",
            path: path.to_string(),
            source,
        })
    }

    async fn send(
        &self,
        method: &'static str,
        path: &str,
        request: RequestBuilder,
    ) -> Result<Response> {
        let request = match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        request.send().await.map_err(|source| Error::Transport {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            source,
        })
    }

    async fn read_json<T: DeserializeOwned>(
        &self,
        method: &'static str,
        path: &str,
        response: Response,
    ) -> Result<T> {
        let response = self.check_status(method, path, response).await?;
        let body = self.read_body(method, path, response).await?;
        serde_json::from_slice(&body).map_err(|source| Error::Json {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            source,
        })
    }

    /// The whole body, read in chunks. A body larger than
    /// `max_response_bytes` is refused before it is read in full.
    async fn read_body(
        &self,
        method: &'static str,
        path: &str,
        mut response: Response,
    ) -> Result<Vec<u8>> {
        let limit = self.max_response_bytes;
        let too_large = || Error::ResponseTooLarge {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            limit,
        };
        let declared_too_large = response
            .content_length()
            .is_some_and(|length| u64::try_from(limit).is_ok_and(|limit| length > limit));
        if declared_too_large {
            return Err(too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|source| Error::Decode {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            source,
        })? {
            if body.len().saturating_add(chunk.len()) > limit {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// Turn a non-2xx response into [`Error::Api`] with the server's message.
    async fn check_status(
        &self,
        method: &'static str,
        path: &str,
        response: Response,
    ) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = match self.read_body(method, path, response).await {
            Ok(body) => String::from_utf8_lossy(&body).into_owned(),
            Err(error @ Error::ResponseTooLarge { .. }) => return Err(error),
            Err(_) => String::new(),
        };
        Err(Error::Api {
            host: self.base_url.clone(),
            method,
            path: path.to_string(),
            status: status.as_u16(),
            message: error_message(body),
        })
    }
}

/// Refuse a base URL that is not `http` or `https`. Refuse plain `http` to
/// a host that is not loopback, unless `allow_plain_http` is set.
fn check_url(base_url: &str, allow_plain_http: bool) -> Result<()> {
    let invalid = |reason: String| Error::InvalidUrl {
        host: base_url.to_string(),
        reason,
    };
    let url = Url::parse(base_url).map_err(|e| invalid(e.to_string()))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_plain_http || url.host_str().is_some_and(is_loopback) => Ok(()),
        "http" => Err(Error::InsecureUrl {
            host: base_url.to_string(),
        }),
        other => Err(invalid(format!("its scheme is {other}, not http or https"))),
    }
}

/// `localhost`, an address in `127.0.0.0/8`, or `::1`. The URL writes an
/// IPv6 host in brackets.
fn is_loopback(host: &str) -> bool {
    let ipv6 = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .and_then(|host| host.parse::<Ipv6Addr>().ok());
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
        || ipv6.is_some_and(|ip| ip.is_loopback())
}

/// Read a streamed answer: a JSON array, one JSON object per line, or nothing.
fn parse_stream<T: DeserializeOwned>(text: &str) -> serde_json::Result<Vec<T>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.starts_with('[') {
        return serde_json::from_str(text);
    }
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// The most characters of a host's error message that the client keeps.
const MAX_ERROR_MESSAGE_CHARS: usize = 1024;

/// The readable part of an error body, made safe to print with [`bounded`].
fn error_message(body: String) -> String {
    bounded(&readable_part(body))
}

/// `message` with each control character replaced by a space, and cut at
/// [`MAX_ERROR_MESSAGE_CHARS`] characters.
///
/// The host chooses the body, and the message reaches logs and terminals. A
/// newline could forge a log line, and an escape sequence could drive a
/// terminal. A body of megabytes would flood both.
fn bounded(message: &str) -> String {
    let mut kept: String = message
        .chars()
        .take(MAX_ERROR_MESSAGE_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let more = message
        .chars()
        .count()
        .saturating_sub(MAX_ERROR_MESSAGE_CHARS);
    if more > 0 {
        kept.push_str(&format!(" ... ({more} more characters)"));
    }
    kept
}

/// The readable part of an error body: a registry's `{"error": ...}`, or a
/// Canton error's code and cause. Anything else is returned as it came.
fn readable_part(body: String) -> String {
    let Ok(value) = serde_json::from_str::<Value>(&body) else {
        return body;
    };
    if let Some(message) = value.get("error").and_then(Value::as_str) {
        return message.to_string();
    }
    let code = value.get("code").and_then(Value::as_str);
    match (code, value.get("cause").and_then(Value::as_str)) {
        (Some(code), Some(cause)) => format!("{code}: {cause}"),
        (None, Some(cause)) => cause.to_string(),
        _ => body,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::*;

    fn host_for(url: &str, options: ClientOptions) -> Result<JsonHost> {
        JsonHost::with_options(url, None, options)
    }

    fn allowing_plain_http() -> ClientOptions {
        ClientOptions {
            allow_plain_http: true,
            ..ClientOptions::default()
        }
    }

    #[test]
    fn https_and_plain_http_to_loopback_are_accepted() {
        for url in [
            "https://scan.example.com",
            "https://scan.example.com/api/scan/",
            "http://localhost:7575/api/json-api",
            "http://127.0.0.1:7575",
            "http://127.8.9.10",
            "http://[::1]:7575",
        ] {
            assert!(
                host_for(url, ClientOptions::default()).is_ok(),
                "{url} must be accepted"
            );
        }
    }

    #[test]
    fn plain_http_to_another_host_is_refused_unless_the_options_allow_it() {
        for url in [
            "http://example.com",
            "http://10.0.0.1:7575/api/json-api",
            "http://[::2]:7575",
            "http://localhost.example.com",
        ] {
            match host_for(url, ClientOptions::default()) {
                Err(Error::InsecureUrl { host }) => assert_eq!(host, url),
                Err(other) => panic!("{url}: expected InsecureUrl, got {other}"),
                Ok(_) => panic!("{url} must be refused"),
            }
            assert!(
                host_for(url, allowing_plain_http()).is_ok(),
                "{url} must be accepted when the options allow plain http"
            );
        }
    }

    #[test]
    fn a_url_that_is_not_http_or_https_is_refused() {
        for url in [
            "ftp://example.com",
            "file:///etc/passwd",
            "localhost:7575",
            "not a url",
            "http://:7575",
        ] {
            match host_for(url, allowing_plain_http()) {
                Err(Error::InvalidUrl { host, .. }) => assert_eq!(host, url),
                Err(other) => panic!("{url}: expected InvalidUrl, got {other}"),
                Ok(_) => panic!("{url} must be refused"),
            }
        }
    }

    fn assert_too_large<T: std::fmt::Debug>(result: Result<T>, wanted_path: &str) {
        match result {
            Err(Error::ResponseTooLarge { path, limit, .. }) => {
                assert_eq!(path, wanted_path);
                assert_eq!(limit, 1024);
            }
            other => panic!("{wanted_path}: expected ResponseTooLarge, got {other:?}"),
        }
    }

    /// Each way the client reads a body stops at the cap: a JSON answer, a
    /// streamed answer, and the body of an error.
    #[tokio::test]
    async fn a_body_over_the_cap_is_refused() {
        let server = MockServer::start().await;
        let padding = "x".repeat(2048);
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "padding": padding })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/stream"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "padding": padding }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/error"))
            .respond_with(ResponseTemplate::new(500).set_body_string(padding.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/small"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;
        let host = match host_for(
            &server.uri(),
            ClientOptions {
                max_response_bytes: 1024,
                ..ClientOptions::default()
            },
        ) {
            Ok(host) => host,
            Err(e) => panic!("the client must build: {e}"),
        };

        assert_too_large(host.get::<Value>("/big").await, "/big");
        assert_too_large(
            host.post_stream::<_, Value>("/stream", &json!({})).await,
            "/stream",
        );
        assert_too_large(host.get::<Value>("/error").await, "/error");
        match host.get::<Value>("/small").await {
            Ok(body) => assert_eq!(body, json!({ "ok": true })),
            Err(e) => panic!("a body under the cap must read: {e}"),
        }
    }

    /// A server that answers once with a chunked body of `chunks` times 256
    /// bytes, without a Content-Length.
    fn chunked_server(chunks: usize) -> String {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(e) => panic!("the test server must bind: {e}"),
        };
        let url = match listener.local_addr() {
            Ok(address) => format!("http://{address}"),
            Err(e) => panic!("the test server must have an address: {e}"),
        };
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let mut response =
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n"
                    .to_vec();
            for _ in 0..chunks {
                response.extend_from_slice(format!("100\r\n{}\r\n", "x".repeat(256)).as_bytes());
            }
            response.extend_from_slice(b"0\r\n\r\n");
            let _ = stream.write_all(&response);
        });
        url
    }

    /// A body without a Content-Length is counted as it arrives.
    #[tokio::test]
    async fn a_chunked_body_over_the_cap_is_refused_as_it_arrives() {
        let host = match host_for(
            &chunked_server(8),
            ClientOptions {
                max_response_bytes: 1024,
                ..ClientOptions::default()
            },
        ) {
            Ok(host) => host,
            Err(e) => panic!("the client must build: {e}"),
        };

        assert_too_large(host.get::<Value>("/chunked").await, "/chunked");
    }

    #[test]
    fn error_message_prefers_the_registry_error_then_the_canton_cause() {
        assert_eq!(
            error_message(r#"{"error": "No holdings provided"}"#.to_string()),
            "No holdings provided"
        );
        assert_eq!(
            error_message(r#"{"code": "NOT_FOUND", "cause": "no such contract"}"#.to_string()),
            "NOT_FOUND: no such contract"
        );
        assert_eq!(error_message("plain text".to_string()), "plain text");
    }

    /// A host could send a body of any size, with newlines that forge log
    /// lines and escape sequences that drive a terminal.
    #[test]
    fn error_message_replaces_control_characters_and_cuts_a_long_body() {
        let body: String = "refused\n\u{1b}[2J\u{1b}[31mred\r\n"
            .chars()
            .cycle()
            .take(10 * 1024)
            .collect();

        let message = error_message(body);

        let suffix = " ... (9216 more characters)";
        assert!(message.ends_with(suffix), "{message}");
        assert_eq!(message.chars().count(), 1024 + suffix.len());
        assert!(
            message.starts_with("refused  [2J [31mred  refused "),
            "{message}"
        );
        assert!(!message.chars().any(char::is_control), "{message}");
    }

    /// The cut counts characters, so a multi-byte character is never split,
    /// and a message from a JSON body is cut too.
    #[test]
    fn error_message_cuts_a_long_registry_error_at_a_character_boundary() {
        let body = json!({ "error": "é".repeat(3000) }).to_string();

        let message = error_message(body);

        assert_eq!(
            message,
            format!("{} ... (1976 more characters)", "é".repeat(1024))
        );
    }

    #[test]
    fn error_message_keeps_a_message_of_exactly_the_cap() {
        let body = "x".repeat(MAX_ERROR_MESSAGE_CHARS);

        assert_eq!(error_message(body.clone()), body);
    }
}
