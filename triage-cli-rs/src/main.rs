//! triage-cli entry point. The library is in `lib.rs`; this is just the binary glue.
fn main() -> std::process::ExitCode {
    triage_cli::run()
}
