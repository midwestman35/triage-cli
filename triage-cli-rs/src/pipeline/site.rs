use crate::extract::{self, SiteStrategy};
use crate::llm;
use crate::models::{SiteEntry, Ticket};

use super::PipelineError;

/// Resolve site with LLM fallback when the deterministic chain returns no_match.
pub async fn resolve_site(
    ticket: &Ticket,
    sites: &[SiteEntry],
    cnc_override: Option<&str>,
    site_override: Option<&str>,
    verbose: bool,
) -> Result<(Option<SiteEntry>, SiteStrategy), PipelineError> {
    let (entry, strategy) = extract::lookup_site(ticket, sites, cnc_override, site_override)?;
    if entry.is_some() {
        return Ok((entry, strategy));
    }
    if verbose {
        eprintln!("Site lookup: no_match — asking LLM to identify site");
    }
    let llm_name = match llm::extract_site(ticket, sites, None).await {
        Ok(n) => n,
        Err(e) => {
            if verbose {
                eprintln!("LLM site extraction failed: {e}");
            }
            return Ok((None, SiteStrategy::NoMatch));
        }
    };
    let Some(name) = llm_name else {
        return Ok((None, SiteStrategy::NoMatch));
    };
    let (entry2, _) = extract::lookup_site(ticket, sites, None, Some(&name))?;
    Ok((entry2, SiteStrategy::SiteSubstring))
}
