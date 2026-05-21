//! Shared test utilities for environment variable isolation.

use std::sync::Mutex;

/// Global lock for tests that mutate environment variables.
/// All integration tests that set TRIAGE_HOME, TRIAGE_MEMORY_MD, etc. must
/// hold this lock to prevent races with other tests running in parallel.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the global ENV_LOCK, recovering from poison if a previous test
/// panicked while holding it.
pub fn acquire_env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
