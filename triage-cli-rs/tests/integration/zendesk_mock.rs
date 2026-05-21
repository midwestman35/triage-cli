use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use triage_cli::models::{Ticket, TicketSummary};
use triage_cli::zendesk::{ZendeskError, ZendeskSource};

pub struct ZendeskFixtureClient {
    ticket: Ticket,
    view_ids: Vec<u64>,
    my_ticket_ids: Vec<u64>,
}

impl ZendeskFixtureClient {
    pub fn from_fixture(name: &str) -> Self {
        let dir = triage_cli::fixture::resolve_named(name);
        let loader = triage_cli::fixture::FixtureLoader::new(&dir).expect("fixture must exist");
        let ticket = loader
            .load_ticket()
            .expect("fixture ticket.json must parse");
        let ticket_id = ticket.id;
        Self {
            ticket,
            view_ids: vec![12345, 67890],
            my_ticket_ids: vec![ticket_id],
        }
    }

    #[allow(dead_code)]
    pub fn with_view_ids(mut self, ids: Vec<u64>) -> Self {
        self.view_ids = ids;
        self
    }

    #[allow(dead_code)]
    pub fn with_my_ticket_ids(mut self, ids: Vec<u64>) -> Self {
        self.my_ticket_ids = ids;
        self
    }
}

impl ZendeskSource for ZendeskFixtureClient {
    fn get_ticket<'a>(
        &'a self,
        ticket_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Ticket, ZendeskError>> + Send + 'a>> {
        Box::pin(async move {
            if ticket_id == self.ticket.id {
                Ok(self.ticket.clone())
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
        Box::pin(async move { vec![] })
    }

    fn list_view_ticket_ids<'a>(
        &'a self,
        view_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>> {
        Box::pin(async move {
            if self.view_ids.contains(&view_id) {
                Ok(self.view_ids.clone())
            } else {
                Err(ZendeskError::ViewNotFound(view_id))
            }
        })
    }

    fn list_my_ticket_ids<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, ZendeskError>> + Send + 'a>> {
        Box::pin(async move { Ok(self.my_ticket_ids.clone()) })
    }

    fn download_attachment<'a>(
        &'a self,
        _url: &'a str,
        _dest_path: &'a Path,
        _max_bytes: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(u64, String), ZendeskError>> + Send + 'a>> {
        Box::pin(async move {
            Err(ZendeskError::AttachmentNotFound(
                "no attachments in fixture client".into(),
            ))
        })
    }
}
