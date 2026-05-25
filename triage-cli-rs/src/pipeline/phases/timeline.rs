use super::super::ctx::PhaseCtx;

pub fn run(ctx: &PhaseCtx<'_>) {
    ctx.reporter.phase_started("build_timeline", "");
    ctx.reporter.phase_done(
        "build_timeline",
        &format!("{} event(s)", ctx.session.timeline.len()),
    );
}
