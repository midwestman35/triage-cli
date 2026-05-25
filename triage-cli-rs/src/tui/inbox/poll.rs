use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::{mpsc, oneshot};

use super::app::InboxEvent;
use crate::datadog::{DatadogClient, DatadogSource};
use crate::extract;
use crate::investigation;
use crate::models::Ticket;
use crate::pipeline::{
    self, ChannelReporter, InvestigateOptions, StructuredInvestigation, TuiEvent,
};
use crate::playbook::Rubric;
use crate::watcher::{self, State, WatcherOptions};
use crate::zendesk::{ZendeskClient, ZendeskSource};

pub(crate) async fn poll_iteration(
    state: State,
    opts: WatcherOptions,
    backfill_cutoff: DateTime<Utc>,
    in_flight_triages: HashSet<u64>,
    tx: mpsc::UnboundedSender<InboxEvent>,
) -> Result<(State, HashSet<u64>), String> {
    let zd = ZendeskClient::from_env().map_err(|e| e.to_string())?;
    poll_iteration_with(
        &zd,
        state,
        opts,
        backfill_cutoff,
        &in_flight_triages,
        tx,
        live_pipeline_runner(),
    )
    .await
}

type InboxPipelineFuture =
    Pin<Box<dyn Future<Output = Result<StructuredInvestigation, String>> + Send>>;
type InboxPipelineRunner = Arc<
    dyn Fn(
            Ticket,
            InvestigateOptions,
            bool,
            mpsc::UnboundedSender<InboxEvent>,
        ) -> InboxPipelineFuture
        + Send
        + Sync,
>;

fn live_pipeline_runner() -> InboxPipelineRunner {
    Arc::new(|ticket, opts, no_logs, tx| Box::pin(run_pipeline(ticket, opts, no_logs, tx)))
}

async fn poll_iteration_with(
    zd: &dyn ZendeskSource,
    state: State,
    opts: WatcherOptions,
    backfill_cutoff: DateTime<Utc>,
    in_flight_triages: &HashSet<u64>,
    tx: mpsc::UnboundedSender<InboxEvent>,
    runner: InboxPipelineRunner,
) -> Result<(State, HashSet<u64>), String> {
    let view_ids: Vec<u64> = match opts.view_id {
        Some(id) => zd
            .list_view_ticket_ids(id)
            .await
            .map_err(|e| e.to_string())?,
        None => zd.list_my_ticket_ids().await.map_err(|e| e.to_string())?,
    };
    let view_set: HashSet<u64> = view_ids.iter().copied().collect();
    let mut new_state = state;

    for tid in &view_ids {
        let ticket = match zd.get_ticket(*tid).await {
            Ok(t) => t,
            Err(e) => {
                let _ = tx.send(InboxEvent::TriageFailed {
                    ticket_id: *tid,
                    error: e.to_string(),
                });
                continue;
            }
        };
        let key = tid.to_string();
        let updated = ticket.updated_at.unwrap_or(ticket.created_at);
        let needs_triage = watcher::should_triage(&ticket, &new_state, backfill_cutoff);
        if !needs_triage {
            new_state
                .triaged
                .entry(key.clone())
                .or_insert_with(|| updated.to_rfc3339());
            continue;
        }
        if in_flight_triages.contains(tid) {
            continue;
        }

        let tx_inner = tx.clone();
        let opts_inner = InvestigateOptions {
            interactive: false,
            workspace: None,
            cnc_override: None,
            site_override: None,
            anchor_override: None,
            window_minutes: opts.window_minutes,
            levels: opts.levels.clone(),
            verbose: opts.verbose,
            redact_enabled: true,
            no_llm: false,
            force: false,
            customer_history_override: None,
            memory_hits_override: None,
            followup_mode: false,
            tickets_root: None,
            allow_unscoped_fixture_logs: false,
        };
        let no_logs = opts.no_logs;
        let tid_copy = *tid;
        let updated_at = updated;
        let runner = runner.clone();
        let _ = tx.send(InboxEvent::TriagePhase {
            ticket_id: tid_copy,
            label: "Triaging".into(),
            step: 1,
        });

        tokio::spawn(async move {
            match runner(ticket, opts_inner, no_logs, tx_inner.clone()).await {
                Ok(outcome) => {
                    let _ = tx_inner.send(InboxEvent::TriageComplete {
                        ticket_id: tid_copy,
                        folder: outcome.paths.folder,
                        updated_at,
                    });
                }
                Err(e) => {
                    let _ = tx_inner.send(InboxEvent::TriageFailed {
                        ticket_id: tid_copy,
                        error: e,
                    });
                }
            }
        });
    }
    let live_set: HashSet<String> = view_set.iter().map(|id| id.to_string()).collect();
    new_state =
        watcher::prune_by_membership(new_state, &live_set, watcher::DEFAULT_MEMBERSHIP_GRACE_DAYS);
    new_state = watcher::prune_state(
        new_state,
        watcher::DEFAULT_PRUNE_CAP,
        watcher::DEFAULT_TTL_DAYS,
        &live_set,
    );
    Ok((new_state, view_set))
}

pub(crate) async fn triage_one_ticket(
    ticket_id: u64,
    opts: WatcherOptions,
    tx: mpsc::UnboundedSender<InboxEvent>,
    site_override: Option<String>,
) -> Result<(), String> {
    let zd = ZendeskClient::from_env().map_err(|e| e.to_string())?;
    let _ = tx.send(InboxEvent::TriagePhase {
        ticket_id,
        label: "Fetching ticket".into(),
        step: 1,
    });
    let ticket = zd.get_ticket(ticket_id).await.map_err(|e| e.to_string())?;

    let cnc_map_path = crate::paths::triage_home().join("data/cnc-map.json");
    let sites = extract::load_site_map(cnc_map_path.as_path()).unwrap_or_default();
    let effective_override = if let Some(s) = site_override.clone() {
        Some(s)
    } else {
        let (entry, _) =
            extract::lookup_site(&ticket, &sites, None, None).map_err(|e| e.to_string())?;
        if entry.is_none() && !sites.is_empty() {
            let (responder_tx, responder_rx) = oneshot::channel();
            let _ = tx.send(InboxEvent::SiteInputNeeded {
                ticket_id,
                subject: ticket.subject.clone(),
                org: ticket.requester_org.clone(),
                responder: responder_tx,
            });
            responder_rx.await.unwrap_or(None)
        } else {
            None
        }
    };

    let opts_inner = InvestigateOptions {
        site_override: effective_override,
        ..opts_to_investigate(opts.clone())
    };
    let updated_at = ticket.updated_at.unwrap_or(ticket.created_at);
    let outcome = run_pipeline(ticket, opts_inner, opts.no_logs, tx.clone()).await?;
    let _ = tx.send(InboxEvent::TriageComplete {
        ticket_id,
        folder: outcome.paths.folder,
        updated_at,
    });
    Ok(())
}

fn opts_to_investigate(opts: WatcherOptions) -> InvestigateOptions {
    InvestigateOptions {
        interactive: false,
        workspace: None,
        cnc_override: None,
        site_override: None,
        anchor_override: None,
        window_minutes: opts.window_minutes,
        levels: opts.levels,
        verbose: opts.verbose,
        redact_enabled: true,
        no_llm: false,
        force: false,
        customer_history_override: None,
        memory_hits_override: None,
        followup_mode: false,
        tickets_root: None,
        allow_unscoped_fixture_logs: false,
    }
}

async fn run_pipeline(
    ticket: Ticket,
    opts: InvestigateOptions,
    no_logs: bool,
    tx: mpsc::UnboundedSender<InboxEvent>,
) -> Result<StructuredInvestigation, String> {
    let mut session = investigation::create_session(ticket.clone());
    let dd = if no_logs {
        None
    } else {
        DatadogClient::from_env().ok()
    };
    let zd = ZendeskClient::from_env().ok();
    let rubric = Rubric::load().map_err(|e| e.to_string())?;
    let dd_source: Option<&dyn DatadogSource> = dd.as_ref().map(|d| d as &dyn DatadogSource);
    let zd_source: Option<&dyn ZendeskSource> = zd.as_ref().map(|z| z as &dyn ZendeskSource);

    let (phase_tx, mut phase_rx) = mpsc::unbounded_channel();
    let ticket_id = ticket.id;
    let inbox_tx = tx.clone();
    let progress_forwarder = tokio::spawn(async move {
        while let Some(ev) = phase_rx.recv().await {
            if let Some((label, step)) = inbox_phase(ev) {
                let _ = inbox_tx.send(InboxEvent::TriagePhase {
                    ticket_id,
                    label,
                    step,
                });
            }
        }
    });

    let result = {
        let reporter = ChannelReporter { tx: phase_tx };
        pipeline::investigate_one_structured(
            ticket,
            &mut session,
            zd_source,
            dd_source,
            &rubric,
            &reporter,
            &opts,
        )
        .await
        .map_err(|e| e.to_string())
    };
    let _ = progress_forwarder.await;
    result
}

fn inbox_phase(ev: TuiEvent) -> Option<(String, u8)> {
    match ev {
        TuiEvent::PhaseStarted { phase, .. } => Some((
            pretty_phase_label(&phase).to_string(),
            phase_to_step(&phase),
        )),
        TuiEvent::PhaseDone { .. } | TuiEvent::PhaseFailed { .. } => None,
    }
}

fn phase_to_step(phase: &str) -> u8 {
    match phase {
        "customer_history" | "memory_lookup" | "evidence_intake" | "build_timeline" => 1,
        "enrichment" => 2,
        "llm_call" => 3,
        "save" => 4,
        _ => 1,
    }
}

fn pretty_phase_label(phase: &str) -> &'static str {
    match phase {
        "customer_history" => "Fetching customer history",
        "memory_lookup" => "Querying prior investigations",
        "evidence_intake" => "Reviewing evidence",
        "build_timeline" => "Building timeline",
        "enrichment" => "Querying Datadog",
        "llm_call" => "Asking LLM",
        "save" => "Writing ticket folder",
        _ => "Triaging",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::models::{Comment, TicketSummary};
    use crate::zendesk::ZendeskError;

    #[derive(Clone)]
    struct StubZendesk {
        ticket: Ticket,
        view_ids: Vec<u64>,
    }

    impl ZendeskSource for StubZendesk {
        fn get_ticket<'a>(
            &'a self,
            ticket_id: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Ticket, ZendeskError>> + Send + 'a>> {
            let ticket = self.ticket.clone();
            Box::pin(async move {
                if ticket.id == ticket_id {
                    Ok(ticket)
                } else {
                    Err(ZendeskError::TicketNotFound(ticket_id))
                }
            })
        }

        fn fetch_customer_history<'a>(
            &'a self,
            _email: &'a str,
            _limit: usize,
        ) -> Pin<Box<dyn Future<Output = Vec<TicketSummary>> + Send + 'a>> {
            Box::pin(async { Vec::new() })
        }

        fn list_view_ticket_ids<'a>(
            &'a self,
            _view_id: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>> {
            let view_ids = self.view_ids.clone();
            Box::pin(async move { Ok(view_ids) })
        }

        fn list_my_ticket_ids<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>> {
            let view_ids = self.view_ids.clone();
            Box::pin(async move { Ok(view_ids) })
        }

        fn download_attachment<'a>(
            &'a self,
            _url: &'a str,
            _dest_path: &'a Path,
            _max_bytes: u64,
        ) -> Pin<Box<dyn Future<Output = Result<(u64, String), ZendeskError>> + Send + 'a>>
        {
            Box::pin(async {
                Err(ZendeskError::AttachmentNotFound(
                    "download_attachment not used in inbox poll test".into(),
                ))
            })
        }
    }

    fn sample_ticket(ticket_id: u64, updated_at: DateTime<Utc>) -> Ticket {
        Ticket {
            id: ticket_id,
            subject: format!("Ticket {ticket_id}"),
            description: "description".into(),
            requester_org: Some("Example Org".into()),
            requester_email: Some("ops@example.com".into()),
            tags: vec![],
            created_at: updated_at - chrono::Duration::minutes(5),
            updated_at: Some(updated_at),
            comments: vec![Comment {
                author: "analyst".into(),
                body: "comment".into(),
                created_at: updated_at - chrono::Duration::minutes(4),
                is_public: true,
                attachments: vec![],
            }],
        }
    }

    fn sample_watcher_opts(state_file: &Path) -> WatcherOptions {
        WatcherOptions {
            view_id: Some(44),
            interval: 30,
            state_file: state_file.to_path_buf(),
            backfill_hours: 24.0,
            window_minutes: 15,
            levels: vec!["error".into()],
            no_logs: true,
            print_notes: false,
            verbose: false,
        }
    }

    #[tokio::test]
    async fn poll_iteration_failed_pipeline_keeps_ticket_retryable() {
        let tmp = tempfile::tempdir().unwrap();
        let state_file = tmp.path().join("watcher-state.json");
        let updated_at = DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ticket = sample_ticket(44, updated_at);
        let zd = StubZendesk {
            ticket: ticket.clone(),
            view_ids: vec![ticket.id],
        };
        let state = State {
            version: 1,
            triaged: BTreeMap::new(),
        };
        let opts = sample_watcher_opts(&state_file);
        let backfill_cutoff = updated_at - chrono::Duration::hours(1);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let runner: InboxPipelineRunner =
            Arc::new(|_, _, _, _| Box::pin(async { Err("pipeline boom".to_string()) }));

        let (new_state, view_ids) = poll_iteration_with(
            &zd,
            state,
            opts,
            backfill_cutoff,
            &HashSet::new(),
            tx,
            runner,
        )
        .await
        .expect("poll iteration should still succeed");

        assert_eq!(view_ids, HashSet::from([ticket.id]));
        assert!(
            !new_state.triaged.contains_key("44"),
            "failed spawned triage must not suppress retry eligibility"
        );
        assert!(watcher::should_triage(&ticket, &new_state, backfill_cutoff));

        let phase = rx.recv().await.expect("expected triage phase event");
        assert!(matches!(
            phase,
            InboxEvent::TriagePhase {
                ticket_id: 44,
                ref label,
                step: 1,
            } if label == "Triaging"
        ));

        let failed = rx.recv().await.expect("expected triage failure event");
        assert!(matches!(
            failed,
            InboxEvent::TriageFailed {
                ticket_id: 44,
                ref error,
            } if error == "pipeline boom"
        ));
    }
}
