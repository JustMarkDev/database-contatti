//! Sites to explore and the parallel crawl behind **Cerca lead**.
//!
//! Seeds (`search_seeds`) come from directories, existing contacts' domains,
//! pasted URLs and web search. Exploring picks pending seeds one per host,
//! reads hosts in parallel through context.dev (each host sequential, see
//! `discovery::crawl_host`; the plan's concurrency limit caps the hosts in
//! flight) and appends each host's leads to a `running` run as soon as the
//! host finishes, so a stopped crawl keeps what it found. A seed's
//! organization, taxonomy and geography pass to the addresses of its own site.
//! A host that could not be read because of credits or capacity stays
//! `pending`. Nothing here writes `contacts`.

use crate::contextdev::{ContextDev, ContextError};
use crate::discovery::{
    CrawlSettings, HostCrawl, LeadAccumulator, LeadContext, RunRecord, append_leads, collect_leads,
    crawl_host, create_run, finish_run, html_to_text, page_anchors, parse_page, url_host,
};
use crate::lead_ai::LeadAi;
use crate::lead_sources::Geography;
use crate::{SearchRunSummary, open_db};
use futures::StreamExt;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;

const MAX_EXPLORE_SITES: usize = 500;
const MAX_DIRECTORY_PAGES: usize = 200;
/// A host crawled this recently is not crawled again from another list.
const RECRAWL_AFTER_DAYS: i64 = 30;
const MAX_RUN_ERRORS: usize = 300;
const MAX_AI_DIRECTORY_PAGES: usize = 15;

static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

pub(crate) fn crawl_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

pub(crate) fn cancelled() -> bool {
    CANCEL.load(Ordering::Relaxed)
}

/// One context.dev job at a time (crawl or discovery): they share the plan's
/// concurrency limit and the credits.
pub(crate) fn start_job() -> Result<RunningGuard, String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("Un'esplorazione è già in corso: fermala o attendi la fine.".to_owned());
    }
    CANCEL.store(false, Ordering::SeqCst);
    Ok(RunningGuard)
}

/// A context.dev client for one job. Refuses to start when the key is missing
/// or Auto-Topup is on without a monthly budget (`GET /org/usage`, free).
pub(crate) async fn open_context(
    app: &tauri::AppHandle,
    budget: Option<u64>,
) -> Result<ContextDev, String> {
    let (key, concurrency) = crate::database::blocking({
        let app = app.clone();
        move || {
            let db = open_db(&app)?;
            Ok((
                crate::contextdev::resolve_key(&db)?,
                crate::contextdev::concurrency(&db)?,
            ))
        }
    })
    .await?;
    let key = key.ok_or_else(|| {
        "Imposta la chiave context.dev nelle Impostazioni per leggere i siti.".to_owned()
    })?;
    let context = ContextDev::new(key, concurrency, budget)?;
    context
        .usage()
        .await
        .map_err(|error| error.to_string())?
        .spend_guard()?;
    Ok(context)
}

/// One site to explore (or, without `url`, an organization still to be found).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SeedRow {
    pub(crate) url: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) context: LeadContext,
}

/// `https://` plus the host lowercased; path and query kept as written.
pub(crate) fn normalize_seed_url(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches(['/', '.', ',', ';']);
    if value.is_empty() || value.contains(' ') || value.contains('@') {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    let rest = ["https://", "http://", "https//", "http//"]
        .iter()
        .find_map(|prefix| lower.starts_with(prefix).then(|| &value[prefix.len()..]))
        .unwrap_or(value);
    let (host, path) = match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let host = host.to_ascii_lowercase();
    (host.contains('.') && !host.starts_with('.') && !host.ends_with('.'))
        .then(|| format!("https://{host}{path}"))
}

pub(crate) fn insert_seeds(
    db: &Connection,
    list_name: &str,
    seeds: &[SeedRow],
    now: &str,
) -> Result<usize, String> {
    insert_seeds_with_status(db, list_name, seeds, "pending", now)
}

/// `pending` seeds are read; `verifica` ones wait for a person and never are.
pub(crate) fn insert_seeds_with_status(
    db: &Connection,
    list_name: &str,
    seeds: &[SeedRow],
    status: &str,
    now: &str,
) -> Result<usize, String> {
    let mut statement = db
        .prepare(
            "INSERT OR IGNORE INTO search_seeds(
                id,list_name,url,host,name,category,subcategory,comune,provincia,regione,
                address,postal_code,official_code,status,created_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?15,?14)",
        )
        .map_err(|error| error.to_string())?;
    let mut inserted = 0;
    for seed in seeds {
        let url = seed.url.as_deref().and_then(normalize_seed_url);
        let name = seed
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        if url.is_none() && name.is_none() {
            continue;
        }
        let context = &seed.context;
        inserted += statement
            .execute(params![
                uuid::Uuid::new_v4().to_string(),
                list_name,
                url,
                url.as_deref().and_then(url_host),
                name,
                context.category,
                context.subcategory,
                context.comune,
                context.provincia,
                context.regione,
                context.address,
                context.postal_code,
                context.official_code,
                now,
                status,
            ])
            .map_err(|error| error.to_string())?;
    }
    Ok(inserted)
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SeedListView {
    pub(crate) list_name: String,
    pub(crate) total: i64,
    pub(crate) pending: i64,
    pub(crate) crawled: i64,
    pub(crate) failed: i64,
    /// Organizations known only by name: "Trova siti" searches them.
    pub(crate) without_site: i64,
    /// Classified below the confidence threshold or not confirmed by the
    /// search: never read until a person checks them.
    pub(crate) to_verify: i64,
}

pub(crate) fn seed_lists(db: &Connection) -> Result<Vec<SeedListView>, String> {
    let mut statement = db
        .prepare(
            "SELECT list_name, COUNT(*),
                    SUM(status='pending' AND url IS NOT NULL), SUM(status='crawled'),
                    SUM(status='failed'), SUM(url IS NULL AND status='pending'),
                    SUM(status='verifica')
             FROM search_seeds GROUP BY list_name ORDER BY max(created_at) DESC",
        )
        .map_err(|error| error.to_string())?;

    statement
        .query_map([], |row| {
            Ok(SeedListView {
                list_name: row.get(0)?,
                total: row.get(1)?,
                pending: row.get(2)?,
                crawled: row.get(3)?,
                failed: row.get(4)?,
                without_site: row.get(5)?,
                to_verify: row.get(6)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

/// Mail providers and shared hosting: their domain is never an organization's site.
const PROVIDER_LABELS: [&str; 18] = [
    "pec",
    "legalmail",
    "arubapec",
    "aruba",
    "postecert",
    "pecimprese",
    "cert",
    "tiscali",
    "fastwebnet",
    "inwind",
    "iol",
    "email",
    "katamail",
    "teletu",
    "tim",
    "vodafone",
    "poste",
    "mailbox",
];

pub(crate) fn is_provider_domain(domain: &str) -> bool {
    crate::ai_typesafe::is_consumer_mail_domain(domain)
        || domain
            .split('.')
            .any(|label| PROVIDER_LABELS.contains(&label))
}

fn registrable(host: &str) -> String {
    let labels: Vec<&str> = host.trim_start_matches("www.").split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}

/// True when the address belongs to the site: same domain or a subdomain of it.
pub(crate) fn same_site(email_domain: &str, host: &str) -> bool {
    let site = host.trim_start_matches("www.");
    email_domain == site
        || email_domain.ends_with(&format!(".{site}"))
        || site.ends_with(&format!(".{email_domain}"))
}

/// One seed per organization domain already in the contacts database, with
/// the contact's taxonomy and geography. A domain shared by more than three
/// comuni is a provider or a ministry, not one organization's site.
pub(crate) fn contact_seeds(
    db: &Connection,
    category: Option<&str>,
    subcategory: Option<&str>,
    regione: Option<&str>,
    limit: usize,
) -> Result<Vec<SeedRow>, String> {
    // ponytail: scans the exportable contacts per call (~1s on 270k rows); a
    // domain table if this becomes interactive.
    let mut statement = db
        .prepare(
            "SELECT lower(email_domain||email_tld) AS domain, max(website), max(category),
                    max(subcategory), max(organization_name), max(comune), max(provincia),
                    max(regione), COUNT(*) AS n
             FROM contacts
             WHERE is_exportable=1 AND is_pec=0 AND coalesce(email_domain,'')<>''
               AND (?1 IS NULL OR category=?1) AND (?2 IS NULL OR subcategory=?2)
               AND (?3 IS NULL OR regione=?3)
             GROUP BY domain HAVING COUNT(DISTINCT coalesce(comune,'')) <= 3
             ORDER BY n DESC LIMIT ?4",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![category, subcategory, regione, (limit * 4) as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    LeadContext {
                        category: row.get(2)?,
                        subcategory: row.get(3)?,
                        organization: row.get(4)?,
                        comune: row.get(5)?,
                        provincia: row.get(6)?,
                        regione: row.get(7)?,
                        ..LeadContext::default()
                    },
                ))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(rows
        .into_iter()
        .filter(|(domain, _, _)| domain.contains('.') && !is_provider_domain(domain))
        .take(limit)
        .map(|(domain, website, context)| {
            let site = website
                .as_deref()
                .and_then(normalize_seed_url)
                .filter(|url| {
                    url_host(url).is_some_and(|host| registrable(&host) == registrable(&domain))
                });
            SeedRow {
                url: Some(site.unwrap_or_else(|| format!("https://{domain}"))),
                name: context.organization.clone(),
                context,
            }
        })
        .collect())
}

/// Which pending seeds to read: every field left empty matches anything.
#[derive(Debug, Default, Clone, Copy)]
struct SeedFilter<'a> {
    list_name: Option<&'a str>,
    category: Option<&'a str>,
    subcategory: Option<&'a str>,
    regione: Option<&'a str>,
}

/// Pending seeds with a site, one per host, skipping hosts crawled recently.
fn pending_seeds(
    db: &Connection,
    filter: SeedFilter,
    limit: usize,
    now: &str,
) -> Result<Vec<SeedRow>, String> {
    let cutoff = chrono::DateTime::parse_from_rfc3339(now)
        .map(|at| (at - chrono::Duration::days(RECRAWL_AFTER_DAYS)).to_rfc3339())
        .unwrap_or_default();
    let mut statement = db
        .prepare(
            "SELECT s.url, s.name, s.category, s.subcategory, s.comune, s.provincia, s.regione,
                    s.address, s.postal_code, s.official_code, s.host
             FROM search_seeds s
             WHERE s.url IS NOT NULL AND s.status='pending' AND (?1 IS NULL OR s.list_name=?1)
               AND (?4 IS NULL OR s.category=?4) AND (?5 IS NULL OR s.subcategory=?5)
               AND (?6 IS NULL OR s.regione=?6)
               AND NOT EXISTS (SELECT 1 FROM search_seeds d WHERE d.host=s.host
                               AND d.status<>'pending' AND d.crawled_at>?2)
             ORDER BY s.created_at, s.rowid LIMIT ?3",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![
                filter.list_name,
                cutoff,
                (limit * 4) as i64,
                filter.category,
                filter.subcategory,
                filter.regione
            ],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(10)?,
                    SeedRow {
                        url: row.get(0)?,
                        name: row.get(1)?,
                        context: LeadContext {
                            category: row.get(2)?,
                            subcategory: row.get(3)?,
                            comune: row.get(4)?,
                            provincia: row.get(5)?,
                            regione: row.get(6)?,
                            address: row.get(7)?,
                            postal_code: row.get(8)?,
                            official_code: row.get(9)?,
                            organization: row.get(1)?,
                            ..LeadContext::default()
                        },
                    },
                ))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let mut hosts = std::collections::HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|(host, _)| host.as_ref().is_some_and(|host| hosts.insert(host.clone())))
        .map(|(_, seed)| seed)
        .take(limit)
        .collect())
}

fn mark_host(
    db: &Connection,
    host: &str,
    status: &str,
    lead_count: usize,
    error: Option<&str>,
    now: &str,
) -> Result<(), String> {
    db.execute(
        "UPDATE search_seeds SET status=?2, lead_count=?3, error=?4, crawled_at=?5
         WHERE host=?1 AND status='pending'",
        params![host, status, lead_count as i64, error, now],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Directory crawl: where its outbound sites go and what they inherit.
#[derive(Debug, Clone)]
pub(crate) struct DirectoryInfo {
    pub(crate) list_name: String,
    pub(crate) category: Option<String>,
    pub(crate) subcategory: Option<String>,
}

pub(crate) struct CrawlJob {
    pub(crate) label: String,
    pub(crate) kind: &'static str,
    pub(crate) source_id: Option<String>,
    pub(crate) max_pages: i64,
    pub(crate) max_pdfs: i64,
    pub(crate) use_ai: bool,
    pub(crate) directory: Option<DirectoryInfo>,
}

#[derive(Debug, Clone, Serialize)]
struct CrawlProgress {
    run_id: String,
    label: String,
    done: usize,
    total: usize,
    leads: i64,
    host: String,
    finished: bool,
}

pub(crate) struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Leads of one crawled host. Addresses of the site itself (or a webmail box
/// published on it) inherit the seed's organization, taxonomy and place; in a
/// directory every address inherits the directory taxonomy. With the model,
/// the text around each address fills office, role, person and comune.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn host_leads(
    crawl: &HostCrawl,
    host: &str,
    seed: &SeedRow,
    job: &CrawlJob,
    ai: Option<&LeadAi>,
    geography: &Geography,
    now: &str,
) -> (Vec<LeadAccumulator>, Vec<SeedRow>, Vec<String>) {
    let mut errors = Vec::new();
    let (mut leads, _) = collect_leads(&crawl.pages, now);
    let directory_context = job.directory.as_ref().map(|directory| LeadContext {
        category: directory.category.clone(),
        subcategory: directory.subcategory.clone(),
        ..LeadContext::default()
    });
    for lead in leads.values_mut() {
        let domain = lead.email().rsplit('@').next().unwrap_or("").to_lowercase();
        if let Some(context) = &directory_context {
            lead.context.fill_from(context);
        } else if same_site(&domain, host) || crate::ai_typesafe::is_consumer_mail_domain(&domain) {
            lead.context.fill_from(&seed.context);
            if lead.context.organization.is_none() {
                lead.context.organization = seed.name.clone();
            }
            if lead.context.website.is_none() {
                lead.context.website = Some(format!("https://{host}"));
            }
        }
    }

    let mut outbound: Vec<SeedRow> = crawl
        .outbound
        .iter()
        .map(|(url, text)| SeedRow {
            url: Some(url.clone()),
            name: (!text.trim().is_empty()).then(|| text.trim().to_owned()),
            context: directory_context.clone().unwrap_or_default(),
        })
        .collect();

    let Some(ai) = ai else {
        return (leads.into_values().collect(), outbound, errors);
    };
    let apply_place = |context: &mut LeadContext, comune: Option<&str>| {
        if context.comune.is_some() {
            return;
        }
        if let Some(comune) = comune {
            let (comune, provincia, regione) = geography.resolve(comune, "", "");
            if regione.is_some() {
                context.comune = comune;
                context.provincia = provincia;
                context.regione = regione;
            }
        }
    };

    if job.directory.is_some() {
        let own = registrable(host);
        for page in crawl.pages.iter().take(MAX_AI_DIRECTORY_PAGES) {
            let anchors: Vec<(String, String)> = page_anchors(&page.url, &page.html)
                .into_iter()
                .filter(|(url, _)| url_host(url).is_some_and(|link| registrable(&link) != own))
                .collect();
            let text = html_to_text(&page.html);
            if anchors.is_empty() && !crate::EMAIL_RE.is_match(&text) {
                continue;
            }
            match ai.directory_entries(&page.url, &text, &anchors).await {
                Ok(entries) => {
                    for entry in entries {
                        if let Some(website) = &entry.website
                            && let Some(seed) = outbound
                                .iter_mut()
                                .find(|seed| seed.url.as_ref() == Some(website))
                        {
                            if entry.name.is_some() {
                                seed.name = entry.name.clone();
                            }
                            apply_place(&mut seed.context, entry.comune.as_deref());
                        }
                        if let Some(email) = &entry.email {
                            let key = crate::parsing::email_comparison_key(email);
                            if let Some(lead) = leads.get_mut(&key) {
                                if lead.context.organization.is_none() {
                                    lead.context.organization = entry.name.clone();
                                }
                                apply_place(&mut lead.context, entry.comune.as_deref());
                                if !lead.method.ends_with("+ia") {
                                    lead.method.push_str("+ia");
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    errors.push(format!("{}: IA elenco: {error}", page.url));
                    break;
                }
            }
        }
        return (leads.into_values().collect(), outbound, errors);
    }

    let mut contexts: BTreeMap<String, String> = BTreeMap::new();
    for page in &crawl.pages {
        for hit in parse_page(&page.url, &page.html).emails {
            contexts
                .entry(hit.email.to_lowercase())
                .or_insert(hit.context);
        }
    }
    let hits: Vec<(String, String)> = leads
        .values()
        .map(|lead| lead.email().to_lowercase())
        .map(|email| {
            let context = contexts.get(&email).cloned().unwrap_or_default();
            (email, context)
        })
        .collect();
    match ai
        .describe_contacts(host, seed.name.as_deref(), &hits)
        .await
    {
        Ok(described) => {
            for lead in leads.values_mut() {
                let Some(found) = described.get(&lead.email().to_lowercase()) else {
                    continue;
                };
                lead.context.fill_from(&LeadContext {
                    organization: found.organization.clone(),
                    department: found.department.clone(),
                    category: found.category.clone(),
                    subcategory: if lead.context.category.is_none()
                        || lead.context.category == found.category
                    {
                        found.subcategory.clone()
                    } else {
                        None
                    },
                    ..LeadContext::default()
                });
                apply_place(&mut lead.context, found.comune.as_deref());
                lead.set_role(found.role.clone());
                lead.set_person_name(found.person_name.clone());
                lead.method.push_str("+ia");
            }
        }
        Err(error) => errors.push(format!("{host}: IA contatti: {error}")),
    }
    (leads.into_values().collect(), outbound, errors)
}

/// Crawl the seeds (grouped by host, hosts in parallel) into a new run.
pub(crate) async fn crawl_urls(
    app: tauri::AppHandle,
    job: CrawlJob,
    seeds: Vec<SeedRow>,
) -> Result<SearchRunSummary, String> {
    let _guard = start_job()?;

    let mut hosts: BTreeMap<String, (Vec<String>, SeedRow)> = BTreeMap::new();
    for seed in seeds {
        let Some(url) = seed.url.as_deref().and_then(normalize_seed_url) else {
            continue;
        };
        let Some(host) = url_host(&url) else { continue };
        let entry = hosts
            .entry(host)
            .or_insert_with(|| (Vec::new(), seed.clone()));
        if !entry.0.contains(&url) {
            entry.0.push(url);
        }
    }
    if hosts.is_empty() {
        return Err("Nessun sito da esplorare".to_owned());
    }
    let total = hosts.len();
    let now = chrono::Utc::now().to_rfc3339();

    let setup_app = app.clone();
    let setup_now = now.clone();
    let label = job.label.clone();
    let (kind, source_id) = (job.kind, job.source_id.clone());
    let use_ai = job.use_ai;
    let context = open_context(&app, None).await?;
    let concurrency = context.concurrency();

    let (run_id, geography, taxonomy) = crate::database::blocking(move || {
        let db = open_db(&setup_app)?;
        let run_id = create_run(
            &db,
            &RunRecord {
                kind,
                query_text: &label,
                source_id: source_id.as_deref(),
                seed_count: total as i64,
                page_count: 0,
                pdf_found: 0,
                errors: &[],
                now: &setup_now,
            },
            "running",
        )?;
        let taxonomy = if use_ai {
            crate::lead_ai::load_taxonomy(&db)?
        } else {
            Vec::new()
        };
        Ok((run_id, Geography::load(&db)?, taxonomy))
    })
    .await?;

    let ai = if job.use_ai {
        match LeadAi::new(taxonomy) {
            Ok(ai) => Some(ai),
            Err(error) => {
                let failed_app = app.clone();
                let failed_run = run_id.clone();
                let message = format!("IA non disponibile: {error}");
                let recorded = message.clone();
                let _ = crate::database::blocking(move || {
                    finish_run(
                        &*open_db(&failed_app)?,
                        &failed_run,
                        "failed",
                        0,
                        0,
                        &[recorded],
                    )
                })
                .await;
                return Err(message);
            }
        }
    } else {
        None
    };
    let settings = CrawlSettings {
        context: &context,
        max_pages: job.max_pages.max(1) as usize,
        max_pdfs: job.max_pdfs.max(0) as usize,
        directory: job.directory.is_some(),
        ai: ai.as_ref(),
        cancel: &CANCEL,
    };

    let mut stream = futures::stream::iter(hosts)
        .map(|(host, (urls, seed))| {
            let (settings, job, ai, geography, now) =
                (&settings, &job, ai.as_ref(), &geography, &now);
            async move {
                if CANCEL.load(Ordering::Relaxed) || settings.context.stop_reason().is_some() {
                    return (
                        host,
                        HostCrawl::default(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        true,
                    );
                }
                let crawl = crawl_host(settings, &urls, seed.context.category.as_deref()).await;
                let site = crawl.site_host.clone().unwrap_or_else(|| host.clone());
                let (leads, outbound, errors) =
                    host_leads(&crawl, &site, &seed, job, ai, geography, now).await;
                (host, crawl, leads, outbound, errors, false)
            }
        })
        .buffer_unordered(concurrency);

    let mut done = 0usize;
    let mut page_count = 0i64;
    let mut pdf_found = 0i64;
    let mut lead_total = 0i64;
    let mut errors: Vec<String> = Vec::new();
    let mut linked_sites: Vec<SeedRow> = Vec::new();
    while let Some((host, crawl, leads, outbound, host_errors, skipped)) = stream.next().await {
        done += 1;
        if skipped {
            continue;
        }
        page_count += crawl.pages.len() as i64;
        pdf_found += crawl.pdf_found as i64;
        // Credits, capacity or the key, not the site: read it again next run.
        let status = match (crawl.pages.is_empty(), crawl.retry_later) {
            (false, _) => Some("crawled"),
            (true, false) => Some("failed"),
            (true, true) => None,
        };
        let first_error = crawl.errors.first().cloned();
        errors.extend(crawl.errors.into_iter().chain(host_errors));
        let (persist_app, persist_run, persist_now) = (app.clone(), run_id.clone(), now.clone());
        linked_sites.extend(outbound);
        let persist_host = host.clone();
        let inserted = crate::database::blocking(move || {
            let db = open_db(&persist_app)?;
            let count = leads.len();
            let inserted = append_leads(&db, &persist_run, leads)?;
            if let Some(status) = status {
                mark_host(
                    &db,
                    &persist_host,
                    status,
                    count,
                    first_error.as_deref(),
                    &persist_now,
                )?;
            }
            Ok(inserted)
        })
        .await?;
        lead_total += inserted as i64;
        let _ = app.emit(
            "lead-crawl-progress",
            CrawlProgress {
                run_id: run_id.clone(),
                label: job.label.clone(),
                done,
                total,
                leads: lead_total,
                host,
                finished: false,
            },
        );
    }
    drop(stream);

    // Channels 1 and 2: links out of the sites read, or a directory's entries.
    let list_name = job
        .directory
        .as_ref()
        .map_or(crate::lead_candidates::CHANNEL_LINKS, |directory| {
            directory.list_name.as_str()
        });
    let candidates: Vec<_> = linked_sites
        .into_iter()
        .filter_map(|seed| {
            Some(crate::lead_candidates::Candidate {
                url: seed.url?,
                name: seed.name,
                context: seed.context,
                ..Default::default()
            })
        })
        .collect();
    if !candidates.is_empty() {
        match crate::lead_candidates::accept_candidates(&app, list_name, candidates).await {
            // The links cost nothing; the directory pages are counted in this run.
            Ok(report) if job.directory.is_none() => {
                let recorded = crate::database::blocking({
                    let app = app.clone();
                    move || {
                        crate::lead_candidates::record_discovery(
                            &*open_db(&app)?,
                            crate::lead_candidates::CHANNEL_LINKS,
                            &report,
                            &[],
                        )
                    }
                })
                .await;
                if let Err(error) = recorded {
                    errors.push(format!("Siti collegati: {error}"));
                }
            }
            Ok(_) => {}
            Err(error) => errors.push(format!("Siti collegati non salvati: {error}")),
        }
    }

    let cancelled = CANCEL.load(Ordering::SeqCst);
    let halted = context.stop_reason();
    errors.truncate(MAX_RUN_ERRORS);
    if let Some(reason) = &halted {
        errors.insert(0, format!("Ricerca fermata: {reason}"));
    }
    let (finish_app, finish_run_id) = (app.clone(), run_id.clone());
    let status = match (&halted, cancelled) {
        (Some(ContextError::BudgetReached), _) | (None, false) => "completed",
        (Some(_), _) => "failed",
        (None, true) => "cancelled",
    };
    crate::database::blocking(move || {
        finish_run(
            &*open_db(&finish_app)?,
            &finish_run_id,
            status,
            page_count,
            pdf_found,
            &errors,
        )
    })
    .await?;
    let _ = app.emit(
        "lead-crawl-progress",
        CrawlProgress {
            run_id: run_id.clone(),
            label: job.label.clone(),
            done,
            total,
            leads: lead_total,
            host: String::new(),
            finished: true,
        },
    );
    if let Some(reason) = halted.filter(|reason| *reason != ContextError::BudgetReached) {
        return Err(format!(
            "Ricerca fermata: {reason}. I lead trovati finora sono salvati; i siti non letti restano da leggere."
        ));
    }
    Ok(SearchRunSummary {
        run_id,
        lead_count: lead_total,
        page_count,
        pdf_found,
        // Page failures of a crawl are routine and stay in the run's own error list.
        errors: Vec::new(),
    })
}

/// Categories and subcategories for the taxonomy pickers.
#[tauri::command]
pub(crate) async fn list_lead_taxonomy(
    app: tauri::AppHandle,
) -> Result<crate::lead_ai::Taxonomy, String> {
    crate::database::blocking(move || crate::lead_ai::load_taxonomy(&*open_db(&app)?)).await
}

/// Free-form request -> taxonomy, region, catalog sources and web search.
#[tauri::command]
pub(crate) async fn plan_lead_request(
    app: tauri::AppHandle,
    request: String,
) -> Result<crate::lead_ai::LeadPlan, String> {
    let request = request.trim().to_owned();
    if request.is_empty() {
        return Err("Descrivi cosa cerchi.".to_owned());
    }
    let taxonomy =
        crate::database::blocking(move || crate::lead_ai::load_taxonomy(&*open_db(&app)?)).await?;
    let ai = LeadAi::new(taxonomy).map_err(|error| format!("IA non disponibile: {error}"))?;
    let sources: Vec<_> = crate::lead_sources::CATALOG
        .iter()
        .filter(|source| source.kind == "directory")
        .map(|source| (source.id, source.name, source.note))
        .collect();
    ai.plan_request(&request, &sources).await
}

#[tauri::command]
pub(crate) fn cancel_lead_crawl() {
    CANCEL.store(true, Ordering::SeqCst);
}

#[tauri::command]
pub(crate) async fn list_seed_lists(app: tauri::AppHandle) -> Result<Vec<SeedListView>, String> {
    crate::database::blocking(move || seed_lists(&*open_db(&app)?)).await
}

#[tauri::command]
pub(crate) async fn delete_seed_list(
    app: tauri::AppHandle,
    list_name: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        open_db(&app)?
            .execute(
                "DELETE FROM search_seeds WHERE list_name=?1",
                params![list_name],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
    .await
}

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// Seeds from the organization domains already in the contacts database.
#[tauri::command]
pub(crate) async fn add_contact_seeds(
    app: tauri::AppHandle,
    category: Option<String>,
    subcategory: Option<String>,
    regione: Option<String>,
    limit: usize,
) -> Result<usize, String> {
    let (category, subcategory, regione) = (clean(category), clean(subcategory), clean(regione));
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let seeds = contact_seeds(
            &db,
            category.as_deref(),
            subcategory.as_deref(),
            regione.as_deref(),
            limit.clamp(1, 5_000),
        )?;
        let scope = [
            subcategory.as_deref(),
            category.as_deref(),
            regione.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        let list = if scope.is_empty() {
            "Contatti: tutti".to_owned()
        } else {
            format!("Contatti: {scope}")
        };
        insert_seeds(&db, &list, &seeds, &chrono::Utc::now().to_rfc3339())
    })
    .await
}

/// Seeds pasted by the user, one URL per line, with an optional taxonomy.
#[tauri::command]
pub(crate) async fn add_seed_urls(
    app: tauri::AppHandle,
    list_name: String,
    urls: Vec<String>,
    category: Option<String>,
    subcategory: Option<String>,
) -> Result<usize, String> {
    let list_name = clean(Some(list_name)).unwrap_or_else(|| "Siti aggiunti".to_owned());
    let context = LeadContext {
        category: clean(category),
        subcategory: clean(subcategory),
        ..LeadContext::default()
    };
    let seeds: Vec<SeedRow> = urls
        .iter()
        .filter_map(|url| normalize_seed_url(url))
        .map(|url| SeedRow {
            url: Some(url),
            name: None,
            context: context.clone(),
        })
        .collect();
    if seeds.is_empty() {
        return Err("Nessun URL valido".to_owned());
    }
    crate::database::blocking(move || {
        insert_seeds(
            &*open_db(&app)?,
            &list_name,
            &seeds,
            &chrono::Utc::now().to_rfc3339(),
        )
    })
    .await
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ExploreRequest {
    pub(crate) list_name: Option<String>,
    /// Only seeds of this taxonomy and region (from a free-form request).
    pub(crate) category: Option<String>,
    pub(crate) subcategory: Option<String>,
    pub(crate) regione: Option<String>,
    pub(crate) limit: usize,
    pub(crate) max_pages: i64,
    pub(crate) max_pdfs: i64,
    pub(crate) use_ai: bool,
}

impl Default for ExploreRequest {
    fn default() -> Self {
        Self {
            list_name: None,
            category: None,
            subcategory: None,
            regione: None,
            limit: 50,
            max_pages: crate::discovery::DEFAULT_MAX_PAGES_PER_DOMAIN,
            max_pdfs: crate::discovery::DEFAULT_MAX_PDFS,
            use_ai: false,
        }
    }
}

/// Crawl the next pending sites of a list (or of every list).
#[tauri::command]
pub(crate) async fn explore_seeds(
    app: tauri::AppHandle,
    request: ExploreRequest,
) -> Result<SearchRunSummary, String> {
    let list_name = clean(request.list_name.clone());
    let limit = request.limit.clamp(1, MAX_EXPLORE_SITES);
    let pick_app = app.clone();
    let (pick_list, pick_category) = (list_name.clone(), clean(request.category.clone()));
    let (pick_subcategory, pick_regione) = (
        clean(request.subcategory.clone()),
        clean(request.regione.clone()),
    );
    let seeds = crate::database::blocking(move || {
        pending_seeds(
            &*open_db(&pick_app)?,
            SeedFilter {
                list_name: pick_list.as_deref(),
                category: pick_category.as_deref(),
                subcategory: pick_subcategory.as_deref(),
                regione: pick_regione.as_deref(),
            },
            limit,
            &chrono::Utc::now().to_rfc3339(),
        )
    })
    .await?;
    if seeds.is_empty() {
        return Err("Nessun sito da esplorare in questo elenco".to_owned());
    }
    let (max_pages, max_pdfs) =
        crate::discovery::clamp_run_limits(Some(request.max_pages), Some(request.max_pdfs));
    crawl_urls(
        app,
        CrawlJob {
            label: format!(
                "Esplora: {}",
                list_name.as_deref().unwrap_or("tutti gli elenchi")
            ),
            kind: "crawl",
            source_id: None,
            max_pages,
            max_pdfs,
            use_ai: request.use_ai,
            directory: None,
        },
        seeds,
    )
    .await
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct DirectoryRequest {
    pub(crate) source_id: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) subcategory: Option<String>,
    pub(crate) max_pages: Option<usize>,
    pub(crate) use_ai: bool,
}

/// Crawl a directory (catalog entry or pasted URL): its emails become leads
/// with the directory taxonomy, its links to other sites become seeds.
#[tauri::command]
pub(crate) async fn explore_directory(
    app: tauri::AppHandle,
    request: DirectoryRequest,
) -> Result<SearchRunSummary, String> {
    let catalog = clean(request.source_id.clone()).map(|id| {
        crate::lead_sources::CATALOG
            .iter()
            .find(|source| source.id == id && source.kind == "directory")
            .ok_or_else(|| format!("Elenco sconosciuto: {id}"))
    });
    let catalog = catalog.transpose()?;
    let (url, name, category, subcategory) = match catalog {
        Some(source) => (
            source.landing_url.to_owned(),
            source.name.to_owned(),
            source.category.map(str::to_owned),
            source.subcategory.map(str::to_owned),
        ),
        None => {
            let url = request
                .url
                .as_deref()
                .and_then(normalize_seed_url)
                .ok_or_else(|| "Indica l'URL della pagina elenco".to_owned())?;
            let name = clean(request.name.clone())
                .or_else(|| url_host(&url))
                .unwrap_or_default();
            (
                url,
                name,
                clean(request.category.clone()),
                clean(request.subcategory.clone()),
            )
        }
    };
    crawl_urls(
        app,
        CrawlJob {
            label: format!("Elenco: {name}"),
            kind: "directory",
            source_id: catalog.map(|source| source.id.to_owned()),
            max_pages: request
                .max_pages
                .unwrap_or(60)
                .clamp(1, MAX_DIRECTORY_PAGES) as i64,
            max_pdfs: 0,
            use_ai: request.use_ai,
            directory: Some(DirectoryInfo {
                list_name: format!("Elenco: {name}"),
                category: category.clone(),
                subcategory: subcategory.clone(),
            }),
        },
        vec![SeedRow {
            url: Some(url),
            name: Some(name),
            context: LeadContext {
                category,
                subcategory,
                ..LeadContext::default()
            },
        }],
    )
    .await
}

/// Find the website of organizations known only by name, `<name> <comune>`:
/// Google Maps when enabled (the place's city must be the comune), otherwise
/// context.dev Search (the result must name the comune or the organization,
/// §8 of the plan). A site that does not pass waits as `verifica`.
#[tauri::command]
pub(crate) async fn find_seed_sites(
    app: tauri::AppHandle,
    list_name: Option<String>,
    limit: usize,
) -> Result<usize, String> {
    let list_name = clean(list_name);
    let read_app = app.clone();
    let read_list = list_name.clone();
    let pending = crate::database::blocking(move || {
        let db = open_db(&read_app)?;
        let mut statement = db
            .prepare(
                "SELECT id, name, coalesce(comune,'') FROM search_seeds
                 WHERE url IS NULL AND status='pending' AND name IS NOT NULL
                   AND (?1 IS NULL OR list_name=?1)
                 ORDER BY created_at, rowid LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        statement
            .query_map(params![read_list, limit.clamp(1, 200) as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    })
    .await?;
    if pending.is_empty() {
        return Err("Nessun ente senza sito in questo elenco".to_owned());
    }
    let _job = start_job()?;
    let context = open_context(&app, None).await?;
    let (found, errors) = crate::lead_candidates::find_sites(&app, &context, &pending).await?;
    let report = crate::lead_candidates::DiscoveryReport {
        found: found.len(),
        credits: context.credits_used(),
        ..Default::default()
    };
    let stop = context.stop_reason();
    let readable = crate::database::blocking(move || {
        let db = open_db(&app)?;
        let readable = crate::lead_candidates::store_found_sites(&db, &found)?;
        let report = crate::lead_candidates::DiscoveryReport {
            accepted: readable,
            ..report
        };
        crate::lead_candidates::record_discovery(
            &db,
            crate::lead_candidates::CHANNEL_FIND_SITES,
            &report,
            &errors,
        )?;
        Ok(readable)
    })
    .await?;
    match stop {
        Some(reason) if reason != ContextError::BudgetReached => {
            Err(format!("Ricerca dei siti fermata: {reason}"))
        }
        _ => Ok(readable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::FetchedPage;
    use crate::schema::setup_schema;
    use std::collections::HashMap;

    #[test]
    fn lead_crawl_seed_urls_are_normalized_and_deduplicated_per_list() {
        assert_eq!(
            normalize_seed_url("WWW.Parrocchia.Example.test/Contatti/").as_deref(),
            Some("https://www.parrocchia.example.test/Contatti")
        );
        assert_eq!(
            normalize_seed_url("http//example.test").as_deref(),
            Some("https://example.test")
        );
        assert_eq!(normalize_seed_url("utente@example.test"), None);
        assert_eq!(normalize_seed_url("non un sito"), None);

        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let seed = |url: Option<&str>, name: Option<&str>| SeedRow {
            url: url.map(str::to_owned),
            name: name.map(str::to_owned),
            context: LeadContext::default(),
        };
        let now = "2026-09-29T10:00:00Z";
        let inserted = insert_seeds(
            &db,
            "Fonte: test",
            &[
                seed(Some("https://a.example.test/"), Some("A")),
                seed(Some("a.example.test/contatti"), Some("A bis")),
                seed(None, Some("Pro Loco Esempio")),
                seed(None, None),
            ],
            now,
        )
        .unwrap();
        assert_eq!(inserted, 2, "same host once per list, empty seed skipped");
        assert_eq!(
            insert_seeds(
                &db,
                "Altro elenco",
                &[seed(Some("https://a.example.test"), None)],
                now
            )
            .unwrap(),
            1
        );
        let lists = seed_lists(&db).unwrap();
        let test_list = lists
            .iter()
            .find(|list| list.list_name == "Fonte: test")
            .unwrap();
        assert_eq!((test_list.pending, test_list.without_site), (1, 1));

        // One seed per host; a host crawled from one list is not crawled again from another.
        assert_eq!(
            pending_seeds(&db, SeedFilter::default(), 10, now)
                .unwrap()
                .len(),
            1
        );
        mark_host(&db, "a.example.test", "crawled", 3, None, now).unwrap();
        assert!(
            pending_seeds(&db, SeedFilter::default(), 10, now)
                .unwrap()
                .is_empty()
        );
        insert_seeds(
            &db,
            "Terzo",
            &[seed(Some("https://a.example.test/chi-siamo"), None)],
            now,
        )
        .unwrap();
        assert!(
            pending_seeds(
                &db,
                SeedFilter {
                    list_name: Some("Terzo"),
                    ..SeedFilter::default()
                },
                10,
                now
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn lead_crawl_pending_seeds_follow_the_request_filter() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let seed = |url: &str, subcategory: &str, regione: &str| SeedRow {
            url: Some(url.to_owned()),
            name: None,
            context: LeadContext {
                category: Some("Turismo e promozione territoriale".to_owned()),
                subcategory: Some(subcategory.to_owned()),
                regione: Some(regione.to_owned()),
                ..LeadContext::default()
            },
        };
        let now = "2026-09-29T10:00:00Z";
        insert_seeds(
            &db,
            "Fonte: test",
            &[
                seed("https://a.example.test", "Pro Loco", "Lombardia"),
                seed("https://b.example.test", "Pro Loco", "Piemonte"),
                seed("https://c.example.test", "APT", "Lombardia"),
            ],
            now,
        )
        .unwrap();
        let hosts = |filter: SeedFilter| {
            pending_seeds(&db, filter, 10, now)
                .unwrap()
                .into_iter()
                .filter_map(|seed| seed.url)
                .collect::<Vec<_>>()
        };
        assert_eq!(hosts(SeedFilter::default()).len(), 3);
        let filter = SeedFilter {
            subcategory: Some("Pro Loco"),
            regione: Some("Lombardia"),
            ..SeedFilter::default()
        };
        assert_eq!(hosts(filter), ["https://a.example.test"]);
    }

    #[test]
    fn lead_crawl_contact_domains_skip_providers_and_shared_domains() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let contact = |email: &str, comune: &str| {
            let (local, domain, tld) = crate::parsing::email_parts(email);
            db.execute(
                "INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,
                    validation_notes,source_file,category,subcategory,paese,comune,regione,
                    email_comparison_key,is_pec,organization_name)
                 VALUES(?1,?2,?3,?4,?5,'valid','[]','test.csv','Enti religiosi','Parrocchia',
                        'Italia',?6,'Lombardia',?2,0,'Parrocchia San Marco')",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    email,
                    local,
                    domain,
                    tld,
                    comune
                ],
            )
            .unwrap();
        };
        contact("info@parrocchia.example.test", "Bergamo");
        contact("don@parrocchia.example.test", "Bergamo");
        contact("parroco@gmail.com", "Bergamo");
        for (index, comune) in ["Bergamo", "Brescia", "Milano", "Como"].iter().enumerate() {
            contact(&format!("scuola{index}@condiviso.example.test"), comune);
        }
        let seeds = contact_seeds(&db, Some("Enti religiosi"), None, None, 10).unwrap();
        assert_eq!(seeds.len(), 1, "{seeds:?}");
        assert_eq!(
            seeds[0].url.as_deref(),
            Some("https://parrocchia.example.test")
        );
        assert_eq!(seeds[0].context.subcategory.as_deref(), Some("Parrocchia"));
        assert_eq!(seeds[0].context.comune.as_deref(), Some("Bergamo"));
    }

    #[tokio::test]
    async fn lead_crawl_site_addresses_inherit_the_seed_context() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let crawl = HostCrawl {
            pages: vec![FetchedPage {
                url: "https://www.parrocchia.example.test/contatti".to_owned(),
                html: "<p>Segreteria parrocchiale segreteria@parrocchia.example.test</p>\
                       <p>Oratorio oratorio.parrocchia@gmail.com</p>\
                       <p>Diocesi curia@diocesi.example.test</p>"
                    .to_owned(),
                via: "archivio",
            }],
            ..HostCrawl::default()
        };
        let seed = SeedRow {
            url: Some("https://www.parrocchia.example.test".to_owned()),
            name: Some("Parrocchia San Marco".to_owned()),
            context: LeadContext {
                category: Some("Enti religiosi".to_owned()),
                subcategory: Some("Parrocchia".to_owned()),
                comune: Some("Bergamo".to_owned()),
                ..LeadContext::default()
            },
        };
        let job = CrawlJob {
            label: "test".to_owned(),
            kind: "crawl",
            source_id: None,
            max_pages: 5,
            max_pdfs: 0,
            use_ai: false,
            directory: None,
        };
        let (leads, outbound, errors) = host_leads(
            &crawl,
            "www.parrocchia.example.test",
            &seed,
            &job,
            None,
            &geography,
            "2026-09-29T10:00:00Z",
        )
        .await;
        assert!(outbound.is_empty() && errors.is_empty());
        let by_email: HashMap<&str, &LeadAccumulator> =
            leads.iter().map(|lead| (lead.email(), lead)).collect();
        let own = by_email["segreteria@parrocchia.example.test"];
        assert_eq!(
            own.context.organization.as_deref(),
            Some("Parrocchia San Marco")
        );
        assert_eq!(own.context.subcategory.as_deref(), Some("Parrocchia"));
        assert_eq!(own.method, "archivio");
        assert_eq!(
            by_email["oratorio.parrocchia@gmail.com"]
                .context
                .comune
                .as_deref(),
            Some("Bergamo")
        );
        assert_eq!(
            by_email["curia@diocesi.example.test"].context.category,
            None
        );
        assert!(same_site(
            "parrocchia.example.test",
            "www.parrocchia.example.test"
        ));
        assert!(!same_site(
            "diocesi.example.test",
            "www.parrocchia.example.test"
        ));
    }
}
