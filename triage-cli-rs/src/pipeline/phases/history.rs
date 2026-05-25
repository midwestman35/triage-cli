use crate::models::CustomerHistoryEvidence;

use super::super::ctx::PhaseCtx;

pub async fn run(ctx: &mut PhaseCtx<'_>) {
    ctx.reporter
        .phase_started("customer_history", "fetching requester history");
    if let Some(history_override) = ctx.opts.customer_history_override.clone() {
        let count = history_override.tickets.len();
        ctx.session.evidence.customer_history = Some(history_override);
        ctx.reporter.phase_done(
            "customer_history",
            &format!("{count} prior ticket(s) (fixture)"),
        );
    } else if let Some(zd) = ctx.clients.zendesk {
        let email = ctx.ticket.requester_email.clone().unwrap_or_default();
        let history = zd.fetch_customer_history(&email, 10).await;
        if !history.is_empty() {
            ctx.session.evidence.customer_history = Some(CustomerHistoryEvidence {
                requester_email: email,
                tickets: history.clone(),
                source: "zendesk_customer_history".into(),
                limit: 10,
            });
        }
        ctx.reporter.phase_done(
            "customer_history",
            &format!("{} prior ticket(s) found", history.len()),
        );
    } else {
        ctx.reporter
            .phase_done("customer_history", "skipped (no Zendesk client)");
    }
}
