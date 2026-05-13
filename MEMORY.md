# Investigation Memory

<!-- Append-only by the tool. Delete entries to prune. Search index rebuilds automatically. -->

---
id: 12345
customer: unknown
date: 2026-05-11T19:07:33+00:00
subject: Audio drops on dispatch console
symptom: Caller audio drops after console reboot.
assessment: [stub] No LLM call. 2 timeline event(s).
resolution: [unknown]
---

---
id: 12345
customer: unknown
date: 2026-05-11T19:09:32+00:00
subject: Audio drops on dispatch console
symptom: Caller audio drops after console reboot.
assessment: [stub] No LLM call. 2 timeline event(s).
resolution: [unknown]
---

---
id: 44688
customer: cfung@dps.state.nv.us
date: 2026-05-12T16:06:42+00:00
subject: [44688][nvdps]-SITE ID: DA0B
symptom: Hello,


While landline with a caller for APEX ID 27491819, a unit keyed up with traffic and I placed the caller on hold to answer the unit. I tried keying up for the unit multiple times but did not hear anything from the unit and also noticed that the caller disconnected. I ended up calling the unit on his phone and he advise he could not hear me key up and I could not hear him key up APEX ID 27491855.

This was at Console 4 and the radio traffic on my end can be heard from 07:36:05 to 07:36:43
assessment: [stub] No LLM call. 3 timeline event(s).
resolution: [unknown]
---

---
id: 44688
customer: cfung@dps.state.nv.us
date: 2026-05-12T16:17:25+00:00
subject: [44688][nvdps]-SITE ID: DA0B
symptom: Hello,


While landline with a caller for APEX ID 27491819, a unit keyed up with traffic and I placed the caller on hold to answer the unit. I tried keying up for the unit multiple times but did not hear anything from the unit and also noticed that the caller disconnected. I ended up calling the unit on his phone and he advise he could not hear me key up and I could not hear him key up APEX ID 27491855.

This was at Console 4 and the radio traffic on my end can be heard from 07:36:05 to 07:36:43
assessment: Console 4 lost two-way audio on the Allied Channel radio path (APEX ID 27491855) while a landline call (APEX ID 27491819) was simultaneously on hold, pointing to a hold-state media-resource or RTP audio-path conflict in APEX that blocked PTT keying on that specific channel/console combination. No Datadog logs are available to confirm or rule out a server-side signaling or media error.
resolution: [unknown]
---
