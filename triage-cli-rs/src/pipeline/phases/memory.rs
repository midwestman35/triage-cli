use crate::memory;
use crate::models::{MemoryContext, MemoryEntry};

use super::super::ctx::PhaseCtx;

pub fn run(ctx: &mut PhaseCtx<'_>) -> Vec<MemoryEntry> {
    ctx.reporter
        .phase_started("memory_lookup", "querying prior investigations");
    let symptom_head: String = ctx.ticket.description.chars().take(500).collect();
    let prior = if let Some(hits) = ctx.opts.memory_hits_override.clone() {
        hits
    } else {
        memory::retrieve_similar(&ctx.ticket.subject, &symptom_head, 3).unwrap_or_default()
    };
    if let Ok(Some(_dup)) = memory::find_duplicate(&ctx.ticket.id.to_string()) {
        eprintln!("⚠ ZD-{} was previously investigated", ctx.ticket.id);
    }
    let query_tokens = ctx
        .ticket
        .subject
        .to_ascii_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    ctx.session.memory_context = Some(MemoryContext {
        entries: prior.clone(),
        query_tokens,
    });
    ctx.reporter.phase_done(
        "memory_lookup",
        &format!("{} prior investigation(s) found", prior.len()),
    );
    prior
}
