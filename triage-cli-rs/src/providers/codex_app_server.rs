//! Codex app-server JSON-RPC client over newline-delimited stdio.
//!
//! Phase 0: transport abstraction, `initialize` gate, and unit tests with a fake
//! transport. Not wired into `get_provider()` until Phase 1.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::ProviderError;

/// JSON-RPC overload / rate-limit code from the Codex app-server.
pub const JSONRPC_OVERLOAD_CODE: i64 = -32001;

/// Newline-delimited JSON-RPC over async readers/writers.
pub trait AppServerTransport: Send {
    fn write_line<'a>(
        &'a mut self,
        line: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ProviderError>> + Send + 'a>>;

    fn read_line<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>>;
}

/// In-memory transport that replays scripted stdout lines for unit tests.
#[derive(Debug)]
pub struct FakeAppServerTransport {
    responses: VecDeque<String>,
    pub written: Vec<String>,
}

impl FakeAppServerTransport {
    pub fn new(responses: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            responses: responses.into_iter().map(Into::into).collect(),
            written: Vec::new(),
        }
    }
}

impl AppServerTransport for FakeAppServerTransport {
    fn write_line<'a>(
        &'a mut self,
        line: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            self.written.push(line.to_string());
            Ok(())
        })
    }

    fn read_line<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            self.responses
                .pop_front()
                .ok_or_else(|| ProviderError::Transport("fake transport: no scripted response".into()))
        })
    }
}

/// Spawns `codex app-server --listen stdio://` and speaks JSON-RPC on its stdio.
pub struct StdioAppServerTransport {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StdioAppServerTransport {
    pub async fn spawn() -> Result<Self, ProviderError> {
        if which::which("codex").is_err() {
            return Err(ProviderError::SubprocessMissing("codex"));
        }
        let mut child = Command::new("codex")
            .args(["app-server", "--listen", "stdio://"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ProviderError::SubprocessFailure("codex app-server", e.to_string()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::Transport("codex app-server: stdin not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::Transport("codex app-server: stdout not piped".into()))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
}

impl AppServerTransport for StdioAppServerTransport {
    fn write_line<'a>(
        &'a mut self,
        line: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            self.stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| ProviderError::Transport(format!("codex app-server write: {e}")))?;
            self.stdin
                .write_all(b"\n")
                .await
                .map_err(|e| ProviderError::Transport(format!("codex app-server write: {e}")))?;
            self.stdin
                .flush()
                .await
                .map_err(|e| ProviderError::Transport(format!("codex app-server flush: {e}")))?;
            Ok(())
        })
    }

    fn read_line<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let mut line = String::new();
            self.stdout
                .read_line(&mut line)
                .await
                .map_err(|e| ProviderError::Transport(format!("codex app-server read: {e}")))?;
            if line.is_empty() {
                return Err(ProviderError::Transport(
                    "codex app-server closed stdout".into(),
                ));
            }
            Ok(line)
        })
    }
}

/// Minimal app-server client: `initialize` must succeed before other RPCs.
pub struct CodexAppServerClient<T: AppServerTransport> {
    transport: T,
    next_id: i64,
    initialized: bool,
}

impl<T: AppServerTransport> CodexAppServerClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 1,
            initialized: false,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Negotiate capabilities with the app-server. Required before any other request.
    pub async fn initialize(&mut self) -> Result<(), ProviderError> {
        let params = json!({
            "clientInfo": {
                "name": "triage-cli",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        self.call("initialize", params).await?;
        self.initialized = true;
        Ok(())
    }

    /// Phase-0 stub for the initialize gate — real methods arrive in Phase 1.
    pub async fn stub_request(&mut self) -> Result<Value, ProviderError> {
        self.call("account/read", json!({})).await
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, ProviderError> {
        if method != "initialize" && !self.initialized {
            return Err(ProviderError::Transport(
                "codex app-server: call initialize before other requests".into(),
            ));
        }
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "method": method, "params": params });
        let line = serde_json::to_string(&request)
            .map_err(|e| ProviderError::Transport(format!("encode request: {e}")))?;
        self.transport.write_line(&line).await?;
        self.read_response_for_id(id).await
    }

    async fn read_response_for_id(&mut self, id: i64) -> Result<Value, ProviderError> {
        loop {
            let line = self.transport.read_line().await?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: Value = serde_json::from_str(trimmed).map_err(|e| {
                ProviderError::Transport(format!("codex app-server: malformed JSON line: {e}"))
            })?;

            if msg.get("method").is_some() && msg.get("id").is_none() {
                continue;
            }

            if let Some(err) = msg.get("error") {
                return Err(jsonrpc_error_to_provider(err));
            }

            if msg.get("id").and_then(Value::as_i64) == Some(id) {
                return msg
                    .get("result")
                    .cloned()
                    .ok_or_else(|| {
                        ProviderError::Transport(format!(
                            "codex app-server: response for id {id} missing result"
                        ))
                    });
            }
        }
    }
}

fn jsonrpc_error_to_provider(error: &Value) -> ProviderError {
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    ProviderError::Transport(format!(
        "codex app-server JSON-RPC error {code}: {message}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialize_ok_response(id: i64) -> String {
        serde_json::json!({
            "id": id,
            "result": {
                "codexHome": "/tmp/codex",
                "platformFamily": "unix",
                "platformOs": "linux",
                "userAgent": "test"
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn fake_initialize_sets_initialized() {
        let transport = FakeAppServerTransport::new([initialize_ok_response(1)]);
        let mut client = CodexAppServerClient::new(transport);
        assert!(!client.is_initialized());
        client.initialize().await.unwrap();
        assert!(client.is_initialized());
    }

    #[tokio::test]
    async fn fake_stub_before_initialize_fails() {
        let transport = FakeAppServerTransport::new([initialize_ok_response(1)]);
        let mut client = CodexAppServerClient::new(transport);
        let err = client.stub_request().await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(ref m) if m.contains("initialize")));
    }

    #[tokio::test]
    async fn fake_malformed_json_returns_transport_error() {
        let transport = FakeAppServerTransport::new(["not-json-at-all"]);
        let mut client = CodexAppServerClient::new(transport);
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(ref m) if m.contains("malformed JSON")));
    }

    #[tokio::test]
    async fn fake_overload_error_surfaces_transport() {
        let transport = FakeAppServerTransport::new([serde_json::json!({
            "id": 1,
            "error": { "code": JSONRPC_OVERLOAD_CODE, "message": "rate limited" }
        })
        .to_string()]);
        let mut client = CodexAppServerClient::new(transport);
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(ref m) if m.contains("-32001")));
    }

    #[tokio::test]
    async fn fake_skips_notifications_until_matching_id() {
        let transport = FakeAppServerTransport::new(vec![
            r#"{"method":"server/ready","params":{}}"#.to_string(),
            initialize_ok_response(1),
        ]);
        let mut client = CodexAppServerClient::new(transport);
        client.initialize().await.unwrap();
        assert!(client.is_initialized());
    }
}
