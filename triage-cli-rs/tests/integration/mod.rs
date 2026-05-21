//! Offline integration tests: exercise each runbook workflow end-to-end using
//! fixture data and mock clients. No network calls required.
//!
//! Run with: cargo test --test integration

mod common;
mod runbook_01_setup;
mod runbook_02_triage;
mod runbook_03_sitemap;
mod runbook_05_llm;
mod runbook_06_watch;
mod runbook_08_certify;
mod runbook_cli_smoke;
mod zendesk_mock;
