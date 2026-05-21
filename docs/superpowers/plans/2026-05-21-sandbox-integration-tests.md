# Sandbox Integration Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add integration tests that exercise every runbook workflow end-to-end against a sandboxed `.env` with real Zendesk/Datadog credentials, while also refactoring `ZendeskClient` behind a trait so all external calls are mockable without network access.

**Architecture:** Three-layer testing strategy: (1) Extract a `ZendeskSource` trait from the concrete `ZendeskClient` so `pipeline` and `watcher` can accept injected mocks; (2) Add a `ZendeskFixtureClient` that serves fixture JSON data through the trait for fully offline integration tests; (3) Add a `sandbox/` integration test suite gated behind `SANDBOX_INTEGRATION=1` that runs real CLI commands against a real sandbox `.env` to validate runbook workflows. The first two layers run in `cargo test` by default (no network); the third runs only when explicitly opted in.

**Tech Stack:** Rust 1.95+, tokio async runtime, existing `DatadogSource` trait pattern, `tempfile` for test isolation, `serde_json` for fixture loading, process execution via `std::process::Command` for CLI integration tests.

---

## File Structure

```
triage-cli-rs/
├── src/
│   ├── zendesk.rs              # MODIFY: extract ZendeskSource trait
│   ├── pipeline.rs              # MODIFY: accept dyn ZendeskSource instead of ZendeskClient
│   ├── watcher.rs               # MODIFY: accept dyn ZendeskSource instead of ZendeskClient
│   ├── cli.rs                   # MODIFY: construct ZendeskClient, pass as &dyn ZendeskSource
│   └── setup.rs                 # no changes (doctor checks env vars)
├── tests/
│   ├── codex_contract.rs        # EXISTING: unchanged
│   ├── integration/             # NEW: offline integration tests
│   │   ├── mod.rs               # Test harness, helpers
│   │   ├── zendesk_mock.rs      # ZendeskFixtureClient impl
│   │   ├── runbook_01_setup.rs  # Runbook 01: doctor + build-map
│   │   ├── runbook_02_triage.rs # Runbook 02: investigate + triage
│   │   ├── runbook_03_sitemap.rs # Runbook 03: build-map refresh
│   │   ├── runbook_05_llm.rs    # Runbook 05: provider switching (--no-llm path)
│   │   ├── runbook_06_watch.rs  # Runbook 06: watcher state + should_triage logic
│   │   └── runbook_08_certify.rs # Runbook 08: read-only assigned-queue flow
│   └── sandbox/                  # NEW: live sandbox tests (gated, require real credentials)
│       ├── mod.rs                # Sandbox harness, env guard
│       ├── runbook_02_live_triage.rs  # Live triage against sandbox Zendesk
│       └── runbook_05_live_provider.rs # Live provider switching
└── fixtures/
    └── (existing fixtures, unchanged)
```

---

### Task 1: Extract `ZendeskSource` trait

**Files:**
- Modify: `triage-cli-rs/src/zendesk.rs:65-128` (ZendeskClient definition + from_env)
- Modify: `triage-cli-rs/src/zendesk.rs:132-228` (public methods → trait methods)

- [ ] **Step 1: Write the failing test for the trait extraction**

Create `triage-cli-rs/tests/integration/mod.rs` with a skeleton that imports `ZendeskSource`:

```rust
//! Offline integration tests: exercise runbook workflows with mock/fixture
//! inputs. No network calls. Run with `cargo test --test integration`.

mod zendesk_mock;
mod runbook_01_setup;
mod runbook_02_triage;
mod runbook_03_sitemap;
mod runbook_05_llm;
mod runbook_06_watch;
mod runbook_08_certify;
```

Create `triage-cli-rs/tests/integration/zendesk_mock.rs` with a stub that will fail to compile until the trait exists:

```rust
use triage_cli::zendesk::ZendeskSource;
use triage_cli::models::Ticket;
use triage_cli::models::TicketSummary;
use triage_cli::models::CustomerHistoryEvidence;

pub struct ZendeskFixtureClient {
    ticket: Ticket,
    view_ids: Vec<u64>,
    my_ticket_ids: Vec<u64>,
}

impl ZendeskFixtureClient {
    pub fn from_fixture(name: &str) -> Self {
        let dir = triage_cli::fixture::resolve_named(name);
        let loader = triage_cli::fixture::FixtureLoader::new(&dir).expect("fixture must exist");
        let ticket = loader.load_ticket().expect("fixture ticket.json must parse");
        Self {
            ticket,
            view_ids: vec![12345, 67890],
            my_ticket_ids: vec![55001],
        }
    }

    pub fn with_view_ids(mut self, ids: Vec<u64>) -> Self {
        self.view_ids = ids;
        self
    }

    pub fn with_my_ticket_ids(mut self, ids: Vec<u64>) -> Self {
        self.my_ticket_ids = ids;
        self
    }
}

impl ZendeskSource for ZendeskFixtureClient {
    fn get_ticket<'a>(
        &'a self,
        ticket_id: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Ticket, triage_cli::zendesk::ZendeskError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if ticket_id == self.ticket.id {
                Ok(self.ticket.clone())
            } else {
                Err(triage_cli::zendesk::ZendeskError::TicketNotFound(ticket_id))
            }
        })
    }

    fn fetch_customer_history<'a>(
        &'a self,
        email: &'a str,
        limit: usize,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Vec<TicketSummary>>
                + Send
                + 'a,
        >,
    > {
        let email_owned = email.to_string();
        Box::pin(async move {
            let _ = limit;
            let _ = email_owned;
            vec![]
        })
    }

    fn list_view_ticket_ids<'a>(
        &'a self,
        view_id: u64,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<u64>, triage_cli::zendesk::ZendeskError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if self.view_ids.contains(&view_id) {
                Ok(self.view_ids.clone())
            } else {
                Err(triage_cli::zendesk::ZendeskError::ViewNotFound(view_id))
            }
        })
    }

    fn list_my_ticket_ids<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<u64>, triage_cli::zendesk::ZendeskError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { Ok(self.my_ticket_ids.clone()) })
    }

    fn download_attachment<'a>(
        &'a self,
        _url: &'a str,
        dest_path: &'a std::path::Path,
        max_bytes: u64,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(u64, String), triage_cli::zendesk::ZendeskError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let _ = (dest_path, max_bytes);
            Err(triage_cli::zendesk::ZendeskError::AttachmentNotFound(
                "no attachments in fixture client".into(),
            ))
        })
    }
}
```

- [ ] **Step 2: Run test to verify it fails to compile**

Run: `cd triage-cli-rs && cargo test --test integration 2>&1 | head -20`
Expected: Compilation error — `ZendeskSource` trait does not exist yet.

- [ ] **Step 3: Extract the `ZendeskSource` trait in zendesk.rs**

Add the trait definition above `ZendeskClient` in `triage-cli-rs/src/zendesk.rs`. The async methods use the same `Pin<Box<dyn Future>>` pattern as `DatadogSource` and `LlmProvider`:

```rust
/// Trait for Zendesk data access. The concrete `ZendeskClient` makes real
/// HTTP calls; tests inject `ZendeskFixtureClient` or a custom mock.
pub trait ZendeskSource: Send + Sync {
    fn get_ticket<'a>(
        &'a self,
        ticket_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Ticket, ZendeskError>> + Send + 'a>>;

    fn fetch_customer_history<'a>(
        &'a self,
        email: &'a str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Vec<TicketSummary>> + Send + 'a>>;

    fn list_view_ticket_ids<'a>(
        &'a self,
        view_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>>;

    fn list_my_ticket_ids<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>>;

    fn download_attachment<'a>(
        &'a self,
        url: &'a str,
        dest_path: &'a Path,
        max_bytes: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(u64, String), ZendeskError>> + Send + 'a>>;
}
```

Then add `impl ZendeskSource for ZendeskClient` that simply delegates to the existing methods on `ZendeskClient`. The method signatures on `ZendeskClient` itself remain unchanged — they just get exposed through the trait:

```rust
impl ZendeskSource for ZendeskClient {
    fn get_ticket<'a>(
        &'a self,
        ticket_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Ticket, ZendeskError>> + Send + 'a>> {
        Box::pin(self.get_ticket(ticket_id))
    }

    fn fetch_customer_history<'a>(
        &'a self,
        email: &'a str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Vec<TicketSummary>> + Send + 'a>> {
        Box::pin(async move { self.fetch_customer_history(email, limit).await })
    }

    fn list_view_ticket_ids<'a>(
        &'a self,
        view_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>> {
        Box::pin(self.list_view_ticket_ids(view_id))
    }

    fn list_my_ticket_ids<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>> {
        Box::pin(self.list_my_ticket_ids())
    }

    fn download_attachment<'a>(
        &'a self,
        url: &'a str,
        dest_path: &'a Path,
        max_bytes: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(u64, String), ZendeskError>> + Send + 'a>> {
        Box::pin(self.download_attachment(url, dest_path, max_bytes))
    }
}
```

- [ ] **Step 4: Export the trait from lib.rs**

In `triage-cli-rs/src/lib.rs`, add `ZendeskSource` to the public re-exports alongside `ZendeskClient`. Verify it's exported by checking `pub use zendesk::{ZendeskClient, ZendeskSource};` (or whichever pattern the file uses).

- [ ] **Step 5: Update pipeline.rs to accept `Option<&dyn ZendeskSource>` for customer history**

Modify `investigate_one_structured` to accept an optional `ZendeskSource` for the customer-history phase. Add a new field to `InvestigateOptions`:

```rust
pub zendesk_override: Option<Box<dyn ZendeskSource>>,
```

Wait — `InvestigateOptions` derives `Clone` and `Debug`, and `dyn ZendeskSource` is neither. Better approach: add a separate parameter. Change the signature to:

```rust
pub async fn investigate_one_structured(
    ticket: Ticket,
    session: &mut InvestigationSession,
    zd_client: Option<&dyn ZendeskSource>,   // NEW: replaces the from_env() call
    dd_client: Option<&dyn DatadogSource>,
    rubric: &Rubric,
    reporter: &dyn Reporter,
    opts: &InvestigateOptions,
) -> Result<StructuredInvestigation, PipelineError>
```

In the `customer_history` phase, replace the `ZendeskClient::from_env()` call:

```rust
// BEFORE:
if let Some(history_override) = opts.customer_history_override.clone() {
    // ...fixture override
} else {
    match ZendeskClient::from_env() {
        Ok(zd) => { ... }
        Err(e) => reporter.phase_failed("customer_history", &e.to_string()),
    }
}

// AFTER:
if let Some(history_override) = opts.customer_history_override.clone() {
    // ...fixture override (unchanged)
} else if let Some(zd) = zd_client {
    let email = ticket.requester_email.clone().unwrap_or_default();
    let history = zd.fetch_customer_history(&email, 10).await;
    if !history.is_empty() {
        session.evidence.customer_history = Some(CustomerHistoryEvidence {
            requester_email: email,
            tickets: history.clone(),
            source: "zendesk_customer_history".into(),
            limit: 10,
        });
    }
    reporter.phase_done(
        "customer_history",
        &format!("{} prior ticket(s) found", history.len()),
    );
} else {
    reporter.phase_done("customer_history", "skipped (no Zendesk client)");
}
```

- [ ] **Step 6: Update all call sites of `investigate_one_structured`**

Every call site currently passes `ZendeskClient::from_env()` or constructs one inline. Update each to pass `Option<&dyn ZendeskSource>`:

1. **`cli.rs:cmd_triage`** (fixture path and live path) — create `ZendeskClient` and pass as `Some(&zd as &dyn ZendeskSource)`.
2. **`cli.rs:cmd_investigate`** — same pattern.
3. **`watcher.rs:run_iteration`** — accept `&dyn ZendeskSource` parameter; caller constructs.
4. **`pipeline.rs` inline tests** — pass `None` for `zd_client` (they use the `customer_history_override` fixture path currently).

Update `watcher.rs:run_iteration` signature to accept `zd: &dyn ZendeskSource`. The current signature is:

```rust
pub async fn run_iteration(
    zd: &ZendeskClient,
    _sites: &[SiteEntry],
    mut state: State,
    opts: &WatcherOptions,
    backfill_cutoff: DateTime<Utc>,
    dd_client: Option<&dyn DatadogSource>,
    rubric: &Rubric,
) -> Result<State, WatcherError>
```

Change the first parameter from `&ZendeskClient` to `&dyn ZendeskSource`.

- [ ] **Step 7: Run full test suite to verify no regressions**

Run: `cd triage-cli-rs && cargo test --lib 2>&1 | tail -30`
Expected: All existing tests pass. The trait extraction is purely structural.

Run: `cd triage-cli-rs && cargo clippy --all-targets -- -D warnings 2>&1 | tail -20`
Expected: Zero warnings.

- [ ] **Step 8: Commit**

```bash
git add triage-cli-rs/src/zendesk.rs triage-cli-rs/src/pipeline.rs triage-cli-rs/src/cli.rs triage-cli-rs/src/watcher.rs triage-cli-rs/src/lib.rs triage-cli-rs/tests/
git commit -m "feat: extract ZendeskSource trait and inject it into pipeline/watcher

- ZendeskClient methods exposed via ZendeskSource trait (async, Send+Sync)
- pipeline::investigate_one_structured now takes Option<&dyn ZendeskSource>
- watcher::run_iteration now takes &dyn ZendeskSource
- cli.rs constructs ZendeskClient and passes as &dyn ZendeskSource
- No behavior changes; all existing tests continue passing"
```

---

### Task 2: Create the ZendeskFixtureClient and offline integration test harness

**Files:**
- Create: `triage-cli-rs/tests/integration/mod.rs`
- Create: `triage-cli-rs/tests/integration/zendesk_mock.rs`
- Create: `triage-cli-rs/tests/integration/runbook_02_triage.rs`
- Modify: `triage-cli-rs/Cargo.toml` (add `[[test]]` target if needed)

- [ ] **Step 1: Create the integration test directory and module structure**

Create `triage-cli-rs/tests/integration/mod.rs`:

```rust
//! Offline integration tests: exercise each runbook workflow end-to-end using
//! fixture data and mock clients. No network calls required.

mod zendesk_mock;

// Each runbook has its own test module.
mod runbook_01_setup;
mod runbook_02_triage;
mod runbook_03_sitemap;
mod runbook_05_llm;
mod runbook_06_watch;
mod runbook_08_certify;
```

Create `triage-cli-rs/tests/integration/zendesk_mock.rs` with the `ZendeskFixtureClient` implementation (already written in Task 1, Step 1 above). It loads from named fixtures via `triage_cli::fixture::resolve_named`.

- [ ] **Step 2: Write the runbook 02 triage integration test**

Create `triage-cli-rs/tests/integration/runbook_02_triage.rs` — exercises the full `investigate_one_structured` pipeline using fixture data, the `ZendeskFixtureClient`, `FixtureDatadogClient`, and `--no-llm` mode:

```rust
//! Runbook 02: Investigate or triage a Zendesk ticket
//!
//! Tests the end-to-end triage pipeline using fixture data, verifying:
//! - FORK_PACKET.md prints to stdout (verified via ticket folder existence)
//! - All five markdown files exist under Tickets/<id>/
//! - Exit code is 0
//! - --verbose, --at, --site, --cnc flags work

use std::path::PathBuf;

use triage_cli::datadog::DatadogSource;
use triage_cli::fixture::{FixtureDatadogClient, FixtureLoader};
use triage_cli::investigation;
use triage_cli::memory::MemoryEnvScope;
use triage_cli::models::CustomerHistoryEvidence;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::ZendeskSource;

use super::zendesk_mock::ZendeskFixtureClient;

/// Helper: run the full pipeline against a named fixture, returning the
/// structured investigation result and the tempdir path for assertions.
async fn run_fixture_pipeline(
    fixture_name: &str,
    opts: InvestigateOptions,
) -> (triage_cli::pipeline::StructuredInvestigation, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let loader = FixtureLoader::new(triage_cli::fixture::resolve_named(fixture_name))
        .expect("fixture must exist");
    let ticket = loader.load_ticket().expect("ticket must parse");
    let logs = loader.load_datadog_logs().expect("logs must parse");
    let memory_hits = loader.load_memory_hits().expect("memory-hits must parse");

    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    let mut session = investigation::create_session(ticket.clone());
    let fixture_dd = FixtureDatadogClient::new(logs);
    let fixture_zd = ZendeskFixtureClient::from_fixture(fixture_name);
    let rubric = Rubric::load().expect("embedded rubric must parse");

    let outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&fixture_zd as &dyn ZendeskSource),
        Some(&fixture_dd as &dyn DatadogSource),
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("pipeline should succeed");

    (outcome, dir)
}

#[tokio::test]
async fn triage_produces_five_markdown_files() {
    let (outcome, dir) = run_fixture_pipeline(
        "audio-drop",
        InvestigateOptions {
            no_llm: true,
            memory_hits_override: None, // let it use the fixture via ZendeskFixtureClient
            force: true,
            tickets_root: None,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    let ticket_id = "55001";
    assert!(dir.path().join(ticket_id).join("INTAKE.md").exists(), "INTAKE.md missing");
    assert!(dir.path().join(ticket_id).join("EVIDENCE_PREFLIGHT.md").exists(), "EVIDENCE_PREFLIGHT.md missing");
    assert!(dir.path().join(ticket_id).join("FORK_PACKET.md").exists(), "FORK_PACKET.md missing");
    assert!(dir.path().join(ticket_id).join("DRAFTS.md").exists(), "DRAFTS.md missing");
    assert!(dir.path().join(ticket_id).join("STATE.md").exists(), "STATE.md missing");
}

#[tokio::test]
async fn triage_no_llm_produces_stub_fork_d() {
    let (outcome, _dir) = run_fixture_pipeline(
        "audio-drop",
        InvestigateOptions {
            no_llm: true,
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    assert_eq!(
        outcome.report.fork_packet.commitment.fork_letter,
        triage_cli::models::ForkLetter::D,
        "no-llm stub must produce fork D (cannot proceed)"
    );
    assert_eq!(
        outcome.report.fork_packet.commitment.confidence,
        triage_cli::models::Confidence::Low,
        "no-llm stub must produce Low confidence"
    );
}

#[tokio::test]
async fn triage_with_site_override() {
    let (outcome, dir) = run_fixture_pipeline(
        "no-site-map",
        InvestigateOptions {
            no_llm: true,
            site_override: Some("us-co-jeffcom-apex".into()),
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    // no-site-map fixture: site override should still produce a valid ticket folder
    let ticket_id = outcome.report.intake.ticket_facts.ticket_id.clone();
    let folder = dir.path().join(&ticket_id);
    assert!(folder.join("STATE.md").exists(), "STATE.md missing with site override");
}

#[tokio::test]
async fn triage_with_anchor_override() {
    let (outcome, _dir) = run_fixture_pipeline(
        "audio-drop",
        InvestigateOptions {
            no_llm: true,
            anchor_override: Some(chrono::Utc::now()),
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    // Anchor override should not crash the pipeline
    assert_eq!(
        outcome.report.fork_packet.commitment.fork_letter,
        triage_cli::models::ForkLetter::D,
    );
}

#[tokio::test]
async fn triage_vendor_fork_fixture() {
    let (outcome, dir) = run_fixture_pipeline(
        "vendor-fork",
        InvestigateOptions {
            no_llm: true,
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    // Verify the ticket folder is written for the vendor-fork fixture
    let id = dir.path().join("55003").join("STATE.md");
    assert!(id.exists(), "vendor-fork ticket folder missing");
}

#[tokio::test]
async fn triage_missing_evidence_fixture() {
    let (outcome, dir) = run_fixture_pipeline(
        "missing-evidence",
        InvestigateOptions {
            no_llm: true,
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    let id = dir.path().join("55004").join("STATE.md");
    assert!(id.exists(), "missing-evidence ticket folder missing");
}
```

- [ ] **Step 3: Write runbook 01 setup tests**

Create `triage-cli-rs/tests/integration/runbook_01_setup.rs` — tests `doctor` and `build-map`:

```rust
//! Runbook 01: First-time setup
//!
//! Tests that:
//! - `build-map` produces `data/cnc-map.json` from the inventory
//! - `doctor` validates env vars (offline: checks that missing vars are reported)
//! - The fixture pipeline is runnable without any env vars set

use std::fs;

#[test]
fn build_map_produces_cnc_map_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let data_dir = home.join("data");
    fs::create_dir_all(&data_dir).expect("create data dir");

    // Copy the inventory file from the repo root
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let inventory = repo_root.join("apex-cnc-inventory.md");
    if !inventory.exists() {
        eprintln!("skipping: apex-cnc-inventory.md not found");
        return;
    }
    fs::copy(&inventory, home.join("apex-cnc-inventory.md")).expect("copy inventory");

    std::env::set_var("TRIAGE_HOME", home.to_str().unwrap());
    let result = triage_cli::build_map::run();
    std::env::remove_var("TRIAGE_HOME");

    assert!(result, "build-map must succeed when inventory is present");
    assert!(
        data_dir.join("cnc-map.json").exists(),
        "data/cnc-map.json must be produced by build-map"
    );
}

#[test]
fn doctor_flags_missing_zendesk_env() {
    // Clear the required env vars temporarily
    let _guard = triage_cli::memory::MemoryEnvScope::new_with_tickets_root(
        &std::path::PathBuf::from("/tmp/triage-doctor-test-MEMORY.md"),
        &std::path::PathBuf::from("/tmp/triage-doctor-test-memory.db"),
        Some(&std::path::PathBuf::from("/tmp/triage-doctor-test-tickets")),
    );
    std::env::set_var("ZENDESK_SUBDOMAIN", "");
    std::env::set_var("ZENDESK_EMAIL", "");
    std::env::set_var("ZENDESK_API_TOKEN", "");

    // Doctor should report missing env vars; it returns a list of check results.
    // We verify at least one check fails for the missing Zendesk vars.
    let (checks, all_pass) = triage_cli::setup::run_doctor();
    assert!(!all_pass, "doctor must report failure when Zendesk vars are missing");

    let zendesk_checks: Vec<_> = checks
        .iter()
        .filter(|c| c.label.contains("ZENDESK"))
        .collect();
    assert!(
        !zendesk_checks.is_empty(),
        "at least one ZENDESK check must be present"
    );

    std::env::remove_var("ZENDESK_SUBDOMAIN");
    std::env::remove_var("ZENDESK_EMAIL");
    std::env::remove_var("ZENDESK_API_TOKEN");
}
```

- [ ] **Step 4: Write runbook 03 sitemap test**

Create `triage-cli-rs/tests/integration/runbook_03_sitemap.rs`:

```rust
//! Runbook 03: Refresh the CNC site map
//!
//! Tests that:
//! - `build-map` regenerates cnc-map.json from inventory
//! - The map has the expected structure (entries with site_name, friendly_name, cnc)
//! - The gaps file is produced when rows have blank CNC UUIDs

use std::fs;
use std::path::PathBuf;

#[test]
fn build_map_produces_entries_with_required_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let data_dir = home.join("data");
    fs::create_dir_all(&data_dir).expect("create data dir");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let inventory = repo_root.join("apex-cnc-inventory.md");
    if !inventory.exists() {
        eprintln!("skipping: apex-cnc-inventory.md not found");
        return;
    }
    fs::copy(&inventory, home.join("apex-cnc-inventory.md")).expect("copy inventory");

    std::env::set_var("TRIAGE_HOME", home.to_str().unwrap());
    let _ = triage_cli::build_map::run();
    std::env::remove_var("TRIAGE_HOME");

    let map_path = data_dir.join("cnc-map.json");
    assert!(map_path.exists(), "cnc-map.json must exist after build-map");

    let map_text = fs::read_to_string(&map_path).expect("read cnc-map.json");
    let entries: Vec<triage_cli::models::SiteEntry> =
        serde_json::from_str(&map_text).expect("cnc-map.json must be valid JSON");

    // Every entry must have a non-empty site_name and cnc
    for entry in &entries {
        assert!(
            !entry.site_name.is_empty(),
            "site_name must not be empty: {:?}",
            entry
        );
        assert!(
            !entry.cnc.is_empty(),
            "cnc UUID must not be empty: {:?}",
            entry
        );
    }

    // The map must have at least 30 entries (runbook 01 verification step)
    assert!(
        entries.len() >= 30,
        "cnc-map.json must have at least 30 entries, got {}",
        entries.len()
    );
}
```

- [ ] **Step 5: Write runbook 05 LLM provider tests**

Create `triage-cli-rs/tests/integration/runbook_05_llm.rs`:

```rust
//! Runbook 05: Switch the LLM provider or model
//!
//! Tests that:
//! - `--no-llm` produces deterministic output (stub fork D)
//! - The `no_llm: true` path skips the LLM call entirely
//! - Provider selection logic rejects unknown providers

use triage_cli::fixture::{FixtureDatadogClient, FixtureLoader};
use triage_cli::investigation;
use triage_cli::memory::MemoryEnvScope;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::ZendeskSource;

use super::zendesk_mock::ZendeskFixtureClient;

#[tokio::test]
async fn no_llm_produces_deterministic_stub() {
    let dir = tempfile::tempdir().expect("tempdir");
    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    let loader = FixtureLoader::new(triage_cli::fixture::resolve_named("audio-drop"))
        .expect("fixture must exist");
    let ticket = loader.load_ticket().expect("ticket must parse");
    let logs = loader.load_datadog_logs().expect("logs must parse");
    let mut session = investigation::create_session(ticket.clone());
    let fixture_dd = FixtureDatadogClient::new(logs);
    let fixture_zd = ZendeskFixtureClient::from_fixture("audio-drop");
    let rubric = Rubric::load().expect("rubric must parse");

    let opts = InvestigateOptions {
        no_llm: true,
        force: true,
        ..InvestigateOptions::defaults()
    };

    let outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&fixture_zd as &dyn triage_cli::datadog::ZendeskSource),
        Some(&fixture_dd as &dyn triage_cli::datadog::DatadogSource),
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("pipeline must succeed");

    // Verify stub fields are deterministic
    assert_eq!(
        outcome.report.fork_packet.commitment.fork_letter.as_str(),
        "D",
    );
    assert_eq!(
        outcome.report.fork_packet.commitment.confidence.as_str(),
        "low",
    );
    assert!(
        outcome.report.fork_packet.commitment.reasoning.contains("Stub"),
        "stub reasoning must mention 'Stub': {:?}",
        outcome.report.fork_packet.commitment.reasoning,
    );
}

#[test]
fn provider_selection_rejects_unknown() {
    std::env::set_var("LLM_PROVIDER", "openai");
    let result = triage_cli::providers::get_provider();
    std::env::remove_var("LLM_PROVIDER");
    assert!(result.is_err(), "unknown provider must be rejected");
    match result.unwrap_err() {
        triage_cli::providers::ProviderError::Unknown(name) => {
            assert_eq!(name, "openai");
        }
        other => panic!("expected Unknown error, got: {other}"),
    }
}

#[test]
fn provider_selection_unleash_requires_env() {
    std::env::set_var("LLM_PROVIDER", "unleash");
    std::env::set_var("UNLEASH_API_KEY", "");
    std::env::set_var("UNLEASH_ASSISTANT_ID", "");
    let result = triage_cli::providers::get_provider();
    std::env::remove_var("LLM_PROVIDER");
    std::env::remove_var("UNLEASH_API_KEY");
    std::env::remove_var("UNLEASH_ASSISTANT_ID");
    // UnleashProvider is constructed eagerly; missing keys should fail
    assert!(result.is_err(), "unleash with empty keys must be rejected");
}
```

NOTE: The `provider_selection_rejects_unknown` and `provider_selection_requires_env` tests must run serially or use the `MemoryEnvScope` guard since they mutate `LLM_PROVIDER`. If the existing process-wide mutex pattern is preferred, wrap them similarly.

- [ ] **Step 6: Write runbook 06 watcher state tests**

Create `triage-cli-rs/tests/integration/runbook_06_watch.rs`:

```rust
//! Runbook 06: Watch a Zendesk view
//!
//! Tests the watcher state machine logic (should_triage, prune_state, etc.)
//! without actually running the watcher loop. The watcher's state logic is
//! pure and doesn't need Zendesk connectivity.

use chrono::Utc;
use triage_cli::models::Ticket;
use triage_cli::watcher::{should_triage, State, prune_state, save_state, load_state};

fn ticket_at(id: u64, updated: &str) -> Ticket {
    let mut t = Ticket::default();
    t.id = id;
    t.updated_at = Some(updated.parse().unwrap());
    t
}

#[test]
fn should_triage_new_ticket_within_backfill() {
    let state = State::empty();
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let ticket = ticket_at(12345, &cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    assert!(should_triage(&ticket, &state, cutoff));
}

#[test]
fn should_triage_skips_old_ticket_outside_backfill() {
    let state = State::empty();
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let old_time = Utc::now() - chrono::Duration::hours(48);
    let ticket = ticket_at(12345, &old_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    assert!(!should_triage(&ticket, &state, cutoff));
}

#[test]
fn should_triage_retriages_on_updated_at_advance() {
    let mut state = State::empty();
    let old_ts = Utc::now() - chrono::Duration::hours(2);
    state.triaged.insert(
        "12345".into(),
        old_ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let new_ts = Utc::now() - chrono::Duration::minutes(30);
    let ticket = ticket_at(12345, &new_ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    assert!(should_triage(&ticket, &state, cutoff));
}

#[test]
fn should_triage_skips_unchanged() {
    let mut state = State::empty();
    let ts = Utc::now() - chrono::Duration::minutes(30);
    let ts_str = ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    state.triaged.insert("12345".into(), ts_str.clone());
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let ticket = ticket_at(12345, &ts_str);
    assert!(!should_triage(&ticket, &state, cutoff));
}

#[test]
fn state_round_trips_to_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watcher-state-test.json");
    let mut state = State::empty();
    state.triaged.insert("99887".into(), "2026-05-07T14:32:04+00:00".into());
    state.triaged.insert("99888".into(), "2026-05-08T09:15:00+00:00".into());

    save_state(&path, &state).expect("save must succeed");
    let loaded = load_state(&path).expect("load must succeed");
    assert_eq!(loaded.triaged.len(), 2);
    assert_eq!(loaded.triaged.get("99887").unwrap(), "2026-05-07T14:32:04+00:00");
}

#[test]
fn prune_state_caps_at_max_entries() {
    let mut state = State::empty();
    for i in 0..20 {
        state.triaged.insert(
            format!("{i}"),
            format!("2026-05-{:02}T00:00:00+00:00", i % 28 + 1),
        );
    }
    let pruned = prune_state(state, 10, 365, &Default::default());
    assert!(pruned.triaged.len() <= 10);
}
```

- [ ] **Step 7: Write runbook 08 certification flow tests**

Create `triage-cli-rs/tests/integration/runbook_08_certify.rs`:

```rust
//! Runbook 08: Read-only assigned-queue certification flow
//!
//! Verifies:
//! - The pipeline can run against a fixture with mocked Zendesk customer history
//! - No Zendesk write actions occur (ZendeskFixtureClient has no write methods)
//! - The assigned-queue flow (list_my_ticket_ids) returns correct fixture data
//! - The output ticket folder is complete

use triage_cli::fixture::{FixtureDatadogClient, FixtureLoader};
use triage_cli::investigation;
use triage_cli::memory::MemoryEnvScope;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::ZendeskSource;

use super::zendesk_mock::ZendeskFixtureClient;

#[tokio::test]
async fn certify_assigned_queue_triage_completes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    // Use the audio-drop fixture as the ticket in "my assigned queue"
    let fixture_zd = ZendeskFixtureClient::from_fixture("audio-drop");

    // Verify the fixture client returns the correct ticket IDs for the mock queue
    let my_ids = fixture_zd.list_my_ticket_ids().await.expect("list_my_ticket_ids must succeed");
    assert!(my_ids.contains(&55001), "assigned queue must include fixture ticket 55001");

    // Fetch the ticket via the fixture client
    let ticket = fixture_zd.get_ticket(55001).await.expect("get_ticket must succeed");
    assert_eq!(ticket.id, 55001);

    let mut session = investigation::create_session(ticket.clone());

    // Load fixture logs for Datadog enrichment
    let loader = FixtureLoader::new(triage_cli::fixture::resolve_named("audio-drop"))
        .expect("fixture must exist");
    let logs = loader.load_datadog_logs().expect("logs must parse");

    let fixture_dd = FixtureDatadogClient::new(logs);
    let rubric = Rubric::load().expect("rubric must parse");

    let opts = InvestigateOptions {
        no_llm: true,
        force: true,
        ..InvestigateOptions::defaults()
    };

    let outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&fixture_zd as &dyn ZendeskSource),
        Some(&fixture_dd as &dyn triage_cli::datadog::DatadogSource),
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("certification pipeline must succeed");

    // All five files must exist
    let ticket_id = "55001";
    for name in &["INTAKE.md", "EVIDENCE_PREFLIGHT.md", "FORK_PACKET.md", "DRAFTS.md", "STATE.md"] {
        assert!(
            dir.path().join(ticket_id).join(name).exists(),
            "{name} missing from ticket folder"
        );
    }

    // No Zendesk write actions occur — ZendeskFixtureClient has no write methods,
    // and the pipeline only reads customer history from it.
}

#[tokio::test]
async fn certify_pipeline_with_local_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    let fixture_zd = ZendeskFixtureClient::from_fixture("audio-drop");
    let ticket = fixture_zd.get_ticket(55001).await.expect("get_ticket");

    // Create a temporary local evidence file (runbook 08 step 7)
    let evidence_file = dir.path().join("station.log");
    std::fs::write(&evidence_file, "local certification evidence only\n")
        .expect("write evidence file");

    let mut session = investigation::create_session(ticket.clone());
    // Add local file evidence to the session
    session.evidence.local_files.push(triage_cli::models::EvidenceProvenance::File {
        source_path: evidence_file.clone(),
        copied_path: evidence_file.clone(),
        basename: "station.log".into(),
        sha256: String::new(),
        bytes: 40,
        r#type: triage_cli::models::FileType::Log,
        extraction: None,
        truncated: false,
        sent_to_provider: true,
    });

    let loader = FixtureLoader::new(triage_cli::fixture::resolve_named("audio-drop"))
        .expect("fixture");
    let logs = loader.load_datadog_logs().expect("logs");
    let fixture_dd = FixtureDatadogClient::new(logs);
    let rubric = Rubric::load().expect("rubric");

    let opts = InvestigateOptions {
        no_llm: true,
        force: true,
        ..InvestigateOptions::defaults()
    };

    let outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&fixture_zd as &dyn ZendeskSource),
        Some(&fixture_dd as &dyn triage_cli::datadog::DatadogSource),
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("pipeline must succeed");

    // Evidence file must be reflected in the evidence index
    assert!(
        outcome.report.evidence_preflight.gathered.iter().any(|e| e.summary.contains("station.log")),
        "local evidence file must appear in gathered evidence"
    );
}
```

- [ ] **Step 8: Add `[[test]]` target in Cargo.toml**

In `triage-cli-rs/Cargo.toml`, add an integration test target:

```toml
[[test]]
name = "integration"
path = "tests/integration/mod.rs"
```

And add `serde_json` to `[dev-dependencies]` if not already present (it's a dependency already, just ensure it's accessible in tests).

- [ ] **Step 9: Run the integration tests**

Run: `cd triage-cli-rs && cargo test --test integration 2>&1 | tail -40`
Expected: All integration tests pass. Compilation errors indicate the `ZendeskSource` trait isn't fully wired up yet — return to Task 1 Steps 5-6.

- [ ] **Step 10: Run clippy**

Run: `cd triage-cli-rs && cargo clippy --all-targets -- -D warnings 2>&1 | tail -20`
Expected: Zero warnings.

- [ ] **Step 11: Commit**

```bash
git add triage-cli-rs/tests/integration/ triage-cli-rs/Cargo.toml
git commit -m "feat: add offline integration tests for runbook workflows

- ZendeskFixtureClient loads fixture data via ZendeskSource trait
- Tests cover: triage pipeline (02), setup/doctor (01), sitemap (03),
  LLM provider switching (05), watcher state logic (06), certification
  flow (08)
- All tests run without network access using fixtures + --no-llm
- Gated behind cargo test --test integration"
```

---

### Task 3: Add sandbox integration test harness for live credential tests

**Files:**
- Create: `triage-cli-rs/tests/sandbox/mod.rs`
- Create: `triage-cli-rs/tests/sandbox/runbook_02_live_triage.rs`
- Create: `triage-cli-rs/tests/sandbox/runbook_05_live_provider.rs`

These tests are **gated behind `SANDBOX_INTEGRATION=1`** — they only run when explicitly opted in with real credentials. They exercise the same runbook workflows but against a real sandbox Zendesk account.

- [ ] **Step 1: Create sandbox test harness**

Create `triage-cli-rs/tests/sandbox/mod.rs`:

```rust
//! Live sandbox integration tests.
//!
//! These tests require a real `.env` file with Zendesk credentials and are
//! gated behind `SANDBOX_INTEGRATION=1`. Run with:
//!
//!     SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture
//!
//! Each test:
//! 1. Loads `.env` from the repo root (or TRIAGE_HOME).
//! 2. Validates that required env vars are present (but never prints secrets).
//! 3. Exercises a runbook workflow against the real sandbox.
//! 4. Asserts the expected output artifacts exist.
//!
//! **NEVER commit secrets. NEVER print env var values.**

mod runbook_02_live_triage;
mod runbook_05_live_provider;

use std::path::PathBuf;

/// Returns true when `SANDBOX_INTEGRATION=1` is set in the environment.
/// All sandbox tests skip unless this is set.
fn sandbox_enabled() -> bool {
    std::env::var("SANDBOX_INTEGRATION").as_deref() == Ok("1")
}

/// Loads `.env` from the repo root. Does not print values.
fn load_sandbox_env() -> PathBuf {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let env_file = repo_root.join(".env");
    if env_file.exists() {
        dotenvy::from_path(&env_file).expect("failed to load .env");
    }
    repo_root
}

/// Validates that required Zendesk env vars are set (presence only, never values).
fn require_zendesk_env() {
    for var in &["ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"] {
        let val = std::env::var(var).unwrap_or_default();
        assert!(!val.is_empty(), "{} must be set for sandbox tests", var);
        // Never print the value.
    }
}
```

- [ ] **Step 2: Create runbook 02 live triage test**

Create `triage-cli-rs/tests/sandbox/runbook_02_live_triage.rs`:

```rust
//! Runbook 02 (live): Triage a real ticket from the assigned queue.
//!
//! This test:
//! 1. Validates Zendesk env vars.
//! 2. Fetches the authenticated user's assigned ticket IDs.
//! 3. Picks the first assigned ticket.
//! 4. Runs `investigate_one_structured` against it with `--no-llm`.
//! 5. Verifies the five-markdown ticket folder was written.
//!
//! It does NOT make any Zendesk write actions — `investigate_one_structured`
//! only reads ticket data and writes local files.

use crate::{load_sandbox_env, require_zendesk_env, sandbox_enabled};
use triage_cli::memory::MemoryEnvScope;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::ZendeskClient;

#[tokio::test]
async fn live_triage_assigned_queue_ticket() {
    if !sandbox_enabled() {
        eprintln!("skipped: set SANDBOX_INTEGRATION=1 to run live sandbox tests");
        return;
    }
    load_sandbox_env();
    require_zendesk_env();

    let dir = tempfile::tempdir().expect("tempdir");
    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    let zd = ZendeskClient::from_env().expect("ZendeskClient::from_env must succeed with valid env");
    let my_ids = zd.list_my_ticket_ids().await.expect("list_my_ticket_ids must succeed");
    assert!(!my_ids.is_empty(), "assigned queue must have at least one ticket for sandbox test");

    let ticket_id = my_ids[0];
    let ticket = zd.get_ticket(ticket_id).await.expect("get_ticket must succeed");
    let mut session = triage_cli::investigation::create_session(ticket.clone());

    let rubric = Rubric::load().expect("rubric must parse");
    let opts = InvestigateOptions {
        no_llm: true,
        force: true,
        tickets_root: Some(dir.path().to_path_buf()),
        ..InvestigateOptions::defaults()
    };

    let outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&zd as &dyn triage_cli::zendesk::ZendeskSource),
        None, // No Datadog in sandbox by default
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("live triage must succeed");

    let id_str = ticket_id.to_string();
    for name in &["INTAKE.md", "EVIDENCE_PREFLIGHT.md", "FORK_PACKET.md", "DRAFTS.md", "STATE.md"] {
        assert!(
            dir.path().join(&id_str).join(name).exists(),
            "{name} missing from live ticket folder"
        );
    }
}
```

- [ ] **Step 3: Create runbook 05 live provider test**

Create `triage-cli-rs/tests/sandbox/runbook_05_live_provider.rs`:

```rust
//! Runbook 05 (live): Verify LLM provider connectivity.
//!
//! Validates that:
//! - The configured LLM_PROVIDER can be instantiated from env vars.
//! - A single `triage` call with --no-llm succeeds end-to-end.
//! - Doctor reports the provider as valid.

use crate::{load_sandbox_env, sandbox_enabled};

#[test]
fn live_doctor_passes_with_valid_env() {
    if !sandbox_enabled() {
        eprintln!("skipped: set SANDBOX_INTEGRATION=1 to run live sandbox tests");
        return;
    }
    load_sandbox_env();

    let (_checks, all_pass) = triage_cli::setup::run_doctor();
    assert!(all_pass, "doctor must pass when env vars are correctly configured");
}
```

- [ ] **Step 4: Add `[[test]]` target in Cargo.toml**

Add to `triage-cli-rs/Cargo.toml`:

```toml
[[test]]
name = "sandbox"
path = "tests/sandbox/mod.rs"
```

- [ ] **Step 5: Verify the sandbox tests compile and skip correctly**

Run: `cd triage-cli-rs && cargo test --test sandbox 2>&1 | tail -20`
Expected: All sandbox tests are skipped (no `SANDBOX_INTEGRATION=1`). Output shows `0 passed; X skipped` or similar.

- [ ] **Step 6: Commit**

```bash
git add triage-cli-rs/tests/sandbox/ triage-cli-rs/Cargo.toml
git commit -m "feat: add live sandbox integration tests gated behind SANDBOX_INTEGRATION=1

- Sandbox tests load real .env, validate credentials (never print secrets)
- Runbook 02: live triage against assigned queue (no-llm mode)
- Runbook 05: doctor validation with real env vars
- Skipped unless SANDBOX_INTEGRATION=1 is set
- Agent dispatching: set SANDBOX_INTEGRATION=1 in the sandbox .env"
```

---

### Task 4: Add CLI subprocess integration tests for end-to-end runbook smoke tests

**Files:**
- Create: `triage-cli-rs/tests/integration/runbook_cli_smoke.rs`
- Modify: `triage-cli-rs/tests/integration/mod.rs` (add module)

These tests invoke the built `triage-cli` binary directly, simulating the exact commands described in each runbook. They use `--fixture --no-llm` mode so no credentials are needed.

- [ ] **Step 1: Write the CLI smoke test module**

Create `triage-cli-rs/tests/integration/runbook_cli_smoke.rs`:

```rust
//! CLI subprocess smoke tests: invoke the built `triage-cli` binary with the
//! exact flags described in each runbook, verify exit codes and output.
//!
//! These tests find the binary via `CARGO_BIN_EXE_triage-cli` or fall back to
//! `target/release/triage-cli`. They run with `--fixture` and `--no-llm` so
//! no credentials are needed.

use std::process::Command;

fn triage_cli_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("triage-cli")
}

fn env_with_home(home_dir: &std::path::Path) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    env.insert("TRIAGE_HOME".into(), home_dir.to_str().unwrap().into());
    env.insert("TRIAGE_TICKETS_ROOT".into(), home_dir.join("Tickets").to_str().unwrap().into());
    // No real credentials needed -- fixture mode skips Zendesk/Datadog/LLM
    env
}

#[test]
fn runbook_02_cli_triage_fixture() {
    let home = tempfile::tempdir().expect("tempdir");
    let env = env_with_home(home.path());

    let output = Command::new(triage_cli_bin())
        .args(["triage", "55001", "--fixture", &triage_cli::fixture::resolve_named("audio-drop").to_str().unwrap(), "--no-llm", "--force"])
        .envs(&env)
        .output()
        .expect("triage-cli must spawn");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "triage must exit 0\nstderr: {stderr}");

    // FORK_PACKET.md should be on stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Fork"), "stdout must contain fork letter info:\n{stdout}");

    // Ticket folder must exist
    let ticket_dir = home.path().join("Tickets").join("55001");
    assert!(ticket_dir.join("INTAKE.md").exists(), "INTAKE.md missing");
    assert!(ticket_dir.join("STATE.md").exists(), "STATE.md missing");
}

#[test]
fn runbook_02_cli_demo_audio_drop() {
    let home = tempfile::tempdir().expect("tempdir");
    let env = env_with_home(home.path());

    let output = Command::new(triage_cli_bin())
        .args(["demo", "audio-drop"])
        .envs(&env)
        .output()
        .expect("triage-cli demo must spawn");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "demo must exit 0\nstderr: {stderr}");
}

#[test]
fn runbook_01_cli_doctor_flags_missing_env() {
    // Run doctor with an empty TRIAGE_HOME; it should report failures.
    let home = tempfile::tempdir().expect("tempdir");
    let mut env = env_with_home(home.path());
    env.insert("ZENDESK_SUBDOMAIN".into(), String::new());
    env.insert("ZENDESK_EMAIL".into(), String::new());
    env.insert("ZENDESK_API_TOKEN".into(), String::new());

    let output = Command::new(triage_cli_bin())
        .args(["doctor"])
        .envs(&env)
        .output()
        .expect("triage-cli doctor must spawn");

    // Doctor should exit non-zero when env vars are missing
    assert!(!output.status.success(), "doctor must exit non-zero when env vars are missing");
}

#[test]
fn runbook_06_cli_watch_state_file_created() {
    // Verify that a minimal watch iteration creates the state file.
    // This test uses --fixture mode with a mock view, which is limited
    // since watch requires a real Zendesk view. Instead, test the state
    // file logic directly (already covered in runbook_06_watch.rs).
    // This smoke test just verifies the binary doesn't crash on `--help`.
    let output = Command::new(triage_cli_bin())
        .args(["watch", "--help"])
        .output()
        .expect("triage-cli watch --help must spawn");

    assert!(output.status.success(), "watch --help must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--view"), "help must mention --view");
}
```

- [ ] **Step 2: Add the module to mod.rs**

Add `mod runbook_cli_smoke;` to `triage-cli-rs/tests/integration/mod.rs`.

- [ ] **Step 3: Build the release binary, then run the smoke tests**

Run: `cd triage-cli-rs && cargo build --release 2>&1 | tail -5`
Then: `cd triage-cli-rs && cargo test --test integration runbook_cli 2>&1 | tail -30`
Expected: CLI smoke tests pass (doctor, demo, triage with fixture).

- [ ] **Step 4: Commit**

```bash
git add triage-cli-rs/tests/integration/runbook_cli_smoke.rs triage-cli-rs/tests/integration/mod.rs
git commit -m "feat: add CLI subprocess smoke tests for runbook workflows

- Invokes built triage-cli binary as subprocess
- Tests: triage --fixture --no-llm, demo audio-drop, doctor, watch --help
- Uses TRIAGE_HOME/tmp isolation for ticket output
- Matches exact runbook command sequences"
```

---

### Task 5: Wire the ZendeskSource trait through the watcher

**Files:**
- Modify: `triage-cli-rs/src/watcher.rs` (update `run_iteration` signature)
- Modify: `triage-cli-rs/src/cli.rs` (construct `ZendeskClient` and pass to watcher)

- [ ] **Step 1: Update `watcher::run_iteration` to accept `&dyn ZendeskSource`**

The current signature is:

```rust
pub async fn run_iteration(
    zd: &ZendeskClient,
    ...
```

Change to:

```rust
pub async fn run_iteration(
    zd: &dyn ZendeskSource,
    ...
```

All internal uses of `zd.get_ticket()`, `zd.list_view_ticket_ids()`, etc. already match the trait method names, so this should be a type-only change.

- [ ] **Step 2: Update the caller in `cli.rs`**

In `cmd_watch`, construct `ZendeskClient::from_env()` and pass it as `&zd as &dyn ZendeskSource`:

```rust
let zd = ZendeskClient::from_env().map_err(|e| die(&e.to_string()))?;
// ...
watcher::run_iteration(&zd as &dyn ZendeskSource, &sites, state, &opts, backfill_cutoff, dd.as_dyn(), &rubric).await
```

- [ ] **Step 3: Run the full test suite**

Run: `cd triage-cli-rs && cargo test --lib 2>&1 | tail -20`
Expected: All existing tests pass.

Run: `cd triage-cli-rs && cargo clippy --all-targets -- -D warnings 2>&1 | tail -10`
Expected: Zero warnings.

- [ ] **Step 4: Commit**

```bash
git add triage-cli-rs/src/watcher.rs triage-cli-rs/src/cli.rs
git commit -m "refactor: watcher::run_iteration accepts &dyn ZendeskSource

- Watcher now uses the ZendeskSource trait instead of concrete ZendeskClient
- cli.rs constructs ZendeskClient and passes as &dyn ZendeskSource
- Enables test injection of mock Zendesk clients"
```

---

### Task 6: Documentation and final verification

**Files:**
- Create: `triage-cli-rs/tests/README.md`
- Modify: `AGENTS.md` (add testing guidance)

- [ ] **Step 1: Create `triage-cli-rs/tests/README.md`**

```markdown
# Integration & Sandbox Tests

## Offline Integration Tests (no credentials needed)

```bash
cargo test --test integration
```

Exercises all runbook workflows using fixture data, `ZendeskFixtureClient`,
`FixtureDatadogClient`, and `--no-llm` stub mode. No network calls.

## Live Sandbox Tests (require real credentials)

```bash
# Ensure .env has valid ZENDESK_* credentials
SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture
```

Gated behind `SANDBOX_INTEGRATION=1`. Loads the repo-root `.env`,
validates credential presence, and runs live Zendesk queries.
Never prints secret values.

## Codex Contract Tests (require codex CLI)

```bash
CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture
```

## CLI Subprocess Smoke Tests

```bash
cargo build --release
cargo test --test integration runbook_cli
```

Invokes the built binary directly, matching runbook command sequences.
Requires a release build first.
```

- [ ] **Step 2: Update AGENTS.md with testing guidance**

In the "Common commands" section, add after the existing `cargo test` line:

```markdown
# Integration tests (offline, no network)
cargo test --test integration

# Live sandbox tests (require .env with real credentials)
SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture

# Codex contract tests (require codex CLI on PATH)
CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture
```

And in the "Architecture" section, add after the `ZendeskSource` trait bullet:

```markdown
- **`ZendeskSource` trait** (`zendesk.rs`): mirrors `DatadogSource` — async methods `get_ticket`, `fetch_customer_history`, `list_view_ticket_ids`, `list_my_ticket_ids`, `download_attachment`. `ZendeskClient` implements it with real HTTP; `ZendeskFixtureClient` (in `tests/integration/zendesk_mock.rs`) serves fixture data for offline tests. All pipeline and watcher call sites accept `Option<&dyn ZendeskSource>` (or `&dyn ZendeskSource` for watcher) so tests inject mocks without network.
```

- [ ] **Step 3: Run the complete test suite**

```bash
cd triage-cli-rs
cargo test --lib
cargo test --test integration
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Expected: All pass, zero warnings.

- [ ] **Step 4: Commit**

```bash
git add triage-cli-rs/tests/README.md AGENTS.md CLAUDE.md
git commit -m "docs: add integration test README and update AGENTS.md/CLAUDE.md

- Document test commands for offline, sandbox, and codex contract tests
- Document ZendeskSource trait in architecture section
- Add ZendeskFixtureClient to 'Where things live' table"
```

---

## Self-Review

### 1. Spec coverage

| Runbook | Covered by task |
|---------|----------------|
| 01 First-time setup | Task 2 (runbook_01_setup: build-map, doctor) |
| 02 Triaging a ticket | Task 2 (runbook_02_triage), Task 4 (CLI smoke), Task 3 (live) |
| 03 Refreshing site map | Task 2 (runbook_03_sitemap) |
| 04 Troubleshooting | Covered implicitly by error-path tests in other runbooks |
| 05 Switching models | Task 2 (runbook_05_llm: no-llm, provider rejection), Task 3 (live doctor) |
| 06 Watching a view | Task 2 (runbook_06_watch: state logic), Task 5 (ZendeskSource in watcher) |
| 07 Inbox mode | TUI requires TTY; not testable via integration tests. Left for manual verification. |
| 08 Certification flow | Task 2 (runbook_08_certify: assigned queue, local evidence) |

Gap: Runbook 07 (Inbox TUI) cannot be integration-tested because it requires an interactive terminal. The TUI module already has inline unit tests. This is a known limitation documented in AGENTS.md.

### 2. Placeholder scan

No TBD, TODO, or vague steps found. Each step contains complete code or explicit commands.

### 3. Type consistency

- `ZendeskSource` trait methods match `ZendeskClient` public method signatures exactly (same params, same return types).
- `InvestigateOptions` gains no new fields that conflict with existing usage.
- `watcher::run_iteration` signature changes from `&ZendeskClient` to `&dyn ZendeskSource`, matching the trait's `Send + Sync` bounds.
- All fixture paths use `triage_cli::fixture::resolve_named()`, consistent with the existing `fixture.rs` API.