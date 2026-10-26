# Lead discovery — discussion notes

Status: shipped. Since 2026-10-25 every page is read and every new site is found through context.dev (§20); sections marked _Superseded_ record the earlier design. Last updated 2026-10-25.

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

_Superseded 2026-10-25:_ the open data below (MIUR, IndicePA, ANA, ICCU, OSM, Wikidata, dati.gov.it) was imported once and the importers are removed; their leads and seeds stay in the sandbox. New sites now come from the discovery channels of §20.

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

_Superseded 2026-10-25:_ PDFs are read by context.dev Scrape as Markdown with OCR off, only for Istruzione seeds and only when the link names a role or a circolare (0 by default, at most 2 per site). The local extractor, `lopdf` and `flate2` are removed.

`pdf-extract` 0.12.1 (MIT, pure Rust, no system deps) added to `src-tauri/Cargo.toml` with user approval. Circolari PDFs are fetched under a per-domain budget (default 20, 8 MB cap, magic check, 1.5s pacing shared with HTML), text-extracted locally, and ingested through the normal email/score path with the PDF URL as source. Image-only scans are skipped as non-fatal errors. Live smoke on the 3 Lombardia seeds: 50 documents (39 HTML + 11 PDF), 13 leads (was 10 HTML-only), 12 PDFs linked, 1 scan-only skipped, 1 hard 403, no throttling.

### PDF extractor replaced (2026-09-25)

`pdf-extract` pulled in `ttf-parser` (RUSTSEC-2026-0192, unmaintained) through `lopdf` 0.42. Text is now extracted by `discovery::pdf_text` on top of `lopdf` 0.45 (MIT, default features off), which no longer depends on `ttf-parser` or `rand`. `lopdf`'s own `extract_text` split addresses drawn in styled or kerned pieces, so `pdf_text` lays text out from glyph positions (simple-font `/Widths`, average width for composite fonts), follows form XObjects (stamped/registered documents), skips pages that fail to decode and bounds every stream at 64 MB decompressed. On 12 public school circolari it extracted the same emails, roles and person names as `pdf-extract`, 2–4× faster; it drops 8 crates from the lockfile.

## 11. Crawl politeness live (2026-09-23)

_Superseded 2026-10-25:_ the local `robots.txt` check, pauses, `Crawl-delay`, `Retry-After` handling, the polite client and the direct sitemap fetch are removed. We assume context.dev respects `robots.txt` (not documented; to be confirmed with support@context.dev). If it does not, restore `discovery/robots.rs` from git.

Before touching a host the crawler reads its `robots.txt` (RFC 9309). It uses the `DatabaseContatti` group if one exists, otherwise `*`, with `*` / `$` patterns and longest-match-wins. A 4xx `robots.txt` means crawl freely. A 5xx, 429 or unreachable one skips the host. `Crawl-delay` raises the 1.5s pause, and a host asking for more than 30s is skipped.

The sitemap comes from `robots.txt`, else `/sitemap.xml`, else `/wp-sitemap.xml`, at most 5 files per host. Its role and department URLs (`bullismo`, `referen`, `struttura`, `organizz`) go into the queue right after the seed, so the 20-page budget reaches them without link-walking.

Throttle answers (429, 503, 509) wait for `Retry-After` (seconds or HTTP-date, default 10s) and retry once. A wait over 120s skips the host. The User-Agent is `DatabaseContatti/0.1 (+https://github.com/JustMarkDev/database-contatti)`.

Live smoke after the change, same 3 Lombardia seeds: 55 documents (was 50), 15 leads (was 13), 20 PDFs, the same single 403, 234s.

## 12. Next: open sources first, search on top (planned)

_Superseded 2026-10-25:_ search runs through context.dev (§20); TinyFish is removed.

Target flow: free-form request ("tutti i referenti bullismo d'Italia") -> Jev maps it to role + geography -> bulk open data gives every matching school with its site and geography -> discovery finds role emails per school -> TinyFish Search fills schools where the site or sitemap has no role page.

- Seeds come from MIUR Anagrafe Scuole (see §3), not typed URLs. Every lead keeps its school's `comune / provincia / regione`, the same fields as `contacts`. No street addresses.
- Search provider: TinyFish Search and Fetch, free with limits of 30 searches/min and 150 fetched URLs/min, called directly with our own API key. [Monid](https://monid.ai/blog/tinyfish) wraps the same free API for coding assistants. It adds a second intermediary and isn't built for app integration, so we skip it.
- Search and Fetch send the queried URL and search text to TinyFish's servers. They see which public school pages we read, not the contacts database. Document this in the README when it ships.
- Scale: about 8,000 state istituti principali (plus paritarie) at ~80s each, measured above, means crawling needs parallelism across hosts (still one connection per host) and a persistent, resumable queue.

## 13. Source catalog live (2026-09-23)

`lead_sources.rs` holds a static catalog. Four entries import today: MIUR scuole statali, MIUR paritarie, IndicePA (organizations, offices, office heads) and ANA sezioni (section 17). ICCU biblioteche, RUNTS and CEI parrocchie are listed without an importer. Import downloads the current file (resource URL from the MIUR listing or the IPA CKAN API), maps rows to category, subcategory, organization, role and geography, and writes a normal sandbox run (migration 48 adds `source_id` and those columns). Geography goes through the gazetteer, so names match `contacts`.

Live run: statali 49,395 rows to 7,707 leads in 3s, paritarie 15,466 to 10,615 in 1s, IndicePA 273,933 to 145,371 in 4s. Every lead has a regione. Buildings share their institute mailbox, hence the drop for statali. MIUR leaves out Trento, Bolzano and Aosta.

Measured against the current contacts DB, the three sources hold 156,164 unique addresses (99,468 PEO). 44,285 PEO are not in the DB yet: 32,764 public administration (21,230 comuni), 5,993 schools, 2,921 health, 241 culture. IndicePA office heads add 53,631 addresses, 27,843 new, almost all `nome.cognome`.

## 14. Search and fetch providers (2026-09-23, revised 2026-10-25)

The reqwest crawler, TinyFish, the Wayback Machine and Common Crawl are replaced by context.dev, one paid API for reading pages (Scrape, HTML), listing a site's URLs (Map, from its sitemap), web search and the Google Maps parser. Extracting leads stays local and free: `parse_page`, `ingest_page`, `host_leads` and the optional OpenCode `LeadAi`. context.dev's own `json`, Answers and Extract outputs would cost more than three times as much and duplicate the model we already pay for.

## 15. TinyFish wired in (2026-09-23)

_Superseded 2026-10-25:_ `tinyfish.rs`, its commands and the TinyFish dev tooling are removed. Migration 66 deletes the stored `tinyfish_api_key`, which is no longer listed among the secrets kept out of backups.

## 16. Open questions from the field audit (2026-09-23)

- PEC: a single `is_pec` 0/1 flag. An address is PEC when its pattern matches (`@pec.`, legalmail, …) or when any MX host in `domain_verification_cache` belongs to a known PEC provider (Aruba PEC, Legalmail, Actalis, Poste, Telecom, Asme, …). Plain `mx.aruba.it` is ordinary mail. Migration 53 promotes the missed rows and drops `email_kind`, which had produced "standard | pec" merges.

## 17. Source field mapping, ANA importer, cleanup (2026-09-23)

_Partly superseded 2026-10-25:_ the ANA importer is removed with the other importers. The migrations, columns and promotion rules below stay.

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

_Superseded 2026-10-25:_ the dati.gov.it bundles are removed with the other importers.

## 19. Free-form request (2026-10-06)

The "Cosa vuoi trovare?" box sends the text to `plan_lead_request`: the model returns category, subcategory, region, directory ids and a web query, each checked against the taxonomy, `REGIONS` and the directories (`parse_plan`). The page then explores the chosen directories, reads the already collected sites that match category, subcategory and region (`ExploreRequest` filters), runs the web search discovery (and Google Maps when enabled) and reads the new sites it accepted. A step that fails is reported and the others go on.

## 20. context.dev (2026-10-25)

**Reading a site** (`discovery::crawl_host`): Scrape the home as HTML (1 credit). If it redirects to another host, that host becomes the site; a redirect to a social network or portal stops there. When none of the home links scores above zero for the seed's category, Map the site (`maxLinks=1000`, no `search`; with `urlRegex` when the seed has a path such as `wixsite.com/nome`). Candidates are the home links plus the Map URLs on the same host, minus noise (news, calendars, albo, privacy, login, feeds, tags, pagination, files). A free local score per seed category (keywords in the URL count most, then in the link text or Map title and description; deep paths and query strings lose points) picks the pages; when no candidate scores above zero, `choose_links` picks by meaning among the first 40. Up to N-1 pages are read (N = pages per site, default 3, max 6), then more (ceiling `EXTEND_UP_TO_PAGES`, 10) while the last page read gave an address not seen before on the host; pages with no keyword are never read on their own. Cost: home 1 + (N-1) pages, plus 1 when Map runs (so 3 to 4 with N = 3); a dead or blocked site costs at most 1. Map failing or returning only the home falls back to the home links.

**Client** (`contextdev.rs`): one semaphore counts every request in flight (Free 2, Developer 10, Pro 100; a 429 above it is immediate). 429 waits `Retry-After` (max 2 retries), 408/5xx/`INTERNAL_ERROR` back off (max 2), `WEBSITE_BLOCKED` retries once, `NOT_FOUND` never. The client stops itself on 402, a rejected key, three 400s in a row or the job's credit cap; a seed that fails for those reasons stays `pending` instead of `failed`. Internal hosts and non-http URLs are never sent (a 400 can disable a Free key). The final URL after redirects is the Scrape response's top-level `url` (the schema has no `metadata.finalUrl`).

**Budget**: every job reads `GET /org/usage` (free) and refuses to start at the reserve (default 100) or with Auto-Topup on and no monthly budget. The cap is the smallest of balance minus reserve, the per-run cap of Settings (default 500) and the request's own. Runs and seeds record `credits_used` (migration 66). The 100 EUR monthly AI budget of §2 does not include context.dev.

**New sites** (`lead_candidates.rs`): every channel hands its sites to `accept_candidates`. Known sites (any seed list, a contact's site or email domain, or a parent domain of those), social networks, portals and providers are dropped. A directory's own taxonomy counts as known; otherwise the deterministic rules classify, then Jev in batches without escalation. A confident classification (>= 0.8) becomes a `pending` seed, the rest is stored as `verifica` and never read. Channels and their lists: links out of every page read (`Scoperta: link in uscita`, no extra credit, at most 40 per site), directories (`Elenco: …`), web search (`Scoperta: ricerca web`, the free query plus `{sottocategoria} {provincia}` for each province of the region) and Google Maps (`Scoperta: Google Maps`, off by default). Each discovery is a `discovery` run with its credits; the page shows credits per new address for each channel.

**Organizations known only by name** (`find_seed_sites`): Google Maps when enabled (the place's city must be the comune and the names must share a significant word), otherwise web search (title or description must name the comune or a significant word of the name). A site that does not pass waits as `verifica`, because a wrong site would attribute right addresses to the wrong organization.

**Phase 0 spike**: `contextdev_live_spike` (ignored test) measures credits per site, recall against known addresses, Map with and without `search`, deep pages, html vs markdown and cache cost, and the discovery channels, under a hard cap of 500 credits. Its Go thresholds (<= 4 credits per site, right pages in 8 of 10 manual checks, Maps precision >= 80% at <= 1.5 credits per valid site) decide which defaults to change.
