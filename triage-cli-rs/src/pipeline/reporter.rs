use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::mpsc;

/// Typed value emitted via `Reporter::record_metric`. Kept simple — the only
/// consumers today are `MetricsReporter` (captures for JSON) and the default
/// no-op on all other implementations.
#[derive(Debug, Clone)]
pub enum MetricValue {
    Float(f64),
    Int(i64),
    Bool(bool),
    Str(String),
}

/// Progress reporter: decouples display from orchestration. The structured
/// pipeline does not emit a terminal "done" payload through this trait — the
/// caller of `investigate_one_structured` receives the `StructuredInvestigation`
/// return value directly, so a separate done callback is no longer useful.
pub trait Reporter: Send + Sync {
    fn phase_started(&self, phase: &str, detail: &str);
    fn phase_done(&self, phase: &str, detail: &str);
    fn phase_failed(&self, phase: &str, err: &str);
    /// Record a named metric for observability. Default is a no-op so existing
    /// reporters (`StderrReporter`, `SilentReporter`, `ChannelReporter`) need
    /// no changes. Only `MetricsReporter` captures these.
    fn record_metric(&self, _key: &str, _value: MetricValue) {}
}

#[derive(Default)]
pub struct StderrReporter {
    pub verbose: bool,
}

impl Reporter for StderrReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        if self.verbose {
            if detail.is_empty() {
                eprintln!("→ {phase}");
            } else {
                eprintln!("→ {phase}: {detail}");
            }
        }
    }
    fn phase_done(&self, phase: &str, detail: &str) {
        if detail.is_empty() {
            eprintln!("✓ {phase}");
        } else {
            eprintln!("✓ {phase}: {detail}");
        }
    }
    fn phase_failed(&self, phase: &str, err: &str) {
        eprintln!("✗ {phase}: {err}");
    }
}

#[derive(Default)]
pub struct SilentReporter;
impl Reporter for SilentReporter {
    fn phase_started(&self, _phase: &str, _detail: &str) {}
    fn phase_done(&self, _phase: &str, _detail: &str) {}
    fn phase_failed(&self, _phase: &str, _err: &str) {}
}

/// Show a braille spinner while `f` runs, when stderr is a TTY.
pub async fn spinner<F, T>(text: &str, show: bool, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    use std::io::IsTerminal;
    if show && std::io::stderr().is_terminal() {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        pb.set_message(text.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));
        let result = f.await;
        pb.finish_and_clear();
        result
    } else {
        f.await
    }
}

/// Reporter that forwards each phase as an async message to a TUI consumer.
///
/// Note: there is no terminal `Done` event in v1 — the caller of
/// `investigate_one_structured` receives the `StructuredInvestigation` value
/// synchronously. The TUI updates its inbox row by reading `STATE.md` off
/// disk after the call returns.
pub struct ChannelReporter {
    pub tx: mpsc::UnboundedSender<TuiEvent>,
}

#[derive(Debug, Clone)]
pub enum TuiEvent {
    PhaseStarted { phase: String, detail: String },
    PhaseDone { phase: String, detail: String },
    PhaseFailed { phase: String, err: String },
}

impl Reporter for ChannelReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        let _ = self.tx.send(TuiEvent::PhaseStarted {
            phase: phase.into(),
            detail: detail.into(),
        });
    }
    fn phase_done(&self, phase: &str, detail: &str) {
        let _ = self.tx.send(TuiEvent::PhaseDone {
            phase: phase.into(),
            detail: detail.into(),
        });
    }
    fn phase_failed(&self, phase: &str, err: &str) {
        let _ = self.tx.send(TuiEvent::PhaseFailed {
            phase: phase.into(),
            err: err.into(),
        });
    }
}

/// Shows a braille spinner on stderr while each pipeline phase is running.
/// TTY-gated: falls back to inner `phase_started` when stderr is not a terminal
/// so tests, the watcher, and piped runs are unaffected.
pub struct SpinnerReporter {
    inner: Box<dyn Reporter>,
    current: Mutex<Option<ProgressBar>>,
}

impl SpinnerReporter {
    pub fn new(inner: Box<dyn Reporter>) -> Self {
        Self {
            inner,
            current: Mutex::new(None),
        }
    }

    fn clear_current(&self) {
        if let Some(pb) = self.current.lock().unwrap().take() {
            pb.finish_and_clear();
        }
    }
}

impl Reporter for SpinnerReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        use std::io::IsTerminal;
        self.clear_current();
        if !std::io::stderr().is_terminal() {
            self.inner.phase_started(phase, detail);
            return;
        }
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        let msg = if detail.is_empty() {
            phase.to_string()
        } else {
            format!("{phase}: {detail}")
        };
        pb.set_message(msg);
        pb.enable_steady_tick(Duration::from_millis(80));
        *self.current.lock().unwrap() = Some(pb);
    }

    fn phase_done(&self, phase: &str, detail: &str) {
        self.clear_current();
        self.inner.phase_done(phase, detail);
    }

    fn phase_failed(&self, phase: &str, err: &str) {
        self.clear_current();
        self.inner.phase_failed(phase, err);
    }
}

/// Wraps another reporter and captures phase timings plus named metrics.
/// Pass `&MetricsReporter` as the `&dyn Reporter` to the pipeline; after the
/// call returns, read collected data via `phase_timings()` and `named_metrics()`.
pub struct MetricsReporter {
    inner: Box<dyn Reporter>,
    phase_starts: Mutex<HashMap<String, Instant>>,
    phase_timings: Mutex<HashMap<String, f64>>,
    named: Mutex<Vec<(String, MetricValue)>>,
}

impl MetricsReporter {
    pub fn new(inner: Box<dyn Reporter>) -> Self {
        Self {
            inner,
            phase_starts: Mutex::new(HashMap::new()),
            phase_timings: Mutex::new(HashMap::new()),
            named: Mutex::new(Vec::new()),
        }
    }

    /// Wall-clock seconds per phase (keyed by phase name).
    pub fn phase_timings(&self) -> HashMap<String, f64> {
        self.phase_timings.lock().unwrap().clone()
    }

    /// Ordered list of (key, value) pairs recorded via `record_metric`.
    pub fn named_metrics(&self) -> Vec<(String, MetricValue)> {
        self.named.lock().unwrap().clone()
    }
}

impl Reporter for MetricsReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        self.phase_starts
            .lock()
            .unwrap()
            .insert(phase.to_string(), Instant::now());
        self.inner.phase_started(phase, detail);
    }

    fn phase_done(&self, phase: &str, detail: &str) {
        let elapsed = self
            .phase_starts
            .lock()
            .unwrap()
            .remove(phase)
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.phase_timings
            .lock()
            .unwrap()
            .insert(phase.to_string(), elapsed);
        self.inner.phase_done(phase, detail);
    }

    fn phase_failed(&self, phase: &str, err: &str) {
        let elapsed = self
            .phase_starts
            .lock()
            .unwrap()
            .remove(phase)
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.phase_timings
            .lock()
            .unwrap()
            .insert(phase.to_string(), elapsed);
        self.inner.phase_failed(phase, err);
    }

    fn record_metric(&self, key: &str, value: MetricValue) {
        self.named
            .lock()
            .unwrap()
            .push((key.to_string(), value.clone()));
        self.inner.record_metric(key, value);
    }
}
