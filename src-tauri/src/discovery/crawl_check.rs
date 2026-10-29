//! Live check of the reading phase on a list of pending seeds, under a hard
//! credit cap. The sites go through the same pipeline as a real run (hosts in
//! parallel, the model on a 2-slot queue, addresses already in the database kept
//! from it). The pages read are then extracted again at other model efforts, for
//! no credit, to compare time and answers. The database is opened read-only;
//! nothing is written. The leads go to `CHECK_LEADS_OUT` (tab-separated);
//! stdout only gets counts.
//!
//! Run: `CONTEXT_DEV_API_KEY=… CONTEXT_DEV_SPIKE_DB=/path/contatti.sqlite3
//! CHECK_SITES=sites.txt CHECK_LEADS_OUT=/tmp/leads.tsv CHECK_CAP=100
//! CHECK_AI_MODEL=… CHECK_AI_EFFORT=high CHECK_COMPARE_EFFORTS=medium,low
//! cargo test --manifest-path src-tauri/Cargo.toml contextdev_live_crawl_check -- --ignored --nocapture`
//! `CHECK_AI_MODEL`/`CHECK_AI_EFFORT` are the choice of Settings.

use super::*;
use crate::ai_opencode::OpenCodeProvider;
use crate::ai_provider::{InferenceProvider, InferenceRequest, ProviderError};
use crate::contextdev::ContextDev;
use crate::lead_ai::{LeadAi, MODEL_CONCURRENCY};
use crate::lead_crawl::{CrawlJob, SeedRow, host_leads};
use futures::StreamExt;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// The real provider, counting the requests and the time spent in them.
struct Counting {
    inner: OpenCodeProvider,
    calls: Arc<AtomicUsize>,
    millis: Arc<AtomicU64>,
}

impl InferenceProvider for Counting {
    fn provider_id(&self) -> &'static str {
        self.inner.provider_id()
    }

    fn complete(
        &self,
        request: InferenceRequest,
    ) -> BoxFuture<'static, Result<String, ProviderError>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let (future, millis) = (self.inner.complete(request), self.millis.clone());
        Box::pin(async move {
            let started = Instant::now();
            let answer = future.await;
            millis.fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
            answer
        })
    }
}

struct Model {
    ai: LeadAi,
    calls: Arc<AtomicUsize>,
    millis: Arc<AtomicU64>,
}

fn model(taxonomy: &crate::lead_ai::Taxonomy, name: &str, effort: Option<&str>) -> Model {
    let (calls, millis) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicU64::new(0)));
    let inner = OpenCodeProvider::new(name, effort, std::time::Duration::from_secs(180))
        .expect("OpenCode available");
    let provider = Counting {
        inner,
        calls: calls.clone(),
        millis: millis.clone(),
    };
    Model {
        ai: LeadAi::with_provider(Arc::new(provider), taxonomy.clone()),
        calls,
        millis,
    }
}

fn known_emails(db: &Connection, host: &str) -> HashSet<String> {
    let mut statement = db
        .prepare("SELECT lower(email) FROM contacts WHERE lower(email_domain||email_tld)=?1")
        .expect("known query");
    statement
        .query_map([host.trim_start_matches("www.")], |row| {
            row.get::<_, String>(0)
        })
        .expect("known rows")
        .collect::<Result<_, _>>()
        .expect("known values")
}

/// Comparison keys among `keys` that a contact already holds (a fresh read-only connection).
fn known_keys(path: &str, keys: &[String]) -> HashSet<String> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("database opens read-only");
    let mut statement = db
        .prepare("SELECT 1 FROM contacts WHERE email_comparison_key=?1 LIMIT 1")
        .expect("known keys query");
    keys.iter()
        .filter(|key| statement.exists([key]).unwrap_or(false))
        .cloned()
        .collect()
}

fn seed_row(db: &Connection, url: &str) -> SeedRow {
    db.query_row(
        "SELECT name, category, subcategory, comune, provincia, regione, address, postal_code,
                official_code FROM search_seeds WHERE url=?1 AND status='pending' LIMIT 1",
        [url],
        |row| {
            Ok(SeedRow {
                url: Some(url.to_owned()),
                name: row.get(0)?,
                context: LeadContext {
                    organization: row.get(0)?,
                    category: row.get(1)?,
                    subcategory: row.get(2)?,
                    comune: row.get(3)?,
                    provincia: row.get(4)?,
                    regione: row.get(5)?,
                    address: row.get(6)?,
                    postal_code: row.get(7)?,
                    official_code: row.get(8)?,
                    ..LeadContext::default()
                },
            })
        },
    )
    .unwrap_or_else(|error| panic!("seed {url}: {error}"))
}

fn field(value: &Option<String>) -> String {
    value.clone().unwrap_or_default().replace(['\t', '\n'], " ")
}

struct Site {
    url: String,
    host: String,
    seed: SeedRow,
    crawl: HostCrawl,
    known: HashSet<String>,
    known_in_db: HashSet<String>,
    crawl_ms: u128,
    total_ms: u128,
    leads: Vec<LeadAccumulator>,
}

fn job() -> CrawlJob {
    CrawlJob {
        label: "check".to_owned(),
        kind: "web",
        source_id: None,
        max_pages: DEFAULT_MAX_PAGES_PER_DOMAIN,
        max_pdfs: DEFAULT_MAX_PDFS,
        directory: None,
        max_credits: 0,
        ai_model: String::new(),
        ai_effort: None,
    }
}

#[tokio::test]
#[ignore = "live context.dev + OpenCode check, spends context.dev credits; see the module docs"]
async fn contextdev_live_crawl_check() {
    let key = std::env::var("CONTEXT_DEV_API_KEY").expect("CONTEXT_DEV_API_KEY");
    let path = std::env::var("CONTEXT_DEV_SPIKE_DB").expect("CONTEXT_DEV_SPIKE_DB");
    let out = std::env::var("CHECK_LEADS_OUT").expect("CHECK_LEADS_OUT");
    let urls: Vec<String> =
        std::fs::read_to_string(std::env::var("CHECK_SITES").expect("CHECK_SITES"))
            .expect("sites file")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
    let cap: u64 = std::env::var("CHECK_CAP")
        .expect("CHECK_CAP")
        .parse()
        .expect("number");
    let name = std::env::var("CHECK_AI_MODEL").unwrap_or_default();
    let effort = std::env::var("CHECK_AI_EFFORT")
        .ok()
        .filter(|effort| !effort.is_empty());
    let compare: Vec<String> = std::env::var("CHECK_COMPARE_EFFORTS")
        .unwrap_or_default()
        .split(',')
        .filter(|effort| !effort.is_empty())
        .map(str::to_owned)
        .collect();
    let db = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("database opens read-only");
    let concurrency = 2;
    let context = ContextDev::new(key, concurrency, Some(cap)).expect("client");
    let usage = context.usage().await.expect("GET /org/usage");
    usage
        .spend_guard()
        .expect("Auto-Topup must be off or capped");
    println!(
        "CHECK balance before: {} cap={cap} sites={} context.dev in flight={concurrency} model in flight={MODEL_CONCURRENCY}",
        usage.credits_remaining,
        urls.len()
    );
    let taxonomy = crate::lead_ai::load_taxonomy(&db).expect("taxonomy");
    let geography = crate::lead_directories::Geography::load(&db).expect("geography");
    let main = model(&taxonomy, &name, effort.as_deref());
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let settings = CrawlSettings {
        context: &context,
        max_pages: DEFAULT_MAX_PAGES_PER_DOMAIN as usize,
        extend_to: EXTEND_UP_TO_PAGES,
        map_always: false,
        max_pdfs: DEFAULT_MAX_PDFS as usize,
        directory: false,
        ai: Some(&main.ai),
        cancel: &cancel,
    };
    let now = chrono::Utc::now().to_rfc3339();
    let job = job();
    let seeds: Vec<(String, SeedRow, HashSet<String>)> = urls
        .iter()
        .map(|url| {
            let seed = seed_row(&db, url);
            let known = known_emails(&db, &url_host(url).unwrap_or_default());
            (url.clone(), seed, known)
        })
        .collect();

    // The pipeline of a real run.
    let wall = Instant::now();
    let mut sites: Vec<Site> = futures::stream::iter(seeds)
        .map(|(url, seed, known_in_db)| {
            let (settings, job, ai, geography, now, path) =
                (&settings, &job, &main.ai, &geography, &now, path.as_str());
            async move {
                let started = Instant::now();
                let host = url_host(&url).unwrap();
                let crawl = if settings.context.stop_reason().is_none() {
                    let category = seed.context.category.clone();
                    crawl_host(settings, std::slice::from_ref(&url), category.as_deref()).await
                } else {
                    HostCrawl::default()
                };
                let crawl_ms = started.elapsed().as_millis();
                let site = crawl.site_host.clone().unwrap_or_else(|| host.clone());
                let keys: Vec<String> = collect_leads(&crawl.pages, now).0.into_keys().collect();
                let known = known_keys(path, &keys);
                let (leads, _, errors) = host_leads(
                    &crawl,
                    &site,
                    &seed,
                    job,
                    Some(ai),
                    geography,
                    now,
                    &known,
                    &[],
                )
                .await;
                if !errors.is_empty() {
                    println!("CHECK   {host}: model errors {errors:?}");
                }
                Site {
                    url,
                    host: site,
                    seed,
                    crawl,
                    known,
                    known_in_db,
                    crawl_ms,
                    total_ms: started.elapsed().as_millis(),
                    leads,
                }
            }
        })
        .buffer_unordered(concurrency + MODEL_CONCURRENCY)
        .collect()
        .await;
    let wall_ms = wall.elapsed().as_millis();
    sites.sort_by(|left, right| left.url.cmp(&right.url));

    let mut rows = String::from(
        "site\tmode\temail\trole\tperson\tdepartment\torganization\tcomune\tcategory\tsubcategory\tmethod\tknown_in_db\n",
    );
    let mut dump = |site: &Site, mode: &str, leads: &[LeadAccumulator]| {
        for lead in leads {
            rows.push_str(&format!(
                "{}\t{mode}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                site.host,
                lead.email(),
                field(&lead.role),
                field(&lead.person_name),
                field(&lead.context.department),
                field(&lead.context.organization),
                field(&lead.context.comune),
                field(&lead.context.category),
                field(&lead.context.subcategory),
                lead.method,
                site.known_in_db.contains(&lead.email().to_lowercase()),
            ));
        }
    };
    let (mut credits, mut pages, mut leads_total, mut same_total, mut known_total, mut recovered) =
        (0u64, 0usize, 0usize, 0usize, 0usize, 0usize);
    for site in &sites {
        let emails: HashSet<String> = site
            .leads
            .iter()
            .map(|lead| lead.email().to_lowercase())
            .collect();
        let same_site = emails
            .iter()
            .filter(|email| {
                crate::lead_crawl::same_site(email.rsplit('@').next().unwrap_or(""), &site.host)
            })
            .count();
        let paths: Vec<String> = site
            .crawl
            .pages
            .iter()
            .map(|page| {
                let path = url_path(&page.url);
                path.split(['?', '#']).next().unwrap_or("").to_owned()
            })
            .collect();
        println!(
            "CHECK {}: credits={} crawl={:.1}s total={:.1}s pages={paths:?} leads={} same_site={same_site} \
             already_known={} known_in_db={} recovered={} errors={}",
            site.host,
            site.crawl.credits,
            site.crawl_ms as f64 / 1000.0,
            site.total_ms as f64 / 1000.0,
            emails.len(),
            site.known.len(),
            site.known_in_db.len(),
            emails.intersection(&site.known_in_db).count(),
            site.crawl.errors.len(),
        );
        credits += site.crawl.credits;
        pages += site.crawl.pages.len();
        leads_total += emails.len();
        same_total += same_site;
        known_total += site.known.len();
        recovered += emails.intersection(&site.known_in_db).count();
        dump(site, "high", &site.leads);
    }
    let calls = context.log.lock().unwrap().clone();
    let (maps, scrapes): (Vec<_>, Vec<_>) = calls
        .iter()
        .filter(|call| call.kind == "map" || call.kind == "scrape")
        .partition(|call| call.kind == "map");
    let average = |list: &[&crate::contextdev::CallLog]| {
        list.iter().map(|call| call.millis).sum::<u128>() as f64 / list.len().max(1) as f64 / 1000.0
    };
    for call in maps.iter().filter(|call| call.status != 200) {
        println!(
            "CHECK   map {} status={} credits={} {:.1}s",
            call.target,
            call.status,
            call.credits,
            call.millis as f64 / 1000.0
        );
    }
    println!(
        "CHECK map URLs returned: {:?}",
        maps.iter()
            .filter_map(|call| call.items)
            .collect::<Vec<_>>()
    );
    println!(
        "CHECK PIPELINE: sites={} wall={:.1}s ({:.1}s/site) credits={credits} (context.dev counted {}) pages={pages} \
         leads={leads_total} same_site={same_total} skipped_for_model_as_known={known_total} recovered_known={recovered} \
         model_calls={} model_time={:.1}s | map: {} calls, {} timed out, avg {:.1}s | scrape: {} calls, avg {:.1}s",
        sites.len(),
        wall_ms as f64 / 1000.0,
        wall_ms as f64 / 1000.0 / sites.len().max(1) as f64,
        context.credits_used(),
        main.calls.load(Ordering::Relaxed),
        main.millis.load(Ordering::Relaxed) as f64 / 1000.0,
        maps.len(),
        maps.iter().filter(|call| call.status == 408).count(),
        average(&maps),
        scrapes.len(),
        average(&scrapes),
    );

    // Same pages, other efforts: time and agreement with the Settings effort.
    for other in &compare {
        let alt = model(&taxonomy, &name, Some(other));
        let started = Instant::now();
        let (mut same, mut total) = (0usize, 0usize);
        for site in &sites {
            let (leads, _, errors) = host_leads(
                &site.crawl,
                &site.host,
                &site.seed,
                &job,
                Some(&alt.ai),
                &geography,
                &now,
                &site.known,
                &[],
            )
            .await;
            if !errors.is_empty() {
                println!("CHECK   {} @{other}: model errors {errors:?}", site.host);
            }
            let reference: HashMap<String, &LeadAccumulator> = site
                .leads
                .iter()
                .map(|lead| (lead.email().to_lowercase(), lead))
                .collect();
            for lead in &leads {
                let Some(base) = reference.get(&lead.email().to_lowercase()) else {
                    continue;
                };
                total += 1;
                same += usize::from(
                    base.context.organization == lead.context.organization
                        && base.context.category == lead.context.category
                        && base.context.subcategory == lead.context.subcategory
                        && base.role == lead.role
                        && base.person_name == lead.person_name,
                );
            }
            dump(site, other, &leads);
        }
        println!(
            "CHECK EFFORT {other}: {:.1}s for {} sites (one at a time), {} model calls, {:.1}s in the model; \
             {same}/{total} leads identical to the {} answer in organization, category, role and person",
            started.elapsed().as_secs_f64(),
            sites.len(),
            alt.calls.load(Ordering::Relaxed),
            alt.millis.load(Ordering::Relaxed) as f64 / 1000.0,
            effort.as_deref().unwrap_or("default"),
        );
    }
    std::fs::write(&out, rows).expect("leads file");
    println!(
        "CHECK total credits={} stop={:?} balance now={:?}",
        context.credits_used(),
        context.stop_reason(),
        context.credits_remaining()
    );
    assert!(context.credits_used() <= cap + 2, "cap respected");
}

const ROLE_WORDS: [&str; 30] = [
    "info",
    "segreteria",
    "protocollo",
    "urp",
    "direzione",
    "ufficio",
    "uffici",
    "amministrazione",
    "biblioteca",
    "scuola",
    "parrocchia",
    "prolo",
    "contatti",
    "sindaco",
    "presidente",
    "pec",
    "comune",
    "anagrafe",
    "tributi",
    "ragioneria",
    "tecnico",
    "polizia",
    "cultura",
    "turismo",
    "servizi",
    "didattica",
    "dirigente",
    "personale",
    "direttivo",
    "assessor",
];

/// `role` for a function mailbox, `personal` for `nome.cognome`, `other` otherwise.
fn mailbox_kind(email: &str) -> &'static str {
    let local = email.split('@').next().unwrap_or("").to_lowercase();
    if ROLE_WORDS.iter().any(|word| local.contains(word)) {
        "role"
    } else if local.contains('.') || local.contains('_') {
        "personal"
    } else {
        "other"
    }
}

fn is_pec(email: &str) -> bool {
    let domain = email.rsplit('@').next().unwrap_or("");
    ["pec", "legalmail", "postecert", "cert"]
        .iter()
        .any(|word| domain.contains(word))
}

/// What reading 5 pages per site buys over the 3 of today (the pages are read in
/// ranked order, so the first 3 are exactly a 3-page read), and which pages
/// only the Map found (the home's own links cannot lead to them).
#[tokio::test]
#[ignore = "live context.dev check, spends context.dev credits; see the module docs"]
async fn contextdev_live_page_depth() {
    let key = std::env::var("CONTEXT_DEV_API_KEY").expect("CONTEXT_DEV_API_KEY");
    let path = std::env::var("CONTEXT_DEV_SPIKE_DB").expect("CONTEXT_DEV_SPIKE_DB");
    let urls: Vec<String> =
        std::fs::read_to_string(std::env::var("CHECK_SITES").expect("CHECK_SITES"))
            .expect("sites file")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
    let base_pages: usize = std::env::var("CHECK_BASE_PAGES")
        .map(|value| value.parse().expect("number"))
        .unwrap_or(3);
    let depth_max: usize = std::env::var("CHECK_MAX_PAGES")
        .map(|value| value.parse().expect("number"))
        .unwrap_or(5);
    let cap: u64 = std::env::var("CHECK_CAP")
        .expect("CHECK_CAP")
        .parse()
        .expect("number");
    let db = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("database opens read-only");
    let context = ContextDev::new(key, 2, Some(cap)).expect("client");
    let usage = context.usage().await.expect("GET /org/usage");
    usage
        .spend_guard()
        .expect("Auto-Topup must be off or capped");
    println!(
        "CHECK DEPTH balance before: {} cap={cap} sites={}",
        usage.credits_remaining,
        urls.len()
    );
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let settings = CrawlSettings {
        context: &context,
        max_pages: base_pages,
        extend_to: depth_max,
        map_always: std::env::var("CHECK_FORCE_MAP").is_ok(),
        max_pdfs: 0,
        directory: false,
        ai: None,
        cancel: &cancel,
    };
    let now = chrono::Utc::now().to_rfc3339();
    let seeds: Vec<(String, Option<String>)> = urls
        .iter()
        .map(|url| (url.clone(), seed_row(&db, url).context.category))
        .collect();
    let started = Instant::now();
    let probes_per_site: usize = std::env::var("CHECK_PROBES")
        .map(|value| value.parse().expect("number"))
        .unwrap_or(0);
    /// A page the local score rejected (score of zero or below), read to see what it held.
    struct Probe {
        url: String,
        label: String,
        score: i32,
        emails: Vec<String>,
    }
    let results: Vec<(String, HostCrawl, Vec<Probe>)> = futures::stream::iter(seeds)
        .map(|(url, category)| {
            let settings = &settings;
            let now = &now;
            async move {
                let crawl = if settings.context.stop_reason().is_none() {
                    crawl_host(settings, std::slice::from_ref(&url), category.as_deref()).await
                } else {
                    HostCrawl::default()
                };
                let mut probes = Vec::new();
                if let (Some(home), true) = (crawl.pages.first(), probes_per_site > 0) {
                    let host = crawl
                        .site_host
                        .clone()
                        .or_else(|| url_host(&url))
                        .unwrap_or_default();
                    let read: HashSet<String> =
                        crawl.pages.iter().map(|page| page.url.clone()).collect();
                    let anchors = page_anchors(&home.url, &home.html);
                    let rejected =
                        rank_candidates(&host, &read, &anchors, &[], category.as_deref())
                            .into_iter()
                            .filter(|(_, _, score)| *score <= 0)
                            .take(probes_per_site);
                    for (probe_url, label, score) in rejected {
                        if settings.context.stop_reason().is_some() {
                            break;
                        }
                        let Ok(page) = settings.context.scrape_html(&probe_url).await else {
                            continue;
                        };
                        let emails = collect_leads(
                            &[FetchedPage {
                                url: page.final_url,
                                html: page.content,
                                via: "contextdev",
                            }],
                            now,
                        )
                        .0
                        .into_keys()
                        .collect();
                        probes.push(Probe {
                            url: probe_url,
                            label,
                            score,
                            emails,
                        });
                    }
                }
                (url, crawl, probes)
            }
        })
        .buffer_unordered(2)
        .collect()
        .await;
    println!("CHECK DEPTH wall {:.0}s", started.elapsed().as_secs_f64());

    // depth -> (credits, leads, new leads, role mailboxes new to the db, pec new)
    #[derive(Default, Clone, Copy, Debug)]
    struct Tally {
        credits: u64,
        leads: usize,
        new: usize,
        new_role: usize,
        new_personal: usize,
        new_pec: usize,
    }
    let (mut at, mut map_only_tally) = ([Tally::default(); 11], Tally::default());
    let (mut home_only_leads, mut sites_read) = (0usize, 0usize);
    let mut reached = [0usize; 11];
    let (mut pages_from_map, mut pages_from_home) = (0usize, 0usize);
    let (mut probe_pages, mut probe_hits, mut probe_new) = (0usize, 0usize, 0usize);
    for (url, crawl, probes) in &results {
        if crawl.pages.is_empty() {
            println!(
                "CHECK DEPTH {url}: nothing read ({:?})",
                crawl.errors.first()
            );
            continue;
        }
        sites_read += 1;
        let anchors: HashSet<String> = page_anchors(&crawl.pages[0].url, &crawl.pages[0].html)
            .iter()
            .map(|(link, _)| page_key(link))
            .collect();
        let origin =
            |index: usize| index == 0 || anchors.contains(&page_key(&crawl.pages[index].url));
        let all_keys: Vec<String> = collect_leads(&crawl.pages, &now).0.into_keys().collect();
        let known = known_keys(&path, &all_keys);
        let mut seen: HashSet<String> = HashSet::new();
        let mut line = Vec::new();
        for (index, page) in crawl.pages.iter().enumerate() {
            let found = collect_leads(std::slice::from_ref(page), &now).0;
            let fresh: Vec<&String> = found.keys().filter(|key| !seen.contains(*key)).collect();
            let fresh_new = fresh.iter().filter(|key| !known.contains(**key)).count();
            seen.extend(found.keys().cloned());
            if index > 0 {
                if origin(index) {
                    pages_from_home += 1;
                } else {
                    pages_from_map += 1;
                    map_only_tally.leads += fresh.len();
                    map_only_tally.new += fresh_new;
                }
            }
            let mut path_text = url_path(&page.url);
            path_text.truncate(50);
            line.push(format!(
                "{}{path_text}[+{}/{}new]",
                if index == 0 {
                    "H "
                } else if origin(index) {
                    "a "
                } else {
                    "M "
                },
                fresh.len(),
                fresh_new
            ));
        }
        for depth in 1..=depth_max.min(10) {
            let prefix = &crawl.pages[..depth.min(crawl.pages.len())];
            let leads = collect_leads(prefix, &now).0;
            let cell = &mut at[depth];
            reached[depth] += usize::from(crawl.pages.len() >= depth);
            cell.leads += leads.len();
            for (key, lead) in &leads {
                if known.contains(key) {
                    continue;
                }
                cell.new += 1;
                match mailbox_kind(lead.email()) {
                    "role" => cell.new_role += 1,
                    "personal" => cell.new_personal += 1,
                    _ => {}
                }
                cell.new_pec += usize::from(is_pec(lead.email()));
            }
            // home, Map, then one credit per page read so far.
            cell.credits += crawl.credits - (crawl.pages.len() - prefix.len()) as u64;
        }
        let home_links: Vec<_> = crawl
            .pages
            .iter()
            .enumerate()
            .filter(|(index, _)| origin(*index))
            .map(|(_, page)| page.clone())
            .collect();
        home_only_leads += collect_leads(&home_links, &now).0.len();
        for probe in probes {
            let fresh: Vec<&String> = probe
                .emails
                .iter()
                .filter(|key| !seen.contains(*key))
                .collect();
            let keys: Vec<String> = fresh.iter().map(|key| (*key).clone()).collect();
            let known_probe = known_keys(&path, &keys);
            let new = keys
                .iter()
                .filter(|key| !known_probe.contains(*key))
                .count();
            probe_pages += 1;
            probe_hits += usize::from(new > 0);
            probe_new += new;
            let mut shown = url_path(&probe.url);
            shown.truncate(60);
            println!(
                "CHECK PROBE {} score={} label={:?} {shown}: {} addresses, {} not on the pages read, {new} new to the db",
                url_host(url).unwrap_or_default(),
                probe.score,
                probe.label.chars().take(40).collect::<String>(),
                probe.emails.len(),
                fresh.len(),
            );
        }
        println!(
            "CHECK DEPTH {} credits={} pages={}: {}",
            url_host(url).unwrap_or_default(),
            crawl.credits,
            crawl.pages.len(),
            line.join(" | ")
        );
    }
    let calls = context.log.lock().unwrap().clone();
    let maps: Vec<_> = calls.iter().filter(|call| call.kind == "map").collect();
    let map_spent: u64 = maps.iter().map(|call| call.credits).sum();
    let map_timeouts = maps.iter().filter(|call| call.status == 408).count();
    println!("CHECK DEPTH sites read={sites_read} of {}", results.len());
    for depth in 1..=depth_max.min(10) {
        let cell = at[depth];
        let before = at[depth.saturating_sub(1)];
        println!(
            "CHECK DEPTH {depth} pages ({} sites got that far): credits={} leads={} new_to_db={} (role {}, personal {}, pec {}) credits/new={:.2} | page {depth} alone: +{} credits, +{} new → {:.2} credits per extra new",
            reached[depth],
            cell.credits,
            cell.leads,
            cell.new,
            cell.new_role,
            cell.new_personal,
            cell.new_pec,
            cell.credits as f64 / cell.new.max(1) as f64,
            cell.credits - before.credits,
            cell.new - before.new,
            (cell.credits - before.credits) as f64 / (cell.new - before.new).max(1) as f64
        );
    }
    println!(
        "CHECK DEPTH map: {} calls ({} timed out), {map_spent} credits, pages reached only through Map: {pages_from_map}, through the home links: {pages_from_home}; \
         leads on Map-only pages: {} ({} new_to_db); leads from home+links only: {home_only_leads} of {} at the deepest read",
        maps.len(),
        map_timeouts,
        map_only_tally.leads,
        map_only_tally.new,
        at[depth_max.min(10)].leads
    );
    println!(
        "CHECK PROBES rejected pages read: {probe_pages}, with a new address: {probe_hits}, new addresses: {probe_new}"
    );
    println!(
        "CHECK DEPTH total credits={} stop={:?} balance now={:?}",
        context.credits_used(),
        context.stop_reason(),
        context.credits_remaining()
    );
    assert!(context.credits_used() <= cap + 2, "cap respected");
}

/// Does an address named after a seed's subcategory reach that seed on a real
/// shared site? Reads the seeds of the database (read-only), spends nothing.
#[test]
#[ignore = "reads the real database; see the module docs"]
fn shared_sites_send_a_named_address_to_its_own_seed() {
    let path = std::env::var("CONTEXT_DEV_SPIKE_DB").expect("CONTEXT_DEV_SPIKE_DB");
    let db = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("database opens read-only");
    // Mailbox word a site would use for each subcategory.
    let word = |subcategory: &str| -> Option<&'static str> {
        Some(match subcategory {
            "Biblioteca" => "biblioteca",
            "Museo" => "museo",
            "Teatro" => "teatro",
            "Parrocchia" => "parrocchia",
            "Chiesa" => "chiesa",
            "Santuario / basilica" => "santuario",
            "Oratorio / centro giovanile" => "oratorio",
            "Alpini" => "alpini",
            "Primaria" => "primaria",
            "Infanzia" => "infanzia",
            "Secondaria di primo grado" => "media",
            "Istituto comprensivo" => "comprensivo",
            _ => return None,
        })
    };
    let hosts: Vec<String> = db
        .prepare(
            "SELECT host FROM search_seeds WHERE status='pending' AND url IS NOT NULL
             GROUP BY host HAVING COUNT(DISTINCT subcategory) > 1 ORDER BY random() LIMIT 600",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let (mut right, mut wrong, mut unmoved, mut tried) = (0usize, 0usize, 0usize, 0usize);
    let mut wrong_examples = Vec::new();
    for host in hosts {
        let siblings: Vec<SeedRow> = db
            .prepare(
                "SELECT name, category, subcategory FROM search_seeds
                 WHERE host=?1 AND status='pending' AND coalesce(name,'')<>''
                 ORDER BY created_at, rowid LIMIT 12",
            )
            .unwrap()
            .query_map([&host], |row| {
                Ok(SeedRow {
                    url: None,
                    name: row.get(0)?,
                    context: LeadContext {
                        category: row.get(1)?,
                        subcategory: row.get(2)?,
                        ..LeadContext::default()
                    },
                })
            })
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        if siblings.len() < 2 {
            continue;
        }
        for seed in &siblings {
            let Some(mailbox) = seed.context.subcategory.as_deref().and_then(word) else {
                continue;
            };
            // Only when exactly one seed of the site has that subcategory.
            let same = siblings
                .iter()
                .filter(|other| other.context.subcategory == seed.context.subcategory)
                .count();
            if same != 1 {
                continue;
            }
            tried += 1;
            let email = format!("{mailbox}@{host}");
            let mut leads = BTreeMap::new();
            leads.insert(
                email.clone(),
                LeadAccumulator::from_source(
                    email.clone(),
                    false,
                    "https://x.example.test/",
                    "now",
                    None,
                    None,
                    siblings[0].context.clone(),
                ),
            );
            crate::lead_crawl::assign_to_seeds(&mut leads, &siblings, &siblings[0]);
            let got = leads[&email].context.subcategory.clone();
            if got == seed.context.subcategory {
                right += 1;
            } else if got == siblings[0].context.subcategory {
                unmoved += 1;
            } else {
                wrong += 1;
                if wrong_examples.len() < 8 {
                    wrong_examples.push(format!(
                        "{host}: {mailbox}@ → {got:?} (expected {:?}); seeds: {:?}",
                        seed.context.subcategory,
                        siblings
                            .iter()
                            .map(|s| s.name.clone().unwrap_or_default())
                            .collect::<Vec<_>>()
                    ));
                }
            }
        }
    }
    println!(
        "CHECK SHARED named addresses tried={tried} right={right} stayed_with_first_seed={unmoved} wrong={wrong}"
    );
    for example in wrong_examples {
        println!("CHECK SHARED wrong: {example}");
    }
}
