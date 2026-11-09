use futures::StreamExt;
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};
#[derive(Clone)]
pub struct Rpc {
    client: reqwest::Client,
    endpoint: String,
    pub retries: u32,
    pub timeout: Duration,
}
#[derive(Debug, snafu::Snafu)]
pub enum Failure {
    #[snafu(display("RPC transport failure"))]
    Transport { source: reqwest::Error },
    #[snafu(display("RPC response JSON invalid"))]
    Json { source: serde_json::Error },
    #[snafu(display("{kind}"))]
    Protocol { kind: &'static str },
}
impl Failure {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "rpc_transport",
            Self::Json { .. } => "rpc_malformed_json",
            Self::Protocol { kind } => kind,
        }
    }
}
impl Rpc {
    pub fn new(endpoint: String, retries: u32, timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint,
            retries,
            timeout,
        })
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, Failure> {
        for attempt in 0..=self.retries {
            let result = tokio::time::timeout(self.timeout, self.once(method, &params)).await;
            let (error, retry, delay) = match result {
                Ok(Ok(v)) => return Ok(v),
                Ok(Err(e)) => e,
                Err(_) => (
                    Failure::Protocol {
                        kind: "rpc_timeout",
                    },
                    true,
                    None,
                ),
            };
            if !retry || attempt == self.retries {
                return Err(error);
            }
            let jitter = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as u64
                % 100;
            let backoff =
                Duration::from_millis((200u64.saturating_mul(1 << attempt.min(8))) + jitter);
            // Refuse an excessive Retry-After rather than retrying before the server permits.
            if delay.is_some_and(|d| d > Duration::from_secs(60)) {
                return Err(Failure::Protocol {
                    kind: "rpc_retry_after_exceeds_budget",
                });
            }
            tokio::time::sleep(delay.unwrap_or(backoff).max(backoff)).await;
        }
        unreachable!()
    }
    async fn once(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<Value, (Failure, bool, Option<Duration>)> {
        let transport = |e: reqwest::Error| {
            let retry = temporary_transport(&e);
            (
                Failure::Transport {
                    source: e.without_url(),
                },
                retry,
                None,
            )
        };
        let response = self
            .client
            .post(&self.endpoint)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .map_err(transport)?;
        let status = response.status();
        if !status.is_success() {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                        httpdate::parse_http_date(v)
                            .ok()
                            .map(|d| d.duration_since(SystemTime::now()).unwrap_or_default())
                    })
                });
            return Err((
                Failure::Protocol {
                    kind: if status.as_u16() == 429 {
                        "rpc_rate_limited"
                    } else {
                        "rpc_http_error"
                    },
                },
                status.as_u16() == 429 || [500, 502, 503, 504].contains(&status.as_u16()),
                delay,
            ));
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport)?;
            if body.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err((
                    Failure::Protocol {
                        kind: "rpc_response_too_large",
                    },
                    false,
                    None,
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let v: Value = serde_json::from_slice(&body)
            .map_err(|source| (Failure::Json { source }, false, None))?;
        if v["jsonrpc"] != "2.0" || v["id"] != 1 {
            return Err((
                Failure::Protocol {
                    kind: "rpc_invalid_envelope",
                },
                false,
                None,
            ));
        }
        if let Some(e) = v.get("error") {
            let code = e["code"].as_i64();
            return Err((
                Failure::Protocol {
                    kind: if code == Some(-32015) {
                        "rpc_unsupported_version"
                    } else {
                        "rpc_error"
                    },
                },
                matches!(code, Some(-32005 | -32004)),
                None,
            ));
        }
        v.get("result").cloned().ok_or((
            Failure::Protocol {
                kind: "rpc_missing_result",
            },
            false,
            None,
        ))
    }
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
