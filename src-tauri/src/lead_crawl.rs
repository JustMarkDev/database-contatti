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
use crate::lead_directories::{DIRECTORIES, Geography};
use crate::{SearchRunSummary, open_db};
use futures::StreamExt;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
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

/// A context.dev client for one job, capped at what the balance allows above
/// the reserve, the per-run cap of the settings and `request_cap` (0: none).
/// Refuses to start when the key is missing, the balance is at the reserve or
/// Auto-Topup is on without a monthly budget (`GET /org/usage`, free).
pub(crate) async fn open_context(
    app: &tauri::AppHandle,
    request_cap: u64,
) -> Result<ContextDev, String> {
    let (key, settings) = crate::database::blocking({
        let app = app.clone();
        move || {
            let db = open_db(&app)?;
            Ok((
                crate::contextdev::resolve_key(&db)?,
                crate::contextdev::load_settings(&db)?,
            ))
        }
    })
    .await?;
    let key = key.ok_or_else(|| {
        "Imposta la chiave context.dev nelle Impostazioni per leggere i siti.".to_owned()
    })?;
    let usage = ContextDev::new(key.clone(), 1, None)?
        .usage()
        .await
        .map_err(|error| error.to_string())?;
    let budget = usage.budget(&settings, request_cap)?;
    ContextDev::new(key, settings.concurrency, Some(budget))
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

/// What is known and read about one subcategory: the page's "how far along" card.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SubcategoryCoverage {
    pub(crate) category: String,
    pub(crate) subcategory: String,
    /// Distinct sites known, those waiting for a person to check them excluded.
    pub(crate) sites: i64,
    pub(crate) pending: i64,
    pub(crate) crawled: i64,
    pub(crate) failed: i64,
    pub(crate) to_verify: i64,
    /// Addresses found on the sites read.
    pub(crate) leads: i64,
    /// Of those, the ones no contact holds yet.
    pub(crate) new_leads: i64,
    /// context.dev credits spent reading these sites.
    pub(crate) credits: i64,
}

/// One row per askable subcategory (catch-alls left out), optionally within one region.
pub(crate) fn subcategory_coverage(
    db: &Connection,
    regione: Option<&str>,
) -> Result<Vec<SubcategoryCoverage>, String> {
    let mut statement = db
        .prepare(
            "SELECT c.name, s.name,
                    COUNT(DISTINCT CASE WHEN d.url IS NOT NULL AND d.status<>'verifica' THEN d.host END),
                    COUNT(DISTINCT CASE WHEN d.url IS NOT NULL AND d.status='pending' THEN d.host END),
                    COUNT(DISTINCT CASE WHEN d.status='crawled' THEN d.host END),
                    COUNT(DISTINCT CASE WHEN d.status='failed' THEN d.host END),
                    COUNT(DISTINCT CASE WHEN d.status='verifica' THEN d.host END),
                    coalesce(SUM(CASE WHEN d.status='crawled' THEN d.lead_count END), 0),
                    coalesce(SUM(d.credits_used), 0)
             FROM subcategories s JOIN categories c ON c.id=s.category_id
             LEFT JOIN search_seeds d ON d.category=c.name AND d.subcategory=s.name
                  AND (?1 IS NULL OR d.regione=?1)
             GROUP BY c.name, s.name ORDER BY c.name, s.name",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![regione], |row| {
            Ok(SubcategoryCoverage {
                category: row.get(0)?,
                subcategory: row.get(1)?,
                sites: row.get(2)?,
                pending: row.get(3)?,
                crawled: row.get(4)?,
                failed: row.get(5)?,
                to_verify: row.get(6)?,
                leads: row.get(7)?,
                new_leads: 0,
                credits: row.get(8)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let new_leads: std::collections::HashMap<(String, String), i64> = db
        .prepare(
            "SELECT l.category, l.subcategory, COUNT(DISTINCT l.email_comparison_key)
             FROM search_leads l
             WHERE (?1 IS NULL OR l.regione=?1) AND l.category IS NOT NULL AND l.subcategory IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM contacts c
                              WHERE c.email_comparison_key = l.email_comparison_key COLLATE NOCASE)
             GROUP BY l.category, l.subcategory",
        )
        .and_then(|mut statement| {
            statement
                .query_map(params![regione], |row| {
                    Ok(((row.get(0)?, row.get(1)?), row.get(2)?))
                })?
                .collect::<Result<_, _>>()
        })
        .map_err(|error| error.to_string())?;
    let rows: Vec<SubcategoryCoverage> = rows
        .into_iter()
        .map(|mut row| {
            row.new_leads = new_leads
                .get(&(row.category.clone(), row.subcategory.clone()))
                .copied()
                .unwrap_or(0);
            row
        })
        .collect();
    Ok(rows
        .into_iter()
        .filter(|row| crate::lead_ai::is_target_subcategory(&row.category, &row.subcategory))
        .collect())
}

#[tauri::command]
pub(crate) async fn list_subcategory_coverage(
    app: tauri::AppHandle,
    regione: Option<String>,
) -> Result<Vec<SubcategoryCoverage>, String> {
    let regione = clean(regione);
    crate::database::blocking(move || subcategory_coverage(&*open_db(&app)?, regione.as_deref()))
        .await
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

/// Status of a host's seeds after its crawl; `None` leaves them pending. Credits,
/// capacity, the key or a stop (not the site) mean read it again next run; the
/// addresses found so far are kept either way.
fn host_status(has_pages: bool, retry_later: bool) -> Option<&'static str> {
    match (has_pages, retry_later) {
        (_, true) => None,
        (true, false) => Some("crawled"),
        (false, false) => Some("failed"),
    }
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

/// Credits a host's reading cost, on the seeds of that host (any status).
fn add_host_credits(db: &Connection, host: &str, credits: u64) -> Result<(), String> {
    db.execute(
        "UPDATE search_seeds SET credits_used=credits_used+?2
         WHERE id=(SELECT id FROM search_seeds WHERE host=?1 ORDER BY created_at, rowid LIMIT 1)",
        params![host, credits as i64],
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
    pub(crate) directory: Option<DirectoryInfo>,
    /// Credits this run may spend at most (0: the settings' per-run cap).
    pub(crate) max_credits: u64,
    /// OpenCode choice of Settings.
    pub(crate) ai_model: String,
    pub(crate) ai_effort: Option<String>,
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
    /// Credits this run has spent so far.
    credits_used: u64,
    /// Balance after the last call, when context.dev reported it.
    credits_remaining: Option<i64>,
}

pub(crate) struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Name words, lowercase, without the little words that tell nothing apart.
fn name_tokens(text: &str) -> HashSet<String> {
    const LITTLE: [&str; 14] = [
        "del", "della", "dello", "dei", "degli", "delle", "per", "con", "gli", "una", "uno",
        "alla", "nel", "sul",
    ];
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3 && !LITTLE.contains(word))
        .map(str::to_owned)
        .collect()
}

/// A site shared by several seeds (a comune with its library, museum and
/// theatre): an address goes to the seed whose name words, those the other seeds
/// do not share, appear in the address's organization, office or mailbox name,
/// and only one seed does. Its taxonomy and place replace the first seed's.
pub(crate) fn assign_to_seeds(
    leads: &mut BTreeMap<String, LeadAccumulator>,
    siblings: &[SeedRow],
    first: &SeedRow,
) {
    if siblings.len() < 2 {
        return;
    }
    let names: Vec<HashSet<String>> = siblings
        .iter()
        .map(|seed| {
            // The subcategory names the kind of body when its name does not ("Palazzo X").
            name_tokens(&format!(
                "{} {}",
                seed.name.as_deref().unwrap_or(""),
                seed.context.subcategory.as_deref().unwrap_or("")
            ))
        })
        .collect();
    let distinctive: Vec<HashSet<String>> = names
        .iter()
        .map(|own| {
            own.iter()
                .filter(|word| names.iter().any(|other| !other.contains(*word)))
                .cloned()
                .collect()
        })
        .collect();
    for lead in leads.values_mut() {
        let local = lead.email().split('@').next().unwrap_or("").to_owned();
        let mut words = name_tokens(&local);
        // The first seed's own name was only inherited: it says nothing about the address.
        let organization = lead
            .context
            .organization
            .as_ref()
            .filter(|name| Some(*name) != first.name.as_ref());
        for text in [organization, lead.context.department.as_ref()]
            .into_iter()
            .flatten()
        {
            words.extend(name_tokens(text));
        }
        let scores: Vec<usize> = distinctive
            .iter()
            .map(|own| own.intersection(&words).count())
            .collect();
        let best = scores.iter().copied().max().unwrap_or(0);
        if best == 0 || scores.iter().filter(|score| **score == best).count() > 1 {
            continue;
        }
        let Some(index) = scores.iter().position(|score| *score == best) else {
            continue;
        };
        let (seed, context) = (&siblings[index], &mut lead.context);
        context.organization = seed.name.clone().or(context.organization.take());
        if seed.context.category.is_some() {
            context.category = seed.context.category.clone();
            context.subcategory = seed.context.subcategory.clone();
        }
        for (target, value) in [
            (&mut context.official_code, &seed.context.official_code),
            (&mut context.address, &seed.context.address),
            (&mut context.postal_code, &seed.context.postal_code),
            (&mut context.comune, &seed.context.comune),
            (&mut context.provincia, &seed.context.provincia),
            (&mut context.regione, &seed.context.regione),
        ] {
            if value.is_some() {
                target.clone_from(value);
            }
        }
    }
}

/// The pending seeds of each host, to tell apart the organizations sharing a site.
async fn host_seeds(app: &tauri::AppHandle, hosts: Vec<String>) -> BTreeMap<String, Vec<SeedRow>> {
    let app = app.clone();
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let mut statement = db
            .prepare(
                "SELECT name, category, subcategory, comune, provincia, regione, address,
                        postal_code, official_code FROM search_seeds
                 WHERE host=?1 AND status='pending' AND coalesce(name,'')<>''
                 ORDER BY created_at, rowid LIMIT 12",
            )
            .map_err(|error| error.to_string())?;
        let mut found = BTreeMap::new();
        for host in hosts {
            let rows: Vec<SeedRow> = statement
                .query_map(params![host], |row| {
                    Ok(SeedRow {
                        url: None,
                        name: row.get(0)?,
                        context: LeadContext {
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
                })
                .map_err(|error| error.to_string())?
                .filter_map(Result::ok)
                .collect();
            if rows.len() > 1 {
                found.insert(host, rows);
            }
        }
        Ok(found)
    })
    .await
    // A failed lookup only leaves every address with the first seed's context.
    .unwrap_or_default()
}

/// Comparison keys among `keys` that already belong to a contact.
async fn known_keys(app: &tauri::AppHandle, keys: Vec<String>) -> HashSet<String> {
    let app = app.clone();
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let mut statement = db
            .prepare("SELECT 1 FROM contacts WHERE email_comparison_key=?1 LIMIT 1")
            .map_err(|error| error.to_string())?;
        Ok(keys
            .into_iter()
            .filter(|key| statement.exists(params![key]).unwrap_or(false))
            .collect())
    })
    .await
    // A failed lookup only costs model requests, never leads.
    .unwrap_or_default()
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
    known: &HashSet<String>,
    siblings: &[SeedRow],
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

    let shared = job.directory.is_none();
    let Some(ai) = ai else {
        if shared {
            assign_to_seeds(&mut leads, siblings, seed);
        }
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
        .iter()
        .filter(|(key, _)| !known.contains(*key))
        .map(|(_, lead)| lead.email().to_lowercase())
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
            // An outside vendor's mailbox found on the site is not a lead of the entity.
            leads.retain(|_, lead| {
                let domain = lead.email().rsplit('@').next().unwrap_or("").to_lowercase();
                let foreign = !same_site(&domain, host)
                    && !crate::ai_typesafe::is_consumer_mail_domain(&domain);
                !(foreign
                    && described
                        .get(&lead.email().to_lowercase())
                        .is_some_and(|found| found.third_party))
            });
            for lead in leads.values_mut() {
                let Some(found) = described.get(&lead.email().to_lowercase()) else {
                    // Not read by the model (already a contact, or unanswered): the page
                    // parser's guess at a person is mostly page furniture ("Cookie Policy").
                    lead.set_described(None, None);
                    continue;
                };
                // The model read the text around the address: where it answers, it
                // prevails over the seed's context (shared by every address of the site).
                let context = &mut lead.context;
                context.organization = found.organization.clone().or(context.organization.take());
                context.department = found.department.clone().or(context.department.take());
                if found.category.is_some() {
                    // Seeds can be wrong too (a provincia seeded as "Comune"): the
                    // model's pair replaces the seed's, not only its category.
                    context.category = found.category.clone();
                    context.subcategory = found.subcategory.clone();
                }
                apply_place(&mut lead.context, found.comune.as_deref());
                lead.set_described(found.role.clone(), found.person_name.clone());
                lead.method.push_str("+ia");
            }
        }
        Err(error) => errors.push(format!("{host}: IA contatti: {error}")),
    }
    if shared {
        assign_to_seeds(&mut leads, siblings, seed);
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
    let (ai_model, ai_effort) = (job.ai_model.clone(), job.ai_effort.clone());
    let context = open_context(&app, job.max_credits).await?;
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
                now: &setup_now,
            },
            "running",
        )?;
        Ok((
            run_id,
            Geography::load(&db)?,
            crate::lead_ai::load_taxonomy(&db)?,
        ))
    })
    .await?;

    let siblings = if job.directory.is_none() {
        host_seeds(&app, hosts.keys().cloned().collect()).await
    } else {
        BTreeMap::new()
    };
    let ai = match LeadAi::new(taxonomy, &ai_model, ai_effort.as_deref()) {
        Ok(ai) => ai,
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
    };
    let settings = CrawlSettings {
        context: &context,
        max_pages: job.max_pages.max(1) as usize,
        map_always: false,
        extend_to: if job.directory.is_some() {
            0
        } else {
            crate::discovery::EXTEND_UP_TO_PAGES
        },
        max_pdfs: job.max_pdfs.max(0) as usize,
        directory: job.directory.is_some(),
        ai: Some(&ai),
        cancel: &CANCEL,
    };

    let mut stream = futures::stream::iter(hosts)
        .map(|(host, (urls, seed))| {
            let (settings, job, ai, geography, now, app, siblings) =
                (&settings, &job, &ai, &geography, &now, &app, &siblings);
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
                // Addresses the database already holds are not worth a model request.
                let keys: Vec<String> = collect_leads(&crawl.pages, now).0.into_keys().collect();
                let known = known_keys(app, keys).await;
                let shared = siblings.get(&host).map_or(&[][..], Vec::as_slice);
                let (leads, outbound, errors) = host_leads(
                    &crawl,
                    &site,
                    &seed,
                    job,
                    Some(ai),
                    geography,
                    now,
                    &known,
                    shared,
                )
                .await;
                (host, crawl, leads, outbound, errors, false)
            }
        })
        // Hosts waiting for the model do not hold a context.dev slot: a few more may be in flight.
        .buffer_unordered(concurrency + crate::lead_ai::MODEL_CONCURRENCY);

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
        let status = host_status(!crawl.pages.is_empty(), crawl.retry_later);
        let first_error = crawl.errors.first().cloned();
        let host_credits = crawl.credits;
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
            add_host_credits(&db, &persist_host, host_credits)?;
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
                credits_used: context.credits_used(),
                credits_remaining: context.credits_remaining(),
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
    let run_credits = context.credits_used();
    crate::database::blocking(move || {
        let db = open_db(&finish_app)?;
        finish_run(&db, &finish_run_id, status, page_count, pdf_found, &errors)?;
        db.execute(
            "UPDATE search_runs SET credits_used=?2 WHERE id=?1",
            params![finish_run_id, run_credits as i64],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
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
            credits_used: context.credits_used(),
            credits_remaining: context.credits_remaining(),
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
    ai_model: Option<String>,
    ai_effort: Option<String>,
) -> Result<crate::lead_ai::LeadPlan, String> {
    let request = request.trim().to_owned();
    if request.is_empty() {
        return Err("Descrivi cosa cerchi.".to_owned());
    }
    let taxonomy = crate::database::blocking(move || {
        crate::lead_ai::load_taxonomy(&*open_db(&app)?).map(crate::lead_ai::target_taxonomy)
    })
    .await?;
    let ai = LeadAi::new(
        taxonomy,
        ai_model.as_deref().unwrap_or(""),
        ai_effort.as_deref(),
    )
    .map_err(|error| format!("IA non disponibile: {error}"))?;
    let sources: Vec<_> = DIRECTORIES
        .iter()
        .map(|entry| (entry.id, entry.name, entry.note))
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
    /// Credits the run may spend at most (0: the settings' per-run cap).
    pub(crate) max_credits: u64,
    /// OpenCode choice of Settings; empty model: the one Settings shows.
    pub(crate) ai_model: Option<String>,
    pub(crate) ai_effort: Option<String>,
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
            max_credits: 0,
            ai_model: None,
            ai_effort: None,
        }
    }
}

/// What reading the next pending sites would cost, shown before starting.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ExploreEstimate {
    pub(crate) sites: usize,
    /// Map 1 + pages per site, plus the PDFs of school sites.
    pub(crate) max_credits: u64,
}

pub(crate) fn estimate_sites(seeds: &[SeedRow], max_pages: i64, max_pdfs: i64) -> ExploreEstimate {
    let schools = seeds
        .iter()
        .filter(|seed| seed.context.category.as_deref() == Some("Istruzione"))
        .count() as u64;
    ExploreEstimate {
        sites: seeds.len(),
        max_credits: seeds.len() as u64 * (1 + max_pages.max(1) as u64)
            + schools * max_pdfs.max(0) as u64,
    }
}

fn explore_filter(
    request: &ExploreRequest,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    (
        clean(request.list_name.clone()),
        clean(request.category.clone()),
        clean(request.subcategory.clone()),
        clean(request.regione.clone()),
    )
}

#[tauri::command]
pub(crate) async fn estimate_explore(
    app: tauri::AppHandle,
    request: ExploreRequest,
) -> Result<ExploreEstimate, String> {
    let (list_name, category, subcategory, regione) = explore_filter(&request);
    let limit = request.limit.clamp(1, MAX_EXPLORE_SITES);
    let (max_pages, max_pdfs) =
        crate::discovery::clamp_run_limits(Some(request.max_pages), Some(request.max_pdfs));
    crate::database::blocking(move || {
        let seeds = pending_seeds(
            &*open_db(&app)?,
            SeedFilter {
                list_name: list_name.as_deref(),
                category: category.as_deref(),
                subcategory: subcategory.as_deref(),
                regione: regione.as_deref(),
            },
            limit,
            &chrono::Utc::now().to_rfc3339(),
        )?;
        Ok(estimate_sites(&seeds, max_pages, max_pdfs))
    })
    .await
}

/// Crawl the next pending sites of a list (or of every list).
#[tauri::command]
pub(crate) async fn explore_seeds(
    app: tauri::AppHandle,
    request: ExploreRequest,
) -> Result<SearchRunSummary, String> {
    let (list_name, pick_category, pick_subcategory, pick_regione) = explore_filter(&request);
    let limit = request.limit.clamp(1, MAX_EXPLORE_SITES);
    let pick_app = app.clone();
    let pick_list = list_name.clone();
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
            directory: None,
            max_credits: request.max_credits,
            ai_model: request.ai_model.unwrap_or_default(),
            ai_effort: request.ai_effort,
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
    pub(crate) max_credits: u64,
    pub(crate) ai_model: Option<String>,
    pub(crate) ai_effort: Option<String>,
}

/// Crawl a directory (catalog entry or pasted URL): its emails become leads
/// with the directory taxonomy, its links to other sites become seeds.
#[tauri::command]
pub(crate) async fn explore_directory(
    app: tauri::AppHandle,
    request: DirectoryRequest,
) -> Result<SearchRunSummary, String> {
    let catalog = clean(request.source_id.clone()).map(|id| {
        DIRECTORIES
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("Elenco sconosciuto: {id}"))
    });
    let catalog = catalog.transpose()?;
    let (url, name, category, subcategory) = match catalog {
        Some(source) => (
            source.landing_url.to_owned(),
            source.name.to_owned(),
            Some(source.category.to_owned()),
            Some(source.subcategory.to_owned()),
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
            max_credits: request.max_credits,
            ai_model: request.ai_model.unwrap_or_default(),
            ai_effort: request.ai_effort,
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
    let context = open_context(&app, 0).await?;
    let (found, errors) = crate::lead_candidates::find_sites(&app, &context, &pending).await?;
    let report = crate::lead_candidates::DiscoveryReport {
        found: found.len(),
        credits: context.credits_used(),
        ..Default::default()
    };
    let stop = context.stop_reason();
    let (found, errors) = (std::sync::Arc::new(found), std::sync::Arc::new(errors));
    let readable = crate::database::blocking_patient(move || {
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
    #[test]
    fn host_stopped_midway_stays_pending() {
        assert_eq!(host_status(true, false), Some("crawled"));
        assert_eq!(host_status(true, true), None);
        assert_eq!(host_status(false, true), None);
        assert_eq!(host_status(false, false), Some("failed"));
    }

    #[test]
    fn runs_left_running_are_closed_on_startup() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let now = "2026-10-01T00:00:00Z";
        let record = RunRecord {
            kind: "crawl",
            query_text: "test",
            source_id: None,
            seed_count: 1,
            now,
        };
        let cut = create_run(&db, &record, "running").unwrap();
        let done = create_run(&db, &record, "completed").unwrap();
        db.execute(
            "INSERT INTO search_leads(id,run_id,email,email_comparison_key,retrieved_at,category)
             VALUES('l1',?1,'a@a.test','a@a.test',?2,'Istruzione')",
            params![cut, now],
        )
        .unwrap();
        assert_eq!(crate::discovery::close_orphaned_runs(&db).unwrap(), 1);
        let row = |id: &str| -> (String, i64) {
            db.query_row(
                "SELECT status, lead_count FROM search_runs WHERE id=?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(row(&cut), ("failed".to_owned(), 1));
        assert_eq!(row(&done).0, "completed");
    }

    use super::*;
    use crate::discovery::FetchedPage;
    use crate::schema::setup_schema;
    use std::collections::HashMap;

    #[test]
    fn lead_crawl_coverage_counts_distinct_sites_per_subcategory_without_catch_alls() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let now = "2026-09-29T10:00:00Z";
        let seed = |url: &str, regione: &str| SeedRow {
            url: Some(url.to_owned()),
            name: None,
            context: LeadContext {
                category: Some("Istruzione".to_owned()),
                subcategory: Some("Infanzia".to_owned()),
                regione: Some(regione.to_owned()),
                ..LeadContext::default()
            },
        };
        // Two lists share a host: one site. One site is read, one waits.
        insert_seeds(
            &db,
            "Base: Infanzia",
            &[seed("https://a.test", "Lombardia")],
            now,
        )
        .unwrap();
        insert_seeds(&db, "Altro", &[seed("https://a.test", "Lombardia")], now).unwrap();
        insert_seeds(
            &db,
            "Base: Infanzia",
            &[seed("https://b.test", "Veneto")],
            now,
        )
        .unwrap();
        mark_host(&db, "a.test", "crawled", 7, None, now).unwrap();
        add_host_credits(&db, "a.test", 12).unwrap();
        // Two addresses found on the sites of the subcategory: one is already a contact.
        let run = create_run(
            &db,
            &RunRecord {
                kind: "crawl",
                query_text: "test",
                source_id: None,
                seed_count: 1,
                now,
            },
            "completed",
        )
        .unwrap();
        for email in ["nuovo@a.test", "noto@a.test"] {
            db.execute(
                "INSERT INTO search_leads(id,run_id,email,email_comparison_key,retrieved_at,
                    category,subcategory,regione) VALUES(?1,?2,?3,?3,?4,'Istruzione','Infanzia','Lombardia')",
                params![uuid::Uuid::new_v4().to_string(), run, email, now],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,
                validation_notes,source_file,category,paese,email_comparison_key,is_pec)
             VALUES('c1','noto@a.test','noto','a','.test','valid','[]','t.csv','Istruzione','Italia','noto@a.test',0)",
            [],
        )
        .unwrap();
        let infanzia = subcategory_coverage(&db, None)
            .unwrap()
            .into_iter()
            .find(|row| row.subcategory == "Infanzia")
            .unwrap();
        assert_eq!((infanzia.credits, infanzia.new_leads), (12, 1));

        let find = |rows: &[SubcategoryCoverage]| {
            rows.iter()
                .find(|row| row.subcategory == "Infanzia")
                .map(|row| (row.sites, row.pending, row.crawled, row.leads))
        };
        assert_eq!(
            find(&subcategory_coverage(&db, None).unwrap()),
            Some((2, 1, 1, 14))
        );
        assert_eq!(
            find(&subcategory_coverage(&db, Some("Veneto")).unwrap()),
            Some((1, 1, 0, 0))
        );
        let all = subcategory_coverage(&db, None).unwrap();
        assert!(all.iter().all(|row| !row.subcategory.starts_with("Altro ")));
        assert!(all.iter().all(|row| row.category != "Varie"));
        assert!(all.iter().any(|row| row.subcategory == "Alpini"));
    }

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
    fn lead_crawl_estimate_counts_map_pages_and_school_pdfs() {
        let seed = |category: &str| SeedRow {
            url: Some("https://a.example.test".to_owned()),
            context: LeadContext {
                category: Some(category.to_owned()),
                ..LeadContext::default()
            },
            ..SeedRow::default()
        };
        let seeds = [
            seed("Istruzione"),
            seed("Enti religiosi"),
            seed("Associazioni"),
        ];
        // Map 1 + 3 pages each, plus 2 PDFs for the one school.
        assert_eq!(
            estimate_sites(&seeds, 3, 2),
            ExploreEstimate {
                sites: 3,
                max_credits: 14
            }
        );
        assert_eq!(estimate_sites(&[], 3, 0).max_credits, 0);
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
    async fn lead_crawl_model_answers_prevail_over_the_seed_and_drop_vendor_addresses() {
        use crate::ai_provider::testing::ScriptedProvider;
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let crawl = HostCrawl {
            pages: vec![FetchedPage {
                url: "https://www.comune.example.test/contatti".to_owned(),
                html: "<p>Cookie Policy sindaco@comune.example.test</p>\
                       <p>DPO esterno dpo@fornitore.example.test</p>"
                    .to_owned(),
                via: "archivio",
            }],
            ..HostCrawl::default()
        };
        // The seed is the library of the same comune: it must not label the mayor's address.
        let seed = SeedRow {
            url: Some("https://www.comune.example.test".to_owned()),
            name: Some("Biblioteca civica".to_owned()),
            context: LeadContext {
                category: Some("Cultura".to_owned()),
                subcategory: Some("Biblioteca".to_owned()),
                ..LeadContext::default()
            },
        };
        let job = CrawlJob {
            label: "test".to_owned(),
            kind: "crawl",
            source_id: None,
            max_pages: 5,
            max_pdfs: 0,
            directory: None,
            max_credits: 0,
            ai_model: String::new(),
            ai_effort: None,
        };
        let answer = serde_json::json!({"contacts": [
            {"email": "sindaco@comune.example.test", "organization": "Comune di Esempio",
             "role": "Sindaco", "person_name": null, "category": "Pubblica amministrazione",
             "subcategory": "Comune", "third_party": false},
            {"email": "dpo@fornitore.example.test", "organization": "Fornitore", "third_party": true},
        ]});
        let taxonomy = vec![(
            "Pubblica amministrazione".to_owned(),
            vec!["Comune".to_owned()],
        )];
        let ai = LeadAi::with_provider(
            std::sync::Arc::new(ScriptedProvider::new(vec![Ok(answer.to_string())])),
            taxonomy,
        );
        let (leads, _, errors) = host_leads(
            &crawl,
            "www.comune.example.test",
            &seed,
            &job,
            Some(&ai),
            &geography,
            "2026-09-29T10:00:00Z",
            &HashSet::new(),
            &[],
        )
        .await;
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(leads.len(), 1, "the vendor's address is dropped");
        let mayor = &leads[0];
        assert_eq!(mayor.email(), "sindaco@comune.example.test");
        assert_eq!(
            mayor.context.organization.as_deref(),
            Some("Comune di Esempio")
        );
        assert_eq!(
            mayor.context.category.as_deref(),
            Some("Pubblica amministrazione")
        );
        assert_eq!(mayor.context.subcategory.as_deref(), Some("Comune"));
        assert_eq!(
            mayor.role_and_person(),
            (Some("Sindaco"), None),
            "\"Cookie Policy\" is not a person"
        );
    }

    #[tokio::test]
    async fn lead_crawl_an_address_goes_to_the_seed_it_names_on_a_shared_site() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let crawl = HostCrawl {
            pages: vec![FetchedPage {
                url: "https://www.comune.example.test/contatti".to_owned(),
                html: "<p>biblioteca@comune.example.test</p>\
                       <p>ufficio.turistico@comune.example.test</p>\
                       <p>info@comune.example.test</p>"
                    .to_owned(),
                via: "archivio",
            }],
            ..HostCrawl::default()
        };
        let seed = |name: &str, category: &str, subcategory: &str, code: &str| SeedRow {
            url: None,
            name: Some(name.to_owned()),
            context: LeadContext {
                category: Some(category.to_owned()),
                subcategory: Some(subcategory.to_owned()),
                official_code: Some(code.to_owned()),
                ..LeadContext::default()
            },
        };
        let siblings = vec![
            seed(
                "Comune di Esempio",
                "Pubblica amministrazione",
                "Comune",
                "C1",
            ),
            seed(
                "Biblioteca civica Gigi Monticone",
                "Cultura",
                "Biblioteca",
                "B1",
            ),
            seed(
                "Ufficio turistico Esempio",
                "Turismo e promozione territoriale",
                "Ufficio turistico",
                "T1",
            ),
        ];
        let job = CrawlJob {
            label: "test".to_owned(),
            kind: "crawl",
            source_id: None,
            max_pages: 5,
            max_pdfs: 0,
            directory: None,
            max_credits: 0,
            ai_model: String::new(),
            ai_effort: None,
        };
        let (leads, _, errors) = host_leads(
            &crawl,
            "www.comune.example.test",
            &siblings[0],
            &job,
            None,
            &geography,
            "2026-09-29T10:00:00Z",
            &HashSet::new(),
            &siblings,
        )
        .await;
        assert!(errors.is_empty(), "{errors:?}");
        let by_email: HashMap<&str, &LeadAccumulator> =
            leads.iter().map(|lead| (lead.email(), lead)).collect();
        let library = &by_email["biblioteca@comune.example.test"].context;
        assert_eq!(library.subcategory.as_deref(), Some("Biblioteca"));
        assert_eq!(library.official_code.as_deref(), Some("B1"));
        assert_eq!(
            library.organization.as_deref(),
            Some("Biblioteca civica Gigi Monticone")
        );
        let tourism = &by_email["ufficio.turistico@comune.example.test"].context;
        assert_eq!(tourism.subcategory.as_deref(), Some("Ufficio turistico"));
        // Names nothing in particular: it stays with the first seed, the comune.
        let general = &by_email["info@comune.example.test"].context;
        assert_eq!(general.subcategory.as_deref(), Some("Comune"));
        assert_eq!(general.official_code.as_deref(), Some("C1"));
    }

    #[tokio::test]
    async fn lead_crawl_known_addresses_are_not_sent_to_the_model() {
        use crate::ai_provider::testing::ScriptedProvider;
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let crawl = HostCrawl {
            pages: vec![FetchedPage {
                url: "https://www.comune.example.test/contatti".to_owned(),
                html: "<p>Sindaco sindaco@comune.example.test</p>".to_owned(),
                via: "archivio",
            }],
            ..HostCrawl::default()
        };
        let seed = SeedRow::default();
        let job = CrawlJob {
            label: "test".to_owned(),
            kind: "crawl",
            source_id: None,
            max_pages: 5,
            max_pdfs: 0,
            directory: None,
            max_credits: 0,
            ai_model: String::new(),
            ai_effort: None,
        };
        let provider = std::sync::Arc::new(ScriptedProvider::new(Vec::new()));
        let ai = LeadAi::with_provider(provider.clone(), Vec::new());
        let known = HashSet::from(["sindaco@comune.example.test".to_owned()]);
        let (leads, _, errors) = host_leads(
            &crawl,
            "www.comune.example.test",
            &seed,
            &job,
            Some(&ai),
            &geography,
            "2026-09-29T10:00:00Z",
            &known,
            &[],
        )
        .await;
        assert_eq!((leads.len(), provider.request_count()), (1, 0));
        assert!(errors.is_empty(), "{errors:?}");
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
            directory: None,
            max_credits: 0,
            ai_model: String::new(),
            ai_effort: None,
        };
        let (leads, outbound, errors) = host_leads(
            &crawl,
            "www.parrocchia.example.test",
            &seed,
            &job,
            None,
            &geography,
            "2026-09-29T10:00:00Z",
            &HashSet::new(),
            &[],
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
