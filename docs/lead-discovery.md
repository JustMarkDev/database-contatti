# Lead discovery — discussion notes

Status: pre-implementation, open questions. Last updated 2026-09-22.

## 1. Goal

Replace aggressive legacy web scraping with a semi-automatic, AI-assisted flow that a non-technical user (dad) can drive free-form.

He needs fresh emails for marketing campaigns. Lead types are structurally similar to the existing `category / subcategory` taxonomy in this app, but we do NOT want to limit v1 to a single pair. Support all pairs plus open/new labels.

Core value hypothesis: a personal teacher address in plain text tied to a role (e.g. `referente bullismo / cyberbullismo`) is worth far more than a generic `info@`, `segreteria@`, `@istruzione.it`. Fewer, sharper sends to e.g. "all cyberbullying referents" beat bulk blasts.

## 2. Non-goals / explicit user decisions

- Free-form input required. No technical query builder. Example: "dammi tutti i referenti bullismo della Lombardia" must resolve to filters + taxonomy.
- Legality review deferred by user. Manual opt-out handling at first, automated later.
- PEC addresses stay saved in DB (do not throw away), filtered out only at `.txt` export time. Campaign sends target PEO, not PEC.
- Budget cap: ~100 EUR/month combined for System One (Jev/TypeSafe) + System Two (reasoning agent, e.g. Muse Spark 1.3 via OpenCode subscription).

## 3. Sources (no scraping first)

1. **Current DB rescore:** reuse existing contacts + `contact_sources`. Add Jev lead-quality score so old generic addresses rank lower than role-specific ones.
2. **MIUR Open Data Anagrafe Scuole:** bulk CSV/JSON per school year, IODL 2.0. Fields: `IndirizzoEmailScuola`, `IndirizzoPecScuola`, `SitoWebScuola`, codice meccanografico, denominazione. Statali + paritarie. Free, versioned, no bot issues.
3. **IPA Open Data:** `amministrazioni.txt`, `Enti`, `Elenco PEC`. Fields: `mail1..mail5 + tipo_mail`, `sito_istituzionale`, CF, codice IPA. Continuous updates. Bulk download, no auth.
4. **Targeted fetch only for gaps:** `SitoWebScuola` / official `.gov.it` / `comune.*.it` pages, circolari PDF where role emails live. This is the only place an AI browser agent should run.

Open taxonomy labels can seed new crawl targets, but bulk sources remain primary.

## 5. Proposed cascade (System One + System Two)

Mirror existing `deterministic -> Jev -> Muse Spark` enrichment:

- **Layer 0 (free):** bulk import MIUR/IPA, normalize, dedupe on case-insensitive key, keep source rows.
- **Layer 1 (free):** deterministic rules: `*.istruzione.it`, `comune.*.it`, exact role tokens (`bullismo`, `cyberbullismo`, `referente`, `dirigente`, `dsga`), codici scuola.
- **Layer 2 (Jev, batched):** cheap judgments over same state:
  - `Choice`: map free-form request -> `category / subcategory` + geo.
  - `Noul`: `is_target_role?`, `is_personal_vs_generic?`, `is_stale?`
  - `Score`: lead quality / specificity for ranking. Reusable as sort weight without re-inference.
  - Batch up to 24 contacts/request, category-only first, paid subcategory only on high-value medium-confidence segments (current threshold pattern: category conf >= 0.8, gap noul >= 0.55).
- **Layer 3 (System Two, capped):** reasoning agent only for high-value gaps, e.g. "this school lists a new referente but no email, check its site". Max 4 contacts/batch, max 2 batches parallel, retry on same model without recalling Jev (current pattern).

Jev reference pricing at time of writing: $0.042/1M input tokens, output free (~$0.015 per 1k short decisions). 40k schools x ~400 tokens ~= $0.67 per relevance pass. System Two dominates cost/latency, hence cap.

Measured 2026-09-22 (`bun run test:cascade`, LIMIT=16, escalation opencode/muse-spark-1.3-contributor, key from app settings): Jev 2 calls, 12,125 input tokens (~758/contact), est. $0.00051 total (~$0.032/1k contacts), RTT mean 534ms, escalation rate 0% on sample. Category fill/replace 100%, subcategory fill 87.5%.

Subscription (OpenCode / Pi) is subsidized but rate-limited; direct API is predictable but metered. Design must run inside 100 EUR/mo by keeping System Two to gaps only. Real figures to come from `bun run test:cascade` (separate fill/replace + geo metrics).

## 6. UX sketch for dad

Input: single free-text field + region/school-year pickers with sane defaults.
System: Jev parses intent -> shows back "Ho capito: Referente Bullismo, Lombardia, solo email personali" with Edit / Confirm.
Output: ranked list with quality score, source link, date, Export `.txt` (PEO only, deduped, blacklist-suppressed).

No taxonomy IDs exposed. Category/subcategory remain internal mapping.

## 7. Data + app invariants to preserve

- Keep Tauri command names, arg shapes, TS types, tests in sync across Rust/frontend.
- Schema changes as versioned, transactional, idempotent migrations. Fresh DB + upgrade covered. Never touch `PRAGMA user_version` manually.
- Keep list paginated (100 rows), exports streaming, stale-async guards.
- Blacklist atomic: insert + candidate suppression + exportable projection in one transaction. PEC filtered at export, not deleted. Manual removals = opt-out list for now.
- `data/email/` is the local-only private corpus (gitignored), never committed nor copied into `src/`, `public/`, `src-tauri/`. New tests use `utente@example.test`.
- Narrowest capability rule for any new native network/file access, with test for that path.

## 8. Open questions for implementation discussion — decisions 2026-09-22

1. Lead score threshold: **show all, ranked, with slider** (no hard cutoff). Default sort by score desc, slider filters client-side.
2. First bulk: **deferred**. V1 = UI prototype + rescore of current DB + one tiny real-world experiment (5-10 schools). No MIUR/IPA bulk import yet.
3. Crawl depth: **as deep as necessary**, including `contatti` pages + circolari PDFs for role names. Polite limits still apply (single connection, per-domain throttle, pause/resume, source URL + date stored).

Remaining for v1 scope: experiment seed list, score v0 weights, export `.txt` contract.

## 10. PDF extraction live (2026-09-23)

`pdf-extract` 0.12.1 (MIT, pure Rust, no system deps) added to `src-tauri/Cargo.toml` with user approval. Circolari PDFs are fetched under a per-domain budget (default 20, 8 MB cap, magic check, 1.5s pacing shared with HTML), text-extracted locally, and ingested through the normal email/score path with the PDF URL as source. Image-only scans are skipped as non-fatal errors. Live smoke on the 3 Lombardia seeds: 50 documents (39 HTML + 11 PDF), 13 leads (was 10 HTML-only), 12 PDFs linked, 1 scan-only skipped, 1 hard 403, no throttling.

### PDF extractor replaced (2026-09-25)

`pdf-extract` pulled in `ttf-parser` (RUSTSEC-2026-0192, unmaintained) through `lopdf` 0.42. Text is now extracted by `discovery::pdf_text` on top of `lopdf` 0.45 (MIT, default features off), which no longer depends on `ttf-parser` or `rand`. `lopdf`'s own `extract_text` split addresses drawn in styled or kerned pieces, so `pdf_text` lays text out from glyph positions (simple-font `/Widths`, average width for composite fonts), follows form XObjects (stamped/registered documents), skips pages that fail to decode and bounds every stream at 64 MB decompressed. On 12 public school circolari it extracted the same emails, roles and person names as `pdf-extract`, 2–4× faster; it drops 8 crates from the lockfile.

## 11. Crawl politeness live (2026-09-23)

Before touching a host the crawler reads its `robots.txt` (RFC 9309). It uses the `DatabaseContatti` group if one exists, otherwise `*`, with `*` / `$` patterns and longest-match-wins. A 4xx `robots.txt` means crawl freely. A 5xx, 429 or unreachable one skips the host. `Crawl-delay` raises the 1.5s pause, and a host asking for more than 30s is skipped.

The sitemap comes from `robots.txt`, else `/sitemap.xml`, else `/wp-sitemap.xml`, at most 5 files per host. Its role and department URLs (`bullismo`, `referen`, `struttura`, `organizz`) go into the queue right after the seed, so the 20-page budget reaches them without link-walking.

Throttle answers (429, 503, 509) wait for `Retry-After` (seconds or HTTP-date, default 10s) and retry once. A wait over 120s skips the host. The User-Agent is `DatabaseContatti/0.1 (+https://github.com/JustMarkDev/database-contatti)`.

Live smoke after the change, same 3 Lombardia seeds: 55 documents (was 50), 15 leads (was 13), 20 PDFs, the same single 403, 234s.

## 12. Next: open sources first, search on top (planned)

Target flow: free-form request ("tutti i referenti bullismo d'Italia") -> Jev maps it to role + geography -> bulk open data gives every matching school with its site and geography -> discovery finds role emails per school -> TinyFish Search fills schools where the site or sitemap has no role page.

- Seeds come from MIUR Anagrafe Scuole (see §3), not typed URLs. Every lead keeps its school's `comune / provincia / regione`, the same fields as `contacts`. No street addresses.
- Search provider: TinyFish Search and Fetch, free with limits of 30 searches/min and 150 fetched URLs/min, called directly with our own API key. [Monid](https://monid.ai/blog/tinyfish) wraps the same free API for coding assistants. It adds a second intermediary and isn't built for app integration, so we skip it.
- Search and Fetch send the queried URL and search text to TinyFish's servers. They see which public school pages we read, not the contacts database. Document this in the README when it ships.
- Scale: about 8,000 state istituti principali (plus paritarie) at ~80s each, measured above, means crawling needs parallelism across hosts (still one connection per host) and a persistent, resumable queue.

## 13. Source catalog live (2026-09-23)

`lead_sources.rs` holds a static catalog. Four entries import today: MIUR scuole statali, MIUR paritarie, IndicePA (organizations, offices, office heads) and ANA sezioni (section 17). ICCU biblioteche, RUNTS and CEI parrocchie are listed without an importer. Import downloads the current file (resource URL from the MIUR listing or the IPA CKAN API), maps rows to category, subcategory, organization, role and geography, and writes a normal sandbox run (migration 48 adds `source_id` and those columns). Geography goes through the gazetteer, so names match `contacts`.

Live run: statali 49,395 rows to 7,707 leads in 3s, paritarie 15,466 to 10,615 in 1s, IndicePA 273,933 to 145,371 in 4s. Every lead has a regione. Buildings share their institute mailbox, hence the drop for statali. MIUR leaves out Trento, Bolzano and Aosta.

Measured against the current contacts DB, the three sources hold 156,164 unique addresses (99,468 PEO). 44,285 PEO are not in the DB yet: 32,764 public administration (21,230 comuni), 5,993 schools, 2,921 health, 241 culture. IndicePA office heads add 53,631 addresses, 27,843 new, almost all `nome.cognome`.

## 14. Search and fetch providers (2026-09-23)

Everything must run unattended for days, so a browser per page is out. Our reqwest crawler stays the primary fetcher: no cost, no quota, polite by design.

| Provider                         | Free limit                              | Use                                                                              |
| -------------------------------- | --------------------------------------- | -------------------------------------------------------------------------------- |
| TinyFish Search                  | 30/min                                  | Primary search. Highest free limit found.                                        |
| TinyFish Fetch                   | 150/min                                 | Only for pages that need JavaScript.                                             |
| Jina Reader                      | 20 RPM keyless, 500 RPM with free key   | Fallback for JavaScript pages.                                                   |
| Exa / Tavily                     | about 1,000 per month                   | Search fallback only.                                                            |
| Common Crawl                     | CDX index rate-limited, WARC under 10/s | Fallback for hosts that block us. Never touches the site, but monthly snapshots. |
| Brave Search                     | no free plan, card required             | Skip.                                                                            |
| Google Custom Search             | closed to new users, ends 2027          | Skip.                                                                            |
| SearXNG                          | self-hosted, upstream engines block it  | Skip.                                                                            |
| Firecrawl / Crawl4AI self-hosted | free                                    | Skip. They run Playwright, which is the browser we want to avoid.                |

## 15. TinyFish wired in (2026-09-23)

`tinyfish.rs` holds both calls. The key lives in `app_settings` (`tinyfish_api_key`) or `TINYFISH_API_KEY`, and without it nothing changes.

- **Trova siti** sends the request text to Search (`location=IT`, `language=it`, social and directory domains excluded) and fills the seed box with up to 10 URLs, one per host. The user reviews them before crawling.
- During a crawl, a robots-allowed page that answers 403, 406 or 451 is fetched once through Fetch (`format=html`) and parsed like any other page. The run's error list notes each page read this way. Throttling (429/503) still waits for `Retry-After`, and robots.txt exclusions are never routed through TinyFish.

## 16. Open questions from the field audit (2026-09-23)

- PEC: a single `is_pec` 0/1 flag. An address is PEC when its pattern matches (`@pec.`, legalmail, …) or when any MX host in `domain_verification_cache` belongs to a known PEC provider (Aruba PEC, Legalmail, Actalis, Poste, Telecom, Asme, …). Plain `mx.aruba.it` is ordinary mail. Migration 53 promotes the missed rows and drops `email_kind`, which had produced "standard | pec" merges.

## 17. Source field mapping, ANA importer, cleanup (2026-09-23)

- Migration 50 adds `search_leads.official_code` and `department`, with the same meaning as the `contacts` columns, so a later promotion copies fields one to one. MIUR sets `official_code` to CODICESCUOLA. IndicePA uses `cod_amm` for organization mailboxes and `cod_uni_ou` for offices. An office mailbox has `department = des_ou` and no role. `mail_resp` gets role "Responsabile", the office as department, and the head's name only when the local part contains a name token. Not stored yet: `titolo_resp`, `cod_ou_padre`, phone, social links.
- ANA: two REST pages, one text line per table cell, then a walk over the lines, which also handles Milano's nested layout. Section mailbox: organization "Sezione ANA X", comune from the address CAP. Group mailbox: department "Gruppo Y", comune only when the group or address town is a municipality of the section's region, otherwise region only. Foreign sections get no geography. Live: 117 sections, 4,509 leads (2,481 with comune), in 2 requests. 272 are not in contacts. Crawling the 133 section and group websites was tried (393 pages, 183 more addresses, 16 minutes) and left out for now: the crawl attributes every address on a site to the section, including third parties.
- Migration 51 drops `permitted_purpose`, `lawful_basis` and `consent_provenance` from `contacts` and `email_candidates`. Nothing ever wrote them.
- Migration 52 deletes the per-candidate `dns_cache` verification events (2.85M rows in the live DB, copies of `domain_verification_cache`). Import derivation now writes an event only for candidates that are not `routing_ok`. The file shrinks only after a VACUUM, since `auto_vacuum` is off.
- **Promotion** ("Aggiungi al database"): identical leads (same address and fields, e.g. a source imported twice) are saved once. A known contact keeps its `status`; a new one takes the last `domain_verification_cache` result of its domain (expired or not) and is `domain_unverified`, hence hidden from Contatti, only when the domain was never checked. `merge_status` lets `domain_unverified` win over `valid`, so promoting every lead as unverified (before migration 65) hid contacts that were already valid. Migration 65 restores them. Copied and blacklisted leads are deleted from `search_leads` in the same transaction; the filtered ids are read first because the `stato` filter reads `contacts`.

## 9. V1 experiment locked 2026-09-22

- Role: `referente bullismo / cyberbullismo` only.
- Geo: Lombardia only, 5-10 schools max from current DB rows with `website` set.
- Score v0: balanced composite as in §5. Stale `personale` does NOT auto-outrank fresh generic role mailbox; freshness stays 20%.
- UI: show all ranked + slider (default 0), export `.txt` PEO-only. Ranking is now global and server-side (`list_search_leads`): the backend applies `scoreLead` plus the small query-relevance bonus and returns order, score, and factors, so stale `personale` addresses cannot outrank fresher generic ones on confidence alone.
- Discovery v1 (2026-09-22): **Cerca lead** starts a discovery run from 1–10 seed URLs and reads leads from a sandbox table (`start_search_run` / `list_search_leads` / `export_search_run_txt`); the contacts DB is never read or mutated by this flow.

## 18. dati.gov.it as one-click sources (2026-10-05)

The manual dati.gov.it search is gone. Six catalog entries (`ckan-turismo`, `ckan-associazioni`, `ckan-cultura`, `ckan-sanita`, `ckan-formazione`, `ckan-pa`) import hand-picked tables from the national catalog, one click per taxonomy category. `ckan_bundle` in `lead_sources.rs` lists, per table, the CKAN dataset name, the publisher's region (used when the table has no region column) and the subcategory. The resource URLs come from one `package_search` per source (dati.gov.it answers 502 under bursts, so it gets one call per source, with retries), so a republished file keeps working; a table that fails is skipped and the import fails only if nothing came in. Check that every pinned table still downloads: `cargo test --manifest-path src-tauri/Cargo.toml lead_sources_live_ckan_tables -- --ignored --nocapture`. Left out on purpose: Sicilian `strutture-sanitarie` (private doctors' practices under personal names), the Lazio cultural institutes file (HTTP 500), hotels, and anything about parishes (dati.gov.it has none, OSM and Wikidata cover them).

## 19. Free-form request (2026-10-06)

The "Cosa vuoi trovare?" box sends the text to `plan_lead_request`: the model returns category, subcategory, region, catalog source ids and a web query, each checked against the taxonomy, `REGIONS` and the catalog (`parse_plan`). The page then imports or explores the chosen sources, reads the already collected sites that match category, subcategory and region (`ExploreRequest` filters), and runs the TinyFish search when a key is set. A step that fails is reported and the others go on. "Importa tutto" in Dati aperti imports every registry that publishes emails.
