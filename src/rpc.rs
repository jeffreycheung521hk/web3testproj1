//! Bounded, read-only JSON-RPC requests with shared pacing and a fail-fast breaker.
use futures::StreamExt;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use snafu::{ResultExt, Snafu};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{sync::Mutex, time::Instant};

const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const RETRY_AFTER_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Rpc {
    client: reqwest::Client,
    endpoint: String,
    pub retries: u32,
    pub timeout: Duration,
    shared: Arc<Shared>,
}

struct Shared {
    limiter: Mutex<RateLimiter>,
    breaker: Mutex<CircuitBreaker>,
}

struct RateLimiter {
    next: Instant,
    interval: Duration,
}
impl RateLimiter {
    fn new(max_rps: u16) -> Self {
        Self {
            next: Instant::now(),
            interval: Duration::from_secs_f64(1.0 / f64::from(max_rps.max(1))),
        }
    }
    async fn acquire(&mut self) {
        // The mutex deliberately stays held while waiting: no unbounded queue of reserved slots.
        tokio::time::sleep_until(self.next).await;
        self.next = Instant::now() + self.interval;
    }
}

#[derive(Debug)]
struct CircuitBreaker {
    consecutive_failures: u16,
    threshold: u16,
    open: bool,
}
impl CircuitBreaker {
    fn new(threshold: u16) -> Self {
        Self {
            consecutive_failures: 0,
            threshold: threshold.max(1),
            open: false,
        }
    }
    fn success(&mut self) {
        // Once opened, a late successful in-flight call must not silently close the breaker.
        if !self.open {
            self.consecutive_failures = 0;
        }
    }
    fn failed_call(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.open |= self.consecutive_failures >= self.threshold;
    }
    fn ensure_closed(&self) -> Result<(), Failure> {
        if self.open {
            Err(Failure::CircuitOpen)
        } else {
            Ok(())
        }
    }
}

/// Infrastructure failures are errors, never persisted payment conclusions.
/// Only `UnsupportedVersion` is a provider response about an unsupported transaction format.
#[derive(Debug, Snafu)]
pub enum Failure {
    #[snafu(display("RPC transport failure"))]
    Transport { source: reqwest::Error },
    #[snafu(display("RPC response JSON invalid"))]
    Json { source: serde_json::Error },
    #[snafu(display("RPC request timed out"))]
    Timeout { source: tokio::time::error::Elapsed },
    #[snafu(display("RPC returned HTTP status {status}"))]
    Http {
        status: u16,
        retry_after: Option<Duration>,
    },
    #[snafu(display("RPC returned error code {code}"))]
    Remote { code: i64 },
    #[snafu(display("RPC transaction version unsupported"))]
    UnsupportedVersion,
    #[snafu(display("RPC response envelope invalid"))]
    InvalidEnvelope,
    #[snafu(display("RPC response result missing"))]
    MissingResult,
    #[snafu(display("RPC response exceeds 2 MiB"))]
    ResponseTooLarge,
    #[snafu(display("RPC Retry-After exceeds the 60-second waiting budget"))]
    RetryAfterBudget,
    #[snafu(display("RPC circuit is open; resume to start a new session"))]
    CircuitOpen,
}
impl Failure {
    /// Stable, credential-free diagnostic code. Provider message text is never retained.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "rpc_transport",
            Self::Json { .. } => "rpc_malformed_json",
            Self::Timeout { .. } => "rpc_timeout",
            Self::Http { status: 429, .. } => "rpc_rate_limited",
            Self::Http { .. } => "rpc_http_error",
            Self::Remote { .. } => "rpc_error",
            Self::UnsupportedVersion => "rpc_unsupported_version",
            Self::InvalidEnvelope => "rpc_invalid_envelope",
            Self::MissingResult => "rpc_missing_result",
            Self::ResponseTooLarge => "rpc_response_too_large",
            Self::RetryAfterBudget => "rpc_retry_after_exceeds_budget",
            Self::CircuitOpen => "rpc_circuit_open",
        }
    }
    pub fn reason(&self) -> &'static str {
        self.code()
    }
    pub fn is_unsupported_version(&self) -> bool {
        matches!(self, Self::UnsupportedVersion)
    }
    pub fn is_infrastructure(&self) -> bool {
        !self.is_unsupported_version()
    }
    fn retryable(&self) -> bool {
        match self {
            Self::Transport { source } => temporary_transport(source),
            Self::Timeout { .. } => true,
            Self::Http { status, .. } => matches!(status, 429 | 500 | 502 | 503 | 504),
            Self::Remote { code } => matches!(code, -32005 | -32004),
            _ => false,
        }
    }
    fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: &'a Value,
}

#[derive(Default)]
enum Presence<T> {
    #[default]
    Missing,
    Present(T),
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Presence<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}
#[derive(Deserialize)]
struct Envelope {
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Presence<Value>,
    error: Option<RemoteError>,
}
#[derive(Deserialize)]
struct RemoteError {
    code: i64,
}

fn decode_response(body: &[u8]) -> Result<Value, Failure> {
    let envelope: Envelope = serde_json::from_slice(body).context(JsonSnafu)?;
    if envelope.jsonrpc != "2.0" || envelope.id != 1 {
        return Err(Failure::InvalidEnvelope);
    }
    match (envelope.result, envelope.error) {
        (Presence::Present(result), None) => Ok(result),
        (Presence::Missing, Some(RemoteError { code: -32015 })) => Err(Failure::UnsupportedVersion),
        (Presence::Missing, Some(RemoteError { code })) => Err(Failure::Remote { code }),
        (Presence::Missing, None) => Err(Failure::MissingResult),
        (Presence::Present(_), Some(_)) => Err(Failure::InvalidEnvelope),
    }
}

impl Rpc {
    pub fn new(endpoint: String, retries: u32, timeout: Duration) -> Result<Self, reqwest::Error> {
        Self::with_limits(endpoint, retries, timeout, 20, 3)
    }
    /// Zero values are conservatively clamped to one; the CLI rejects them before construction.
    pub fn with_limits(
        endpoint: String,
        retries: u32,
        timeout: Duration,
        max_rps: u16,
        threshold: u16,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint,
            retries,
            timeout,
            shared: Arc::new(Shared {
                limiter: Mutex::new(RateLimiter::new(max_rps)),
                breaker: Mutex::new(CircuitBreaker::new(threshold)),
            }),
        })
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, Failure> {
        for attempt in 0..=self.retries {
            self.shared.breaker.lock().await.ensure_closed()?;
            self.shared.limiter.lock().await.acquire().await;
            self.shared.breaker.lock().await.ensure_closed()?;
            let error = match tokio::time::timeout(self.timeout, self.once(method, &params)).await {
                Ok(Ok(value)) => {
                    self.shared.breaker.lock().await.success();
                    return Ok(value);
                }
                Ok(Err(error)) => error,
                Err(source) => Failure::Timeout { source },
            };
            if !error.is_infrastructure() {
                self.shared.breaker.lock().await.success();
                return Err(error);
            }
            if !error.retryable() || attempt == self.retries {
                if error.retryable() {
                    self.shared.breaker.lock().await.failed_call();
                }
                return Err(error);
            }
            if error
                .retry_after()
                .is_some_and(|delay| delay > RETRY_AFTER_BUDGET)
            {
                self.shared.breaker.lock().await.failed_call();
                return Err(Failure::RetryAfterBudget);
            }
            let jitter = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as u64
                % 100;
            let backoff =
                Duration::from_millis(200u64.saturating_mul(1 << attempt.min(8)) + jitter);
            tokio::time::sleep(error.retry_after().unwrap_or(backoff).max(backoff)).await;
        }
        unreachable!("the inclusive retry loop always returns on its final attempt")
    }
    async fn once(&self, method: &str, params: &Value) -> Result<Value, Failure> {
        let request = Request {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let response = self
            .client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .context(TransportSnafu)?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|value| parse_retry_after(value, SystemTime::now()));
            return Err(Failure::Http {
                status,
                retry_after,
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > RESPONSE_LIMIT as u64)
        {
            return Err(Failure::ResponseTooLarge);
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(reqwest::Error::without_url)
                .context(TransportSnafu)?;
            if body.len() + chunk.len() > RESPONSE_LIMIT {
                return Err(Failure::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        decode_response(&body)
    }
}

fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(now).unwrap_or_default())
        })
}

// Inspect typed causes rather than retrying every request/connect error (e.g. TLS certificates).
fn temporary_transport(error: &reqwest::Error) -> bool {
    use std::error::Error;
    use std::io::ErrorKind;
    if error.is_timeout() {
        return true;
    }
    let mut source = error.source();
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::ConnectionRefused
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::TimedOut
                    | ErrorKind::Interrupted
                    | ErrorKind::WouldBlock
            )
        {
            return true;
        }
        if let Some(http) = cause.downcast_ref::<hyper::Error>()
            && (http.is_incomplete_message() || http.is_closed() || http.is_timeout())
        {
            return true;
        }
        source = cause.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn null_result_is_not_missing_and_error_messages_are_not_retained() {
        assert_eq!(
            decode_response(br#"{"jsonrpc":"2.0","id":1,"result":null}"#)
                .expect("valid null result"),
            Value::Null
        );
        assert!(matches!(
            decode_response(br#"{"jsonrpc":"2.0","id":1}"#),
            Err(Failure::MissingResult)
        ));
        let failure = decode_response(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"credential-DO-NOT-LEAK"}}"#).expect_err("remote error");
        assert_eq!(failure.code(), "rpc_error");
        assert!(!format!("{failure:?} {failure}").contains("DO-NOT-LEAK"));
    }

    #[test]
    fn unsupported_version_is_distinct_from_infrastructure() {
        let failure = decode_response(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32015}}"#)
            .expect_err("version error");
        assert!(failure.is_unsupported_version());
        assert!(!failure.is_infrastructure());
        assert!(!failure.retryable());
    }

    #[test]
    fn envelope_rejects_wrong_identity_and_ambiguous_outcome() {
        for value in [
            json!({"jsonrpc":"2.0","id":2,"result":null}),
            json!({"jsonrpc":"1.0","id":1,"result":null}),
            json!({"jsonrpc":"2.0","id":1,"result":null,"error":{"code":-32005}}),
        ] {
            assert!(matches!(
                decode_response(&serde_json::to_vec(&value).expect("serialize")),
                Err(Failure::InvalidEnvelope)
            ));
        }
    }

    #[test]
    fn retryable_http_statuses_exclude_permanent_errors() {
        for status in [429, 500, 502, 503, 504] {
            assert!(
                Failure::Http {
                    status,
                    retry_after: None
                }
                .retryable()
            );
        }
        for status in [400, 401, 403, 404, 501] {
            assert!(
                !Failure::Http {
                    status,
                    retry_after: None
                }
                .retryable()
            );
        }
        assert!(!Failure::ResponseTooLarge.retryable());
        assert!(!Failure::InvalidEnvelope.retryable());
    }

    #[test]
    fn retry_after_supports_seconds_dates_and_past_dates() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(parse_retry_after("7", now), Some(Duration::from_secs(7)));
        assert_eq!(
            parse_retry_after(&httpdate::fmt_http_date(now + Duration::from_secs(5)), now),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            parse_retry_after(&httpdate::fmt_http_date(now - Duration::from_secs(5)), now),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after("invalid", now), None);
    }

    #[test]
    fn breaker_resets_successes_and_stays_open_after_late_success() {
        let mut breaker = CircuitBreaker::new(2);
        breaker.failed_call();
        breaker.success();
        breaker.failed_call();
        assert!(breaker.ensure_closed().is_ok());
        breaker.failed_call();
        assert!(matches!(breaker.ensure_closed(), Err(Failure::CircuitOpen)));
        breaker.success();
        assert!(matches!(breaker.ensure_closed(), Err(Failure::CircuitOpen)));
    }

    #[tokio::test]
    async fn limiter_and_breaker_are_shared_across_client_clones() {
        let client = Rpc::with_limits(
            "http://127.0.0.1:1".into(),
            0,
            Duration::from_secs(1),
            100,
            2,
        )
        .expect("client");
        let clone = client.clone();
        let start = Instant::now();
        client.shared.limiter.lock().await.acquire().await;
        clone.shared.limiter.lock().await.acquire().await;
        client.shared.limiter.lock().await.acquire().await;
        assert!(start.elapsed() >= Duration::from_millis(20));
        client.shared.breaker.lock().await.failed_call();
        clone.shared.breaker.lock().await.failed_call();
        let before = Instant::now();
        assert!(matches!(
            client.call("unused", json!([])).await,
            Err(Failure::CircuitOpen)
        ));
        assert!(before.elapsed() < Duration::from_millis(100));
    }
}
