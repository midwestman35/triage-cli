# Refresh the CNC site map

> **When to use this:** a new APEX customer onboarded, the customer-to-`site_name` mapping changed, or you spotted a row in `data/cnc-map-gaps.md` that you can now fill in.

The site map at `data/cnc-map.json` is generated from `apex-cnc-inventory.md` by `scripts/build_cnc_map.py`. There is no `confluence.py` in this repo by design — the inventory file is the source of truth and is refreshed manually.

## Steps

1. **Open the inventory in your editor:**

   ```bash
   $EDITOR apex-cnc-inventory.md
   ```

2. **Pull the missing details from Confluence out-of-band.** In your Claude.ai chat (the one with the Confluence connector enabled), do:
   - Search the OP Confluence space for the missing site's "APEX Overview" page.
   - Read off the three fields you need: Site Name, Friendly Name, CNC UUID.
   - Append a row to the appropriate table in `apex-cnc-inventory.md`. Per-site overview entries go under `## Confirmed via per-site Overview pages`. Master-table-only entries (display label only) go under `## From master "APEX Sites Description" page (display label only)`.

   The row format is four pipe-separated columns; `site_name` must be lowercase-with-hyphens (no spaces). Match the existing rows exactly.

3. **Rebuild the map:**

   ```bash
   triage-cli build-map
   ```

4. **Inspect the output:**

   ```bash
   git diff data/cnc-map.json data/cnc-map-gaps.md
   ```

   The new entry should appear in `data/cnc-map.json`, and `data/cnc-map-gaps.md` should have one fewer gap (if you filled in a previously-incomplete row).

## Verification

- `git diff data/cnc-map.json` shows the new entry as added.
- A `triage-cli triage` run against a ticket from that customer resolves to the new entry. Use `--verbose` to confirm which lookup strategy hit:

  ```bash
  triage-cli triage <ticket-id> --verbose --no-logs 2>&1 | grep "Site resolved"
  ```

## Troubleshooting

- **`build-map` runs but the new row isn't in `cnc-map.json`** — check the H2 headings in `apex-cnc-inventory.md` are spelled exactly:

  ```
  ## Confirmed via per-site Overview pages
  ## From master "APEX Sites Description" page (display label only)
  ```

  The parser keys off them. A typo silently drops the table.

- **`build-map` errors about row format** — the row probably has the wrong column count or a stray character. Each row needs exactly four pipe-separated columns matching the existing table. Don't put spaces inside `site_name`.

- **New row landed in `cnc-map-gaps.md`, not `cnc-map.json`** — the row is missing the CNC UUID or `site_name`. Both are required for the entry to be usable. Fill in the missing field and re-run `triage-cli build-map`.
