//! Codex app-server JSON-RPC client over newline-delimited stdio.
//!
//! Phase 1: production client, singleton process, overload retry, thread/turn
//! RPCs for `LlmProvider::followup`. `complete()` stays on subprocess (Phase 3).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use super::codex::CodexSubprocessProvider;
use super::{
    CompletionResult, FollowupResult, LlmProvider, ProviderError, ProviderProgress,
};

/// JSON-RPC overload / rate-limit code from the Codex app-server.
pub const JSONRPC_OVERLOAD_CODE: i64 = -32001;

/// Default max NDJSON lines read while waiting for one JSON-RPC response.
pub const DEFAULT_MAX_READ_LINES: usize = 10_000;

/// Default wall-clock budget while waiting for one JSON-RPC response.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Wall-clock budget while waiting for device-code login to complete.
pub const LOGIN_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// Hint printed when doctor/setup Codex checks fail.
pub const SETUP_HINT: &str = "run `triage-cli setup`";

const OVERLOAD_MAX_ATTEMPTS: u32 = 3;
const OVERLOAD_TOTAL_CAP: Duration = Duration::from_secs(8);

static APP_SERVER_FALLBACK_HINT: AtomicBool = AtomicBool::new(false);

static SHARED_CLIENT: Lazy<AsyncMutex<Option<CodexAppServerClient<StdioAppServerTransport>>>> =
    Lazy::new(|| AsyncMutex::new(None));

/// Optional progress sink for inbox wiring (Phase 4). Internal-only.
type ProgressReporter = Arc<dyn Fn(ProviderProgress) + Send + Sync>;

static PROGRESS_REPORTER: Lazy<AsyncMutex<Option<ProgressReporter>>> =
    Lazy::new(|| AsyncMutex::new(None));

/// Register a process-wide progress callback (typically from the inbox pipeline).
pub async fn set_progress_reporter(reporter: Option<ProgressReporter>) {
    *PROGRESS_REPORTER.lock().await = reporter;
}

fn emit_progress(progress: ProviderProgress) {
    if let Ok(guard) = PROGRESS_REPORTER.try_lock() {
        if let Some(cb) = guard.as_ref() {
            cb(progress);
        }
    }
}

/// Log once when `get_provider()` falls back from app-server to subprocess.
pub(crate) fn log_app_server_fallback_once() {
    if !APP_SERVER_FALLBACK_HINT.swap(true, Ordering::Relaxed) {
        eprintln!(
            "hint: codex app-server unavailable; using `codex exec` for Codex. \
             Set CODEX_TRANSPORT=exec to silence this message."
        );
    }
}

/// Returns true when the `codex` binary is on `PATH`.
pub fn codex_binary_on_path() -> bool {
    which::which("codex").is_ok()
}

/// Returns true when the installed Codex CLI exposes the `app-server` subcommand.
pub fn codex_supports_app_server_sync() -> bool {
    if !codex_binary_on_path() {
        return false;
    }
    std::process::Command::new("codex")
        .args(["app-server", "--help"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Probe whether app-server is usable (`codex` on PATH + subcommand + `initialize`).
pub async fn probe_app_server() -> bool {
    if !codex_supports_app_server_sync() {
        return false;
    }
    match StdioAppServerTransport::spawn().await {
        Ok(transport) => {
            let mut client = CodexAppServerClient::new(transport);
            client.initialize().await.is_ok()
        }
        Err(_) => false,
    }
}

/// Sync probe for `get_provider()` — uses the current Tokio runtime or a temporary one.
pub fn probe_app_server_sync() -> bool {
    if !codex_supports_app_server_sync() {
        return false;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(probe_app_server()),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| rt.block_on(probe_app_server()))
            .unwrap_or(false),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ReadBounds {
    max_lines: usize,
    timeout: Duration,
}

impl Default for ReadBounds {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_READ_LINES,
            timeout: DEFAULT_READ_TIMEOUT,
        }
    }
}

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
    responses: std::collections::VecDeque<String>,
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
            self.responses.pop_front().ok_or_else(|| {
                ProviderError::Transport("fake transport: no scripted response".into())
            })
        })
    }
}

/// Spawns `codex app-server --listen stdio://` and speaks JSON-RPC on its stdio.
pub struct StdioAppServerTransport {
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
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                ProviderError::SubprocessFailure("codex app-server", e.to_string())
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            ProviderError::Transport("codex app-server: stdin not piped".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ProviderError::Transport("codex app-server: stdout not piped".into())
        })?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    async fn stderr_snippet(&mut self) -> String {
        let mut stderr_buf = Vec::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut stderr_buf).await;
        }
        let snippet = String::from_utf8_lossy(&stderr_buf);
        let trimmed = snippet.trim();
        if trimmed.is_empty() {
            String::new()
        } else {
            const MAX: usize = 500;
            if trimmed.len() > MAX {
                format!("{}…", &trimmed[..MAX])
            } else {
                trimmed.to_string()
            }
        }
    }

    async fn child_failure_message(&mut self, context: &str) -> String {
        let status = self.child.try_wait().ok().flatten();
        let stderr = self.stderr_snippet().await;
        match (status, stderr.is_empty()) {
            (Some(s), true) => format!("{context}: child exited {:?} ", s.code()),
            (Some(s), false) => format!(
                "{context}: child exited {:?}; stderr: {stderr}",
                s.code()
            ),
            (None, false) => format!("{context}; stderr: {stderr}"),
            (None, true) => context.to_string(),
        }
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
            let n = self
                .stdout
                .read_line(&mut line)
                .await
                .map_err(|e| ProviderError::Transport(format!("codex app-server read: {e}")))?;
            if n == 0 {
                let detail = self
                    .child_failure_message("codex app-server closed stdout")
                    .await;
                return Err(ProviderError::Transport(detail));
            }
            Ok(line)
        })
    }
}

/// App-server JSON-RPC client.
pub struct CodexAppServerClient<T: AppServerTransport> {
    transport: T,
    next_id: i64,
    initialized: bool,
    bounds: ReadBounds,
}

impl<T: AppServerTransport> CodexAppServerClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 1,
            initialized: false,
            bounds: ReadBounds::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_bounds(mut self, bounds: ReadBounds) -> Self {
        self.bounds = bounds;
        self
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

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

    pub async fn account_read(&mut self) -> Result<Value, ProviderError> {
        self.call("account/read", json!({})).await
    }

    /// Start ChatGPT device-code login (`account/login/start`).
    pub async fn account_login_start(&mut self) -> Result<Value, ProviderError> {
        self.call(
            "account/login/start",
            json!({ "type": "chatgptDeviceCode" }),
        )
        .await
    }

    /// List models available to the authenticated account.
    pub async fn model_list(&mut self) -> Result<Value, ProviderError> {
        self.call("model/list", json!({})).await
    }

    /// Drain notifications until `account/login/completed` (setup device-code flow).
    pub async fn wait_for_login_completed(&mut self) -> Result<(), ProviderError> {
        let started = Instant::now();
        let mut lines = 0usize;
        loop {
            if lines >= self.bounds.max_lines {
                return Err(ProviderError::Transport(format!(
                    "codex app-server: login exceeded {} lines",
                    self.bounds.max_lines
                )));
            }
            if started.elapsed() >= LOGIN_READ_TIMEOUT {
                return Err(ProviderError::Transport(
                    "codex app-server: timed out waiting for account/login/completed".into(),
                ));
            }

            let remaining = LOGIN_READ_TIMEOUT.saturating_sub(started.elapsed());
            let line = match timeout(remaining, self.transport.read_line()).await {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(ProviderError::Transport(
                        "codex app-server: timed out waiting for account/login/completed".into(),
                    ));
                }
            };
            lines += 1;

            let msg = parse_line(&line)?;

            if try_handle_server_request(&mut self.transport, &msg).await?.is_some() {
                continue;
            }

            let Some(method) = msg.get("method").and_then(Value::as_str) else {
                continue;
            };
            if msg.get("id").is_some() {
                continue;
            }
            let Some(params) = msg.get("params") else {
                continue;
            };

            if method == "account/login/completed" {
                let success = params
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if success {
                    return Ok(());
                }
                let detail = params
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("login failed");
                return Err(ProviderError::Transport(format!(
                    "codex app-server: account login failed: {detail}"
                )));
            }

            handle_notification(method, params);
        }
    }

    pub async fn thread_start(
        &mut self,
        base_instructions: &str,
        model: &str,
    ) -> Result<String, ProviderError> {
        let mut params = json!({
            "approvalPolicy": "never",
        });
        if !base_instructions.is_empty() {
            params["baseInstructions"] = json!(base_instructions);
        }
        if !model.is_empty() {
            params["model"] = json!(model);
        }
        let result = self.call("thread/start", params).await?;
        thread_id_from_result(&result).ok_or_else(|| {
            ProviderError::Transport("codex app-server: thread/start missing thread.id".into())
        })
    }

    pub async fn thread_resume(&mut self, thread_id: &str) -> Result<(), ProviderError> {
        self.call(
            "thread/resume",
            json!({
                "threadId": thread_id,
                "approvalPolicy": "never",
            }),
        )
        .await?;
        Ok(())
    }

    /// Start a turn and collect streamed assistant text until `turn/completed`.
    pub async fn turn_start_and_collect(
        &mut self,
        thread_id: &str,
        prompt: &str,
        model: &str,
    ) -> Result<String, ProviderError> {
        emit_progress(ProviderProgress::Stage {
            label: "turn/start".into(),
        });
        let mut params = json!({
            "threadId": thread_id,
            "input": [{"type": "text", "text": prompt}],
            "approvalPolicy": "never",
        });
        if !model.is_empty() {
            params["model"] = json!(model);
        }
        let _start_result = self.call("turn/start", params).await?;
        self.drain_until_turn_completed(thread_id).await
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, ProviderError> {
        if method != "initialize" && !self.initialized {
            return Err(ProviderError::Transport(
                "codex app-server: call initialize before other requests".into(),
            ));
        }

        let started = Instant::now();
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let id = self.next_id;
            self.next_id += 1;
            let request = json!({ "id": id, "method": method, "params": params.clone() });
            let line = serde_json::to_string(&request)
                .map_err(|e| ProviderError::Transport(format!("encode request: {e}")))?;

            match self.transport.write_line(&line).await {
                Ok(()) => {}
                Err(e) => return Err(e),
            }

            match self.read_response_for_id(id).await {
                Ok(v) => return Ok(v),
                Err(e) if is_overload_error(&e) && attempt < OVERLOAD_MAX_ATTEMPTS => {
                    let elapsed = started.elapsed();
                    if elapsed >= OVERLOAD_TOTAL_CAP {
                        return Err(e);
                    }
                    let base_ms = 200u64.saturating_mul(1u64 << (attempt - 1));
                    let jitter = (id as u64).wrapping_mul(37) % 100;
                    let delay = Duration::from_millis(base_ms + jitter);
                    let remaining = OVERLOAD_TOTAL_CAP.saturating_sub(elapsed);
                    tokio::time::sleep(delay.min(remaining)).await;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn read_response_for_id(&mut self, id: i64) -> Result<Value, ProviderError> {
        let started = Instant::now();
        let mut lines = 0usize;
        loop {
            if lines >= self.bounds.max_lines {
                return Err(ProviderError::Transport(format!(
                    "codex app-server: no response for id {id} within {} lines",
                    self.bounds.max_lines
                )));
            }
            if started.elapsed() >= self.bounds.timeout {
                return Err(ProviderError::Transport(format!(
                    "codex app-server: timed out waiting for response id {id}"
                )));
            }

            let remaining = self.bounds.timeout.saturating_sub(started.elapsed());
            let line = match timeout(remaining, self.transport.read_line()).await {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(ProviderError::Transport(format!(
                        "codex app-server: timed out waiting for response id {id}"
                    )));
                }
            };
            lines += 1;

            let msg = match parse_line(&line) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };

            if try_handle_server_request(&mut self.transport, &msg).await?.is_some() {
                continue;
            }

            if let Some(method) = msg.get("method").and_then(Value::as_str) {
                if msg.get("id").is_none() {
                    if let Some(params) = msg.get("params") {
                        handle_notification(method, params);
                    }
                    continue;
                }
            }

            if let Some(err) = msg.get("error") {
                if response_id_matches(&msg, id) {
                    return Err(jsonrpc_error_to_provider(err));
                }
                continue;
            }

            if response_id_matches(&msg, id) {
                return msg.get("result").cloned().ok_or_else(|| {
                    ProviderError::Transport(format!(
                        "codex app-server: response for id {id} missing result"
                    ))
                });
            }
        }
    }

    async fn drain_until_turn_completed(&mut self, thread_id: &str) -> Result<String, ProviderError> {
        let started = Instant::now();
        let mut lines = 0usize;
        let mut text = String::new();

        loop {
            if lines >= self.bounds.max_lines {
                return Err(ProviderError::Transport(format!(
                    "codex app-server: turn for thread {thread_id} exceeded {} lines",
                    self.bounds.max_lines
                )));
            }
            if started.elapsed() >= self.bounds.timeout {
                return Err(ProviderError::Transport(format!(
                    "codex app-server: timed out waiting for turn/completed on thread {thread_id}"
                )));
            }

            let remaining = self.bounds.timeout.saturating_sub(started.elapsed());
            let line = match timeout(remaining, self.transport.read_line()).await {
                Ok(Ok(l)) => l,
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(ProviderError::Transport(format!(
                        "codex app-server: timed out waiting for turn/completed on thread {thread_id}"
                    )));
                }
            };
            lines += 1;

            let msg = parse_line(&line)?;

            if try_handle_server_request(&mut self.transport, &msg).await?.is_some() {
                continue;
            }

            if let Some(method) = msg.get("method").and_then(Value::as_str) {
                if msg.get("id").is_none() {
                    if let Some(params) = msg.get("params") {
                        if method == "item/agentMessage/delta" {
                            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                                text.push_str(delta);
                                emit_progress(ProviderProgress::TextDelta {
                                    text: delta.to_string(),
                                });
                            }
                        } else {
                            handle_notification(method, params);
                        }
                        if method == "turn/completed"
                            && params
                                .get("threadId")
                                .and_then(Value::as_str)
                                .is_some_and(|t| t == thread_id)
                        {
                            return Ok(text);
                        }
                    }
                    continue;
                }
            }
        }
    }
}

fn handle_notification(method: &str, params: &Value) {
    match method {
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                emit_progress(ProviderProgress::TextDelta {
                    text: delta.to_string(),
                });
            }
        }
        "error" | "thread/error" => {
            let message = params
                .get("message")
                .or_else(|| params.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("app-server error");
            emit_progress(ProviderProgress::Error {
                message: message.to_string(),
            });
        }
        _ => {}
    }
}

async fn try_handle_server_request<T: AppServerTransport>(
    transport: &mut T,
    msg: &Value,
) -> Result<Option<()>, ProviderError> {
    let Some(req_id) = msg.get("id") else {
        return Ok(None);
    };
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };

    let response = match method {
        "item/commandExecution/requestApproval" => {
            json!({ "id": req_id, "result": { "decision": "acceptForSession" } })
        }
        "item/fileChange/requestApproval" => {
            json!({ "id": req_id, "result": { "decision": "accept" } })
        }
        _ => json!({ "id": req_id, "result": {} }),
    };
    let line = serde_json::to_string(&response)
        .map_err(|e| ProviderError::Transport(format!("encode server response: {e}")))?;
    transport.write_line(&line).await?;
    Ok(Some(()))
}

fn parse_line(line: &str) -> Result<Value, ProviderError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::Transport(
            "codex app-server: empty line".into(),
        ));
    }
    serde_json::from_str(trimmed).map_err(|e| {
        ProviderError::Transport(format!("codex app-server: malformed JSON line: {e}"))
    })
}

fn response_id_matches(msg: &Value, id: i64) -> bool {
    match msg.get("id") {
        Some(Value::Number(n)) => n.as_i64() == Some(id),
        Some(Value::Null) => true,
        _ => false,
    }
}

fn jsonrpc_error_code(error: &Value) -> Option<i64> {
    error.get("code").and_then(Value::as_i64)
}

fn is_overload_error(err: &ProviderError) -> bool {
    match err {
        ProviderError::Transport(msg) => msg.contains(&JSONRPC_OVERLOAD_CODE.to_string()),
        _ => false,
    }
}

fn jsonrpc_error_to_provider(error: &Value) -> ProviderError {
    let code = jsonrpc_error_code(error).unwrap_or(0);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    ProviderError::Transport(format!(
        "codex app-server JSON-RPC error {code}: {message}"
    ))
}

fn thread_id_from_result(result: &Value) -> Option<String> {
    result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// True when `account/read` reports a signed-in account (non-null `account`).
pub fn account_is_authenticated(result: &Value) -> bool {
    result.get("account").is_some_and(|a| !a.is_null())
}

/// Email from a ChatGPT `account/read` payload, when present.
pub fn account_email(result: &Value) -> Option<&str> {
    result
        .pointer("/account/email")
        .and_then(Value::as_str)
}

/// Device-code fields from `account/login/start` (`chatgptDeviceCode` variant).
pub fn parse_device_code_login(result: &Value) -> Option<(&str, &str)> {
    let response_type = result.get("type").and_then(Value::as_str)?;
    if response_type != "chatgptDeviceCode" {
        return None;
    }
    let url = result.get("verificationUrl").and_then(Value::as_str)?;
    let code = result.get("userCode").and_then(Value::as_str)?;
    Some((url, code))
}

/// Whether `model/list` includes the given model id (matches `id` or `model` fields).
pub fn model_list_contains(result: &Value, model_id: &str) -> bool {
    let Some(data) = result.get("data").and_then(Value::as_array) else {
        return false;
    };
    data.iter().any(|entry| {
        entry.get("id").and_then(Value::as_str) == Some(model_id)
            || entry.get("model").and_then(Value::as_str) == Some(model_id)
    })
}

fn is_thread_resume_failure(err: &ProviderError) -> bool {
    match err {
        ProviderError::Transport(msg) => {
            msg.contains("thread/resume")
                || msg.contains("not found")
                || msg.contains("no rollout")
        }
        _ => false,
    }
}

async fn spawn_shared_client(
    guard: &mut Option<CodexAppServerClient<StdioAppServerTransport>>,
) -> Result<(), ProviderError> {
    let transport = StdioAppServerTransport::spawn().await?;
    let mut client = CodexAppServerClient::new(transport);
    client.initialize().await?;
    *guard = Some(client);
    Ok(())
}

async fn app_server_followup(
    session_id: Option<&str>,
    prompt: &str,
    system_prompt: &str,
    model: &str,
) -> Result<FollowupResult, ProviderError> {
    for attempt in 0..2u8 {
        let mut guard = SHARED_CLIENT.lock().await;
        if guard.is_none() {
            spawn_shared_client(&mut guard).await?;
        }

        let Some(client) = guard.as_mut() else {
            return Err(ProviderError::Transport(
                "codex app-server: client unavailable".into(),
            ));
        };

        let run = async {
            let (thread_id, resumed) = if let Some(sid) = session_id {
                match client.thread_resume(sid).await {
                    Ok(()) => (sid.to_string(), true),
                    Err(e) if is_thread_resume_failure(&e) => {
                        let tid = client.thread_start(system_prompt, model).await?;
                        (tid, false)
                    }
                    Err(e) => return Err(e),
                }
            } else {
                let tid = client.thread_start(system_prompt, model).await?;
                (tid, false)
            };

            let text = client
                .turn_start_and_collect(&thread_id, prompt, model)
                .await?;

            Ok(FollowupResult {
                text,
                tokens_in: None,
                tokens_out: None,
                session_id: Some(thread_id),
                resumed,
            })
        };

        match run.await {
            Ok(r) => return Ok(r),
            Err(e) if attempt == 0 && is_transport_death(&e) => {
                *guard = None;
                drop(guard);
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Err(ProviderError::Transport(
        "codex app-server: failed after respawn".into(),
    ))
}

fn is_transport_death(err: &ProviderError) -> bool {
    matches!(
        err,
        ProviderError::Transport(msg)
            if msg.contains("closed stdout") || msg.contains("child exited")
    )
}

/// Codex via persistent app-server (followup) + subprocess (complete).
pub struct CodexAppServerProvider {
    exec: CodexSubprocessProvider,
}

impl CodexAppServerProvider {
    pub fn new() -> Self {
        Self {
            exec: CodexSubprocessProvider,
        }
    }
}

impl Default for CodexAppServerProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmProvider for CodexAppServerProvider {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResult, ProviderError>> + Send + 'a>> {
        self.exec.complete(prompt, system_prompt, model)
    }

    fn followup<'a>(
        &'a self,
        session_id: Option<&'a str>,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
        attachments: &'a [crate::models::Attachment],
    ) -> Pin<Box<dyn Future<Output = Result<FollowupResult, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let stamped = super::stamp_attachments_into_prompt(prompt, attachments);
            app_server_followup(session_id, &stamped, system_prompt, model).await
        })
    }
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

    fn thread_start_ok(id: i64) -> String {
        serde_json::json!({
            "id": id,
            "result": {
                "thread": { "id": "thread-abc-123" },
                "approvalPolicy": "never",
                "approvalsReviewer": "user",
                "cwd": "/tmp",
                "model": "gpt-5.5",
                "modelProvider": "openai",
                "sandbox": {}
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
    async fn fake_account_read_before_initialize_fails() {
        let transport = FakeAppServerTransport::new([initialize_ok_response(1)]);
        let mut client = CodexAppServerClient::new(transport);
        let err = client.account_read().await.unwrap_err();
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
    async fn fake_overload_retries_then_succeeds() {
        let transport = FakeAppServerTransport::new([
            serde_json::json!({
                "id": 1,
                "error": { "code": JSONRPC_OVERLOAD_CODE, "message": "rate limited" }
            })
            .to_string(),
            serde_json::json!({
                "id": 2,
                "error": { "code": JSONRPC_OVERLOAD_CODE, "message": "rate limited" }
            })
            .to_string(),
            initialize_ok_response(3),
        ]);
        let mut client = CodexAppServerClient::new(transport);
        client.initialize().await.unwrap();
        assert!(client.is_initialized());
        assert_eq!(client.transport.written.len(), 3);
    }

    #[tokio::test]
    async fn fake_non_overload_error_not_retried() {
        let transport = FakeAppServerTransport::new([serde_json::json!({
            "id": 1,
            "error": { "code": -32600, "message": "bad request" }
        })
        .to_string()]);
        let mut client = CodexAppServerClient::new(transport);
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(ref m) if m.contains("-32600")));
        assert_eq!(client.transport.written.len(), 1);
    }

    #[tokio::test]
    async fn fake_mismatched_error_id_is_skipped() {
        let transport = FakeAppServerTransport::new([
            serde_json::json!({
                "id": 999,
                "error": { "code": -32600, "message": "unrelated" }
            })
            .to_string(),
            initialize_ok_response(1),
        ]);
        let mut client = CodexAppServerClient::new(transport);
        client.initialize().await.unwrap();
    }

    #[tokio::test]
    async fn fake_bounded_read_timeout() {
        let transport = FakeAppServerTransport::new(std::iter::repeat_n(
            r#"{"method":"server/ready","params":{}}"#.to_string(),
            20,
        ));
        let bounds = ReadBounds {
            max_lines: 5,
            timeout: Duration::from_millis(50),
        };
        let mut client = CodexAppServerClient::new(transport).with_bounds(bounds);
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, ProviderError::Transport(ref m)
            if m.contains("timed out") || m.contains("within 5 lines")));
    }

    #[tokio::test]
    async fn fake_skips_notifications_until_matching_id() {
        let transport = FakeAppServerTransport::new(vec![
            r#"{"method":"server/ready","params":{}}"#.to_string(),
            initialize_ok_response(1),
        ]);
        let mut client = CodexAppServerClient::new(transport);
        client.initialize().await.unwrap();
    }

    #[tokio::test]
    async fn fake_thread_start_returns_thread_id() {
        let transport = FakeAppServerTransport::new([
            initialize_ok_response(1),
            thread_start_ok(2),
        ]);
        let mut client = CodexAppServerClient::new(transport);
        client.initialize().await.unwrap();
        let tid = client.thread_start("sys", "gpt-5.5").await.unwrap();
        assert_eq!(tid, "thread-abc-123");
    }

    #[test]
    fn account_is_authenticated_detects_non_null_account() {
        let v = json!({ "requiresOpenaiAuth": true, "account": { "type": "chatgpt", "email": "a@b.com", "planType": "pro" } });
        assert!(account_is_authenticated(&v));
        let missing = json!({ "requiresOpenaiAuth": true, "account": null });
        assert!(!account_is_authenticated(&missing));
    }

    #[test]
    fn parse_device_code_login_extracts_url_and_code() {
        let v = json!({
            "type": "chatgptDeviceCode",
            "loginId": "lid",
            "verificationUrl": "https://auth.example/verify",
            "userCode": "ABCD-1234"
        });
        assert_eq!(
            parse_device_code_login(&v),
            Some(("https://auth.example/verify", "ABCD-1234"))
        );
        let other = json!({ "type": "chatgpt", "authUrl": "https://x", "loginId": "l" });
        assert!(parse_device_code_login(&other).is_none());
    }

    #[test]
    fn model_list_contains_matches_id_or_model_field() {
        let v = json!({
            "data": [
                { "id": "gpt-5.5", "model": "gpt-5.5", "displayName": "GPT", "description": "", "hidden": false, "isDefault": true, "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [] }
            ]
        });
        assert!(model_list_contains(&v, "gpt-5.5"));
        assert!(!model_list_contains(&v, "gpt-4"));
    }

    #[tokio::test]
    async fn fake_device_code_login_completed_notification() {
        let transport = FakeAppServerTransport::new([
            initialize_ok_response(1),
            serde_json::json!({
                "id": 2,
                "result": {
                    "type": "chatgptDeviceCode",
                    "loginId": "lid",
                    "verificationUrl": "https://auth.example/verify",
                    "userCode": "CODE"
                }
            })
            .to_string(),
            serde_json::json!({
                "method": "account/login/completed",
                "params": { "success": true, "loginId": "lid", "error": null }
            })
            .to_string(),
        ]);
        let mut client = CodexAppServerClient::new(transport);
        client.initialize().await.unwrap();
        let start = client.account_login_start().await.unwrap();
        let pair = parse_device_code_login(&start).unwrap();
        assert_eq!(pair.0, "https://auth.example/verify");
        assert_eq!(pair.1, "CODE");
        client.wait_for_login_completed().await.unwrap();
    }

    #[tokio::test]
    async fn fake_delta_aggregation_until_turn_completed() {
        let transport = FakeAppServerTransport::new([
            initialize_ok_response(1),
            serde_json::json!({ "id": 2, "result": { "turn": { "id": "turn-1" } } }).to_string(),
            serde_json::json!({
                "method": "item/agentMessage/delta",
                "params": { "delta": "hel", "itemId": "i", "threadId": "t1", "turnId": "turn-1" }
            }).to_string(),
            serde_json::json!({
                "method": "item/agentMessage/delta",
                "params": { "delta": "lo", "itemId": "i", "threadId": "t1", "turnId": "turn-1" }
            }).to_string(),
            serde_json::json!({
                "method": "turn/completed",
                "params": { "threadId": "t1", "turn": { "id": "turn-1" } }
            }).to_string(),
        ]);
        let mut client = CodexAppServerClient::new(transport);
        client.initialize().await.unwrap();
        let text = client
            .turn_start_and_collect("t1", "hi", "gpt-5.5")
            .await
            .unwrap();
        assert_eq!(text, "hello");
    }
}
