/// The current analyst's identifier for `STATE.md`. Falls back through
/// `TRIAGE_OWNER` → `USER` (unix) → `USERNAME` (Windows) → "unknown" so the
/// soft-lock has a useful value even in headless / CI environments and on
/// Windows where `$USER` does not exist.
pub(crate) fn current_owner() -> String {
    if let Ok(v) = std::env::var("TRIAGE_OWNER") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Ok(v) = std::env::var("USER") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Ok(v) = std::env::var("USERNAME") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    "unknown".into()
}
