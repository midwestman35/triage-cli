# Implementation 
1. Add a flag and utilize LLM-extracted timestamp from the ticket body.
2. These can be found by mapping the CNCs found in the triage-cli directory at /Users/envelazquez/Documents/triage-cli
3. Let's hold off on this, we might be using a provider like codex for this for usage limits. But for a working prototype, lets wire in claude-sonnet-4-6. Stay model-agnostic.
4. Yes - for now. On my working model. In production, this should be user limited input i think but I will do some more evaluations as time goes on.
5. Add -levels now


***TO NOTE

The working directory for this project is as stated above. /Users/envelazquez/Documents/triage-cli
