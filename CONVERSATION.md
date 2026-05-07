# triage-cli: Planning Conversation Transcript

Reference material for Claude Code. The operative document is `HANDOFF.md`. If this transcript and the handoff prompt conflict, the handoff prompt wins. This exists to answer "why did we decide X."

---

## Origin

User uploaded a PDF (`noc-agent-brainstorm.pdf`, May 6 2026) covering career transition planning and a five-concept agent architecture for NOC tooling at Axon (Carbyne APEX team, NG911/E911 SaaS operations). User asked to evaluate the ideas and start scoping a single-user mockup of the first concept (Triage Intelligence Agent).

## Evaluation of the source PDF

Five agent concepts in the doc:
1. Triage Intelligence Agent (selected as starting point)
2. Backlog Intelligence Agent
3. Shift Handoff Agent
4. Alert Reasoning Layer
5. Post-Incident Knowledge Capture

Concept 1 was selected because it has bounded scope, clear success criteria, and benign failure mode (a bad internal note is ignored, not destructive).

Issues raised against the source PDF that shaped the v1 plan:

- **Pattern matching against past incidents (INC-2047 example) is wishful thinking.** Requires a vector store of resolved tickets with clean resolution notes, which doesn't exist. Cut from v1.
- **15-min Datadog window ± ticket creation timestamp is wrong.** Customer-reported tickets often lag the actual incident by hours. The window should be derived from ticket content where possible, not creation time alone.
- **Confidence scoring as written is hallucination.** "CONFIDENCE: Medium" in the sample output is the LLM making up a calibration it doesn't have. Either ground confidence in something measurable or omit. Cut from v1.
- **`agent-triaged` tag as a single-bit lock is fragile.** New comments, status changes, and updates aren't handled. Acceptable limitation for a mockup; flagged for later.
- **AWS Lambda framing in the doc is premature.** For a single-user CLI running locally, deployment is noise. Build as a Python script. Containerize later if it earns it.

## Scoping decisions

User answered three scoping questions:

| Question | Answer |
|---|---|
| Mockup scope | CLI first; watcher only if value and consistency established |
| Output destination | Terminal only; Zendesk internal note only after results are trusted |
| Datadog scope | Pull logs by CNC from ticket content |

Follow-up answers:

| Question | Answer |
|---|---|
| Language | Python |
| Project name | `triage-cli` |
| CNC scrape pattern | Both: subcommand exists, file committed as starting point |
| CNC mapping file format | JSON (machine-only) |

User explicitly removed two adjacent projects (NocLense, AutoLense) from context, stating they may be redundant given familiarity with LLMs and agent workflows. Do not reference them in the build.

## Python vs TypeScript rationale

Chose Python because:
- `httpx`/`requests` plus the `anthropic` SDK plus `click`/`typer` puts the whole pipeline in roughly 200 lines
- Datadog's official Python SDK (`datadog-api-client`) has more usage and better docs than its TypeScript counterpart
- Standard library is stronger for log parsing, regex, and datetime math, all of which this tool does
- If this ever moves to Lambda or EventBridge, Python is the path of least resistance
- Pydantic gives most of the type safety TS would otherwise provide

TypeScript would only have won if a web UI were imminent or if the user lived in Node already.

## CNC mapping mechanism

User's plan: use the Claude Confluence connector to scrape CNC mappings into a local file the CLI references. Example shape provided by user:

> In Zendesk the customer with friendly name `Nevada Department of Public Safety` uses the site name `us-nv-nvdps-apex` and the cnc to be mapped to it in datadog is `de9ee414-da5a-471d-bac2-10643190da0b`

Three fields per entry: friendly name, site name, CNC UUID. JSON format selected.

The scrape is an out-of-band step done interactively in Claude with the Confluence connector. The committed file is the starting point. A subcommand (`triage-cli refresh-cnc`) exists for future automation but is not the v1 mechanism for getting the file populated.

## Station-level log granularity

Flagged by user as a known future requirement (real triage often needs station-level filtering, not just CNC-level). Explicitly out of scope for v1. v1 pulls all logs for the CNC in the time window. Station filtering is a v2 problem.

## Auth and licensing notes

- Zendesk API token under user's name means agent actions are logged as user. Acceptable for personal CLI; relevant if this graduates to a watcher.
- Anthropic API: assumed personal/user-controlled key for v1. If this becomes a scheduled watcher running on Axon time, that's a conversation with Danielle.
- Datadog: direct API, not via any other internal tool. User has API key + app key already (assumed; confirm in setup).

## Open question deferred to Claude Code

Confluence page parseability: it's not yet confirmed whether a clean machine-parseable CNC mapping page exists. The user's plan assumes it can be assembled via the Claude connector. The build should not block on this; the JSON file can be hand-populated initially and the scrape subcommand can be a stub that prints "not yet implemented" until the source page is identified.

## Output schema decided for v1

Four sections, dropped Priority and Confidence:

1. **Summary** — what the ticket says, in two sentences
2. **Log signals** — what the CNC's logs show in the relevant window, factual
3. **Likely cause** — LLM inference, explicitly marked as inference
4. **Suggested first action** — what to check first

Disagreement between sections 2 and 3 is itself signal.

## Project structure agreed

```
triage-cli/
├── pyproject.toml
├── README.md
├── .env.example
├── triage_cli/
│   ├── __init__.py
│   ├── cli.py
│   ├── zendesk.py
│   ├── datadog.py
│   ├── confluence.py
│   ├── extract.py
│   ├── llm.py
│   ├── render.py
│   └── models.py
├── data/
│   └── cnc-map.json
└── tests/
    ├── fixtures/
    └── test_extract.py
```

`data/cnc-map.json` is committed (starting point). `cache/` is not used in v1; the committed file is the lookup source.

## Explicit non-goals for v1

- Not posting anywhere (terminal only)
- Not running on a schedule
- Not pattern-matching against past incidents
- Not deduplicating tickets
- Not station-level log filtering
- Not handling ticket updates after first triage
- Not touching anything besides Zendesk read, Datadog read, and Anthropic API
