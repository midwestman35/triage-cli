use crate::datadog::DatadogSource;
use crate::models::{InvestigationSession, Ticket};
use crate::playbook::Rubric;
use crate::zendesk::ZendeskSource;

use super::options::InvestigateOptions;
use super::reporter::Reporter;

pub struct Clients<'a> {
    pub zendesk: Option<&'a dyn ZendeskSource>,
    pub datadog: Option<&'a dyn DatadogSource>,
}

pub struct PhaseCtx<'a> {
    pub ticket: &'a Ticket,
    pub session: &'a mut InvestigationSession,
    pub opts: &'a InvestigateOptions,
    pub rubric: &'a Rubric,
    pub reporter: &'a dyn Reporter,
    pub clients: Clients<'a>,
    pub levels: Vec<String>,
}
