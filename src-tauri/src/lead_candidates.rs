//! New sites from the discovery channels, filtered before any reading credit.
//!
//! Every channel (links out of read sites, directories, Google Maps, web
//! search) hands its sites to `accept_candidates`: already known sites,
//! social networks, portals and mail providers are dropped; the rest is
//! classified by the deterministic rules, then by Jev in batches. Only a
//! confident classification becomes a `pending` seed; the rest is stored as
//! `verifica` and never read. Each channel's seeds go to its own list
//! (`Scoperta: <canale>`), and each discovery records its credits as a
//! `discovery` run, so the yield of a channel can be measured.

use crate::contextdev::{ContextDev, MapsPlace, SearchHit};
use crate::discovery::{LeadContext, RunRecord, create_run, finish_run, url_host};
use crate::lead_crawl::{
    SeedRow, insert_seeds_with_status, is_provider_domain, normalize_seed_url,
};
use crate::open_db;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub(crate) const CHANNEL_LINKS: &str = "Scoperta: link in uscita";
pub(crate) const CHANNEL_SEARCH: &str = "Scoperta: ricerca web";
pub(crate) const CHANNEL_MAPS: &str = "Scoperta: Google Maps";
/// Credits spent finding the site of organizations known only by name.
pub(crate) const CHANNEL_FIND_SITES: &str = "Scoperta: trova siti";
const MAX_SEARCH_QUERIES: usize = 120;
const DEFAULT_SEARCH_QUERIES: usize = 15;

/// A site a channel found.
#[derive(Debug, Clone, Default)]
pub(crate) struct Candidate {
    pub(crate) url: String,
    /// Page title, link text or the name Maps shows.
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    /// What the channel knows for sure: a directory's taxonomy, a Maps city.
    pub(crate) context: LeadContext,
    /// Category the channel suggests (the search, the Maps type): Jev may confirm it.
    pub(crate) hint: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct DiscoveryReport {
    pub(crate) found: usize,
    /// Already a seed, a contact's site or a contact's email domain.
    pub(crate) known: usize,
    /// Social networks, portals, providers.
    pub(crate) rejected: usize,
    /// New `pending` seeds.
    pub(crate) accepted: usize,
    /// Classified below the confidence threshold: stored, never read.
    pub(crate) to_verify: usize,
    pub(crate) credits: u64,
}

/// Hosts the database already knows, `www.` removed: every seed list, every
/// contact's site and every contact's email domain.
pub(crate) struct KnownSites(HashSet<String>);

impl KnownSites {
    pub(crate) fn load(db: &Connection) -> Result<Self, String> {
        let mut statement = db
            .prepare(
                "SELECT host FROM search_seeds WHERE host IS NOT NULL
                 UNION SELECT lower(email_domain||email_tld) FROM contacts
                   WHERE coalesce(email_domain,'')<>''
                 UNION SELECT website FROM contacts WHERE coalesce(website,'')<>''",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let mut hosts = HashSet::new();
        for row in rows {
            let value = row.map_err(|error| error.to_string())?;
            let host = normalize_seed_url(&value)
                .and_then(|url| url_host(&url))
                .unwrap_or_else(|| value.to_lowercase());
            hosts.insert(host.trim_start_matches("www.").to_owned());
        }
        Ok(Self(hosts))
    }

    /// The host or a parent domain of it (`biblioteca.comune.x.it` under `comune.x.it`).
    pub(crate) fn contains(&self, host: &str) -> bool {
        let host = host.trim_start_matches("www.");
        std::iter::successors(Some(host), |rest| {
            rest.split_once('.').map(|(_, tail)| tail)
        })
        .take_while(|suffix| suffix.contains('.'))
        .any(|suffix| self.0.contains(suffix))
    }
}

pub(crate) fn is_unwanted(host: &str) -> bool {
    crate::discovery::is_portal(host) || is_provider_domain(host.trim_start_matches("www."))
}

/// Taxonomy decided for a candidate: `(category, subcategory)` display names.
type Decision = Option<(String, Option<String>)>;

fn deterministic(
    catalog: Option<&crate::deterministic_rules::CompiledCatalog<'_>>,
    host: &str,
    candidate: &Candidate,
) -> Decision {
    let email = format!("info@{}", host.trim_start_matches("www."));
    let evidence = crate::deterministic_rules::DeterministicEvidence {
        email: &email,
        denomination: candidate.name.as_deref(),
        source_url: Some(&candidate.url),
    };
    let found = crate::deterministic_rules::evaluate_compiled(catalog?, evidence).ok()??;
    Some((
        found.result.category.to_owned(),
        found.result.subcategory.map(str::to_owned),
    ))
}

fn jev_context(candidate: &Candidate) -> String {
    let mut fields = serde_json::Map::new();
    fields.insert("Sito Web".to_owned(), candidate.url.clone().into());
    for (key, value) in [
        ("Denominazione", &candidate.name),
        ("Descrizione", &candidate.description),
        ("Comune", &candidate.context.comune),
        ("Provincia", &candidate.context.provincia),
        ("Regione", &candidate.context.regione),
    ] {
        if let Some(value) = value.as_deref().filter(|value| !value.trim().is_empty()) {
            fields.insert(key.to_owned(), value.trim().into());
        }
    }
    serde_json::json!([{ "source_id": "lead_discovery", "fields": fields }]).to_string()
}

/// Jev on the candidates no rule settled; `None` when Jev is not configured.
async fn jev_decisions(
    app: &tauri::AppHandle,
    pending: &[(usize, &Candidate)],
) -> Result<Option<HashMap<usize, (String, Option<String>)>>, String> {
    if pending.is_empty() {
        return Ok(Some(HashMap::new()));
    }
    let setup = crate::database::blocking({
        let app = app.clone();
        move || {
            let db = open_db(&app)?;
            let selection = crate::ai_selection::ProviderSelection::default();
            let Ok(client) = crate::ai_registry::build_typesafe_client(&db, &selection, 120) else {
                return Ok(None);
            };
            let taxonomy = crate::taxonomy::load_active_taxonomy(&db)?;
            let names = crate::ai_registry::load_taxonomy_display_names(&db, taxonomy.id)?;
            Ok(Some((
                client,
                taxonomy.categories,
                names,
                selection.spend_policy(),
            )))
        }
    })
    .await?;
    let Some((client, taxonomy, names, policy)) = setup else {
        return Ok(None);
    };
    let items = pending
        .iter()
        .map(|(index, candidate)| crate::ai_batch::BatchContactItem {
            id: index.to_string(),
            country: "IT".to_owned(),
            source_context: jev_context(candidate),
            previous_errors: Vec::new(),
            current_category: candidate.hint.clone(),
        })
        .collect();
    let judged = crate::ai_cascade::jev_classify(&client, items, &taxonomy, &names, policy)
        .await
        .map_err(|error| format!("Jev: {}", error.message))?;
    let display = |key: (String, Option<String>, String)| names.get(&key).cloned().unwrap_or(key.2);
    Ok(Some(
        judged
            .into_iter()
            .filter_map(|(id, (category, subcategory))| {
                let label = display(("category".to_owned(), None, category.clone()));
                let sub =
                    subcategory.map(|sub| display(("subcategory".to_owned(), Some(category), sub)));
                Some((id.parse().ok()?, (label, sub)))
            })
            .collect(),
    ))
}

/// Filter, classify and store the candidates of one channel into `list_name`.
pub(crate) async fn accept_candidates(
    app: &tauri::AppHandle,
    list_name: &str,
    candidates: Vec<Candidate>,
) -> Result<DiscoveryReport, String> {
    let mut report = DiscoveryReport {
        found: candidates.len(),
        ..DiscoveryReport::default()
    };
    let (known, catalog) = crate::database::blocking({
        let app = app.clone();
        move || {
            let db = open_db(&app)?;
            let catalog: Option<String> = db
                .query_row(
                    "SELECT id FROM deterministic_rule_catalogs WHERE status='active'",
                    [],
                    |row| row.get(0),
                )
                .ok();
            let catalog = catalog
                .map(|id| crate::deterministic_rules::catalog_by_id(&db, &id))
                .transpose()?;
            Ok((KnownSites::load(&db)?, catalog))
        }
    })
    .await?;
    let compiled = catalog.as_ref().map(|catalog| catalog.compile());

    let mut hosts = HashSet::new();
    let mut fresh: Vec<(Candidate, String)> = Vec::new();
    for mut candidate in candidates {
        let Some(url) = normalize_seed_url(&candidate.url) else {
            report.rejected += 1;
            continue;
        };
        let Some(host) = url_host(&url) else {
            report.rejected += 1;
            continue;
        };
        if known.contains(&host) || !hosts.insert(host.trim_start_matches("www.").to_owned()) {
            report.known += 1;
        } else if is_unwanted(&host) || crate::discovery::is_internal_host(&host) {
            report.rejected += 1;
        } else {
            candidate.url = url;
            fresh.push((candidate, host));
        }
    }

    let mut decisions: Vec<Decision> = fresh
        .iter()
        .map(|(candidate, host)| match &candidate.context.category {
            // A directory's own taxonomy is a deterministic fact about its entries.
            Some(category) => Some((category.clone(), candidate.context.subcategory.clone())),
            None => deterministic(compiled.as_ref(), host, candidate),
        })
        .collect();
    let undecided: Vec<(usize, &Candidate)> = fresh
        .iter()
        .enumerate()
        .filter(|(index, _)| decisions[*index].is_none())
        .map(|(index, (candidate, _))| (index, candidate))
        .collect();
    if let Some(judged) = jev_decisions(app, &undecided).await? {
        for (index, decision) in judged {
            decisions[index] = Some(decision);
        }
    }

    let (mut accepted, mut to_verify) = (Vec::new(), Vec::new());
    for ((candidate, _), decision) in fresh.into_iter().zip(decisions) {
        let mut context = candidate.context;
        let target = if let Some((category, subcategory)) = decision {
            context.category = Some(category);
            context.subcategory = subcategory;
            &mut accepted
        } else {
            &mut to_verify
        };
        target.push(SeedRow {
            url: Some(candidate.url),
            name: candidate.name,
            context,
        });
    }
    let list_name = list_name.to_owned();
    let (accepted, to_verify) = (
        std::sync::Arc::new(accepted),
        std::sync::Arc::new(to_verify),
    );
    let (inserted, held) = crate::database::blocking_patient({
        let app = app.clone();
        move || {
            let db = open_db(&app)?;
            let now = chrono::Utc::now().to_rfc3339();
            Ok((
                insert_seeds_with_status(&db, &list_name, &accepted, "pending", &now)?,
                insert_seeds_with_status(&db, &list_name, &to_verify, "verifica", &now)?,
            ))
        }
    })
    .await?;
    report.accepted = inserted;
    report.to_verify = held;
    Ok(report)
}

/// A finished `discovery` run: what a channel cost and how many seeds it added.
pub(crate) fn record_discovery(
    db: &Connection,
    channel: &str,
    report: &DiscoveryReport,
    errors: &[String],
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let run_id = create_run(
        db,
        &RunRecord {
            kind: "discovery",
            query_text: channel,
            source_id: None,
            seed_count: report.accepted as i64,
            now: &now,
        },
        "running",
    )?;
    finish_run(db, &run_id, "completed", 0, 0, errors)?;
    db.execute(
        "UPDATE search_runs SET credits_used=?2 WHERE id=?1",
        params![run_id, report.credits as i64],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// The searches of a request: the free text, plus `{sottocategoria} {provincia}`
/// for each province of the region when the request names both.
pub(crate) fn search_queries(request: &SearchDiscoveryRequest) -> Vec<String> {
    let mut queries: Vec<String> = request
        .query
        .iter()
        .map(|query| query.trim().to_owned())
        .filter(|query| !query.is_empty())
        .collect();
    if let (Some(subcategory), Some(regione)) = (&request.subcategory, &request.regione) {
        for (provincia, region) in crate::parsing::PROVINCE_REGIONS {
            if region.eq_ignore_ascii_case(regione.trim()) {
                queries.push(format!("{} {provincia}", subcategory.trim()));
            }
        }
    }
    let cap = if request.max_queries == 0 {
        DEFAULT_SEARCH_QUERIES
    } else {
        request.max_queries.min(MAX_SEARCH_QUERIES)
    };
    queries.dedup();
    queries.truncate(cap);
    queries
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SearchDiscoveryRequest {
    pub(crate) query: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) subcategory: Option<String>,
    pub(crate) regione: Option<String>,
    /// Searches at most (one credit each); 0 for the default.
    pub(crate) max_queries: usize,
    /// Credits at most for this discovery; 0 for no cap beyond the searches.
    pub(crate) max_credits: u64,
}

/// Channel 4: web search (one credit every ten results) into new seeds.
#[tauri::command]
pub(crate) async fn discover_by_search(
    app: tauri::AppHandle,
    request: SearchDiscoveryRequest,
) -> Result<DiscoveryReport, String> {
    let queries = search_queries(&request);
    if queries.is_empty() {
        return Err("Scrivi cosa cercare o scegli sottocategoria e regione.".to_owned());
    }
    let _job = crate::lead_crawl::start_job()?;
    let context = crate::lead_crawl::open_context(&app, request.max_credits).await?;
    let mut candidates = Vec::new();
    let mut errors = Vec::new();
    for query in &queries {
        if crate::lead_crawl::cancelled() {
            break;
        }
        match context.search(query).await {
            Ok(hits) => candidates.extend(hits.into_iter().map(|hit| Candidate {
                url: hit.url,
                name: (!hit.title.trim().is_empty()).then_some(hit.title),
                description: (!hit.description.trim().is_empty()).then_some(hit.description),
                hint: request.category.clone(),
                ..Candidate::default()
            })),
            Err(error) => {
                errors.push(format!("{query}: {error}"));
                if context.stop_reason().is_some() {
                    break;
                }
            }
        }
    }
    let mut report = accept_candidates(&app, CHANNEL_SEARCH, candidates).await?;
    report.credits = context.credits_used();
    let errors = std::sync::Arc::new(errors);
    crate::database::blocking_patient(move || {
        record_discovery(&*open_db(&app)?, CHANNEL_SEARCH, &report, &errors)
    })
    .await?;
    Ok(report)
}

/// What one discovery channel cost and returned: the plan's "crediti per lead utile".
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ChannelYield {
    pub(crate) channel: String,
    pub(crate) discovery_credits: i64,
    pub(crate) reading_credits: i64,
    pub(crate) seeds: i64,
    /// Addresses of the channel's sites not already in contacts, automatic
    /// mailboxes excluded.
    // shortcut: mailbox routing (MX) is not checked here; join the DNS cache
    // when yields start deciding which channel to switch off.
    pub(crate) new_emails: i64,
}

pub(crate) fn channel_yields(db: &Connection) -> Result<Vec<ChannelYield>, String> {
    let mut statement = db
        .prepare(
            "WITH channels(channel) AS (
                SELECT list_name FROM search_seeds
                 WHERE list_name LIKE 'Scoperta: %' OR list_name LIKE 'Elenco: %'
                UNION SELECT query_text FROM search_runs WHERE kind='discovery'
             )
             SELECT c.channel,
                (SELECT coalesce(SUM(credits_used),0) FROM search_runs
                  WHERE kind IN ('discovery','directory') AND query_text=c.channel),
                (SELECT coalesce(SUM(credits_used),0) FROM search_seeds WHERE list_name=c.channel),
                (SELECT COUNT(*) FROM search_seeds WHERE list_name=c.channel),
                (SELECT COUNT(DISTINCT l.email_comparison_key) FROM search_leads l
                  JOIN search_seeds s ON s.list_name=c.channel AND l.website='https://'||s.host
                  WHERE NOT EXISTS (SELECT 1 FROM contacts k
                                    WHERE k.email_comparison_key=l.email_comparison_key)
                    AND l.email NOT LIKE 'noreply%' AND l.email NOT LIKE 'no-reply%'
                    AND l.email NOT LIKE 'donotreply%' AND l.email NOT LIKE 'mailer-daemon%'
                    AND l.email NOT LIKE 'postmaster%')
             FROM channels c ORDER BY c.channel",
        )
        .map_err(|error| error.to_string())?;
    statement
        .query_map([], |row| {
            Ok(ChannelYield {
                channel: row.get(0)?,
                discovery_credits: row.get(1)?,
                reading_credits: row.get(2)?,
                seeds: row.get(3)?,
                new_emails: row.get(4)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn list_channel_yields(
    app: tauri::AppHandle,
) -> Result<Vec<ChannelYield>, String> {
    crate::database::blocking(move || channel_yields(&*open_db(&app)?)).await
}

const NAME_STOPWORDS: [&str; 24] = [
    "parrocchia",
    "associazione",
    "comune",
    "scuola",
    "istituto",
    "comprensivo",
    "della",
    "delle",
    "degli",
    "dello",
    "dei",
    "del",
    "san",
    "santa",
    "santo",
    "santi",
    "pro",
    "loco",
    "gruppo",
    "sezione",
    "circolo",
    "centro",
    "club",
    "onlus",
];

/// §8: a search result is this organization's site only when its title or
/// description names the comune or a significant word of the name. A wrong
/// site would attribute right addresses to the wrong organization.
pub(crate) fn hit_matches_entity(name: &str, comune: &str, hit: &SearchHit) -> bool {
    let text = crate::gazetteer::norm(&format!("{} {}", hit.title, hit.description));
    let words: HashSet<&str> = text.split_whitespace().collect();
    let comune = crate::gazetteer::norm(comune);
    if !comune.is_empty() && text.contains(&comune) {
        return true;
    }
    crate::gazetteer::norm(name)
        .split_whitespace()
        .filter(|word| word.chars().count() >= 4 && !NAME_STOPWORDS.contains(word))
        .any(|word| words.contains(word))
}

fn maps_enabled(app: &tauri::AppHandle) -> Result<bool, String> {
    crate::contextdev::maps_enabled(&*open_db(app)?)
}

/// Channel 3a: Google Maps searches (off by default) into new seeds. A place's
/// city is structured, so it becomes the seed's comune.
#[tauri::command]
pub(crate) async fn discover_by_maps(
    app: tauri::AppHandle,
    request: SearchDiscoveryRequest,
) -> Result<DiscoveryReport, String> {
    let enabled = crate::database::blocking({
        let app = app.clone();
        move || maps_enabled(&app)
    })
    .await?;
    if !enabled {
        return Err("Attiva Google Maps nelle Impostazioni per usarlo.".to_owned());
    }
    let queries = search_queries(&request);
    if queries.is_empty() {
        return Err("Scrivi cosa cercare o scegli sottocategoria e regione.".to_owned());
    }
    let _job = crate::lead_crawl::start_job()?;
    let context = crate::lead_crawl::open_context(&app, request.max_credits).await?;
    let mut candidates = Vec::new();
    let mut errors = Vec::new();
    for query in &queries {
        if crate::lead_crawl::cancelled() {
            break;
        }
        match context.scrape_maps(query).await {
            Ok(places) => candidates.extend(places.into_iter().filter_map(|place| {
                Some(Candidate {
                    url: place.website?,
                    name: place.name,
                    description: Some(place.google_category.join(", ")),
                    context: LeadContext {
                        comune: place.city,
                        address: place.address,
                        postal_code: place.postal_code,
                        ..LeadContext::default()
                    },
                    hint: request.category.clone(),
                })
            })),
            Err(error) => {
                errors.push(format!("{query}: {error}"));
                if context.stop_reason().is_some() {
                    break;
                }
            }
        }
    }
    let mut report = accept_candidates(&app, CHANNEL_MAPS, candidates).await?;
    report.credits = context.credits_used();
    let errors = std::sync::Arc::new(errors);
    crate::database::blocking_patient(move || {
        record_discovery(&*open_db(&app)?, CHANNEL_MAPS, &report, &errors)
    })
    .await?;
    Ok(report)
}

/// What `find_seed_sites` decides for one organization: the site to read,
/// a site to verify, or nothing found.
#[derive(Debug, PartialEq)]
pub(crate) enum SiteMatch {
    Found(String),
    ToVerify(String),
    NotFound,
}

pub(crate) fn choose_site(name: &str, comune: &str, hits: &[SearchHit]) -> SiteMatch {
    let usable: Vec<&SearchHit> = hits
        .iter()
        .filter(|hit| url_host(&hit.url).is_some_and(|host| !is_unwanted(&host)))
        .collect();
    if let Some(hit) = usable
        .iter()
        .find(|hit| hit_matches_entity(name, comune, hit))
    {
        return SiteMatch::Found(hit.url.clone());
    }
    usable.first().map_or(SiteMatch::NotFound, |hit| {
        SiteMatch::ToVerify(hit.url.clone())
    })
}

/// Maps check is structural: the place's city must be the organization's
/// comune and its name must share a significant word with the known name.
pub(crate) fn choose_maps_site(name: &str, comune: &str, places: &[MapsPlace]) -> SiteMatch {
    let with_site: Vec<&MapsPlace> = places
        .iter()
        .filter(|place| {
            place
                .website
                .as_deref()
                .and_then(url_host)
                .is_some_and(|host| !is_unwanted(&host))
        })
        .collect();
    let comune = crate::gazetteer::norm(comune);
    let matching = with_site.iter().find(|place| {
        let city = crate::gazetteer::norm(place.city.as_deref().unwrap_or(""));
        let as_hit = SearchHit {
            url: String::new(),
            title: place.name.clone().unwrap_or_default(),
            description: String::new(),
        };
        !comune.is_empty() && city == comune && hit_matches_entity(name, "", &as_hit)
    });
    match (matching, with_site.first()) {
        (Some(place), _) => SiteMatch::Found(place.website.clone().unwrap_or_default()),
        (None, Some(place)) => SiteMatch::ToVerify(place.website.clone().unwrap_or_default()),
        (None, None) => SiteMatch::NotFound,
    }
}

/// Channel 3b: the site of organizations known only by name, through Google
/// Maps when it is enabled, otherwise web search.
pub(crate) async fn find_sites(
    app: &tauri::AppHandle,
    context: &ContextDev,
    pending: &[(String, String, String)],
) -> Result<(Vec<(String, SiteMatch)>, Vec<String>), String> {
    let use_maps = crate::database::blocking({
        let app = app.clone();
        move || maps_enabled(&app)
    })
    .await?;
    if !use_maps {
        return Ok(find_sites_by_search(context, pending).await);
    }
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for (id, name, comune) in pending {
        if crate::lead_crawl::cancelled() || context.stop_reason().is_some() {
            break;
        }
        let query = format!("{name} {comune}").trim().to_owned();
        match context.scrape_maps(&query).await {
            Ok(places) => found.push((id.clone(), choose_maps_site(name, comune, &places))),
            Err(error) if error.is_site_error() => found.push((id.clone(), SiteMatch::NotFound)),
            Err(error) => errors.push(format!("{query}: {error}")),
        }
    }
    Ok((found, errors))
}

/// Channel 3b (web search): the site of organizations known only by name,
/// searched as `<name> <comune>` and checked against §8 before it is read.
pub(crate) async fn find_sites_by_search(
    context: &ContextDev,
    pending: &[(String, String, String)],
) -> (Vec<(String, SiteMatch)>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for (id, name, comune) in pending {
        if crate::lead_crawl::cancelled() || context.stop_reason().is_some() {
            break;
        }
        let query = format!("{name} {comune}").trim().to_owned();
        match context.search(&query).await {
            Ok(hits) => found.push((id.clone(), choose_site(name, comune, &hits))),
            Err(error) if error.is_site_error() => found.push((id.clone(), SiteMatch::NotFound)),
            Err(error) => errors.push(format!("{query}: {error}")),
        }
    }
    (found, errors)
}

/// Store the sites found for name-only seeds: a verified site becomes
/// readable, an unverified one waits as `verifica`, none marks the seed failed.
pub(crate) fn store_found_sites(
    db: &Connection,
    found: &[(String, SiteMatch)],
) -> Result<usize, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut readable = 0;
    for (id, site) in found {
        let (url, status, error) = match site {
            SiteMatch::Found(url) => (normalize_seed_url(url), "pending", None),
            SiteMatch::ToVerify(url) => (
                normalize_seed_url(url),
                "verifica",
                Some("sito da verificare"),
            ),
            SiteMatch::NotFound => (None, "failed", Some("sito non trovato")),
        };
        let changed = match &url {
            Some(url) => db
                .execute(
                    "UPDATE OR IGNORE search_seeds SET url=?2, host=?3, status=?4, error=?5
                     WHERE id=?1 AND url IS NULL",
                    params![id, url, url_host(url), status, error],
                )
                .map_err(|error| error.to_string())?,
            None => 0,
        };
        if changed == 0 {
            db.execute(
                "UPDATE search_seeds SET status='failed', error=?2, crawled_at=?3
                 WHERE id=?1 AND url IS NULL",
                params![id, error.unwrap_or("sito già in elenco"), now],
            )
            .map_err(|error| error.to_string())?;
        } else if status == "pending" {
            readable += 1;
        }
    }
    Ok(readable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::setup_schema;

    fn hit(url: &str, title: &str, description: &str) -> SearchHit {
        SearchHit {
            url: url.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
        }
    }

    #[test]
    fn lead_candidates_known_sites_cover_seeds_contacts_and_parent_domains() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let (local, domain, tld) = crate::parsing::email_parts("info@comune.esempio.test");
        db.execute(
            "INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,
                validation_notes,source_file,email_comparison_key,is_pec,website,category,subcategory)
             VALUES('c1','info@comune.esempio.test',?1,?2,?3,'valid','[]','t.csv',
                    'info@comune.esempio.test',0,'www.museo.example.test/visita',
                    'Pubblica amministrazione','Comune')",
            params![local, domain, tld],
        )
        .unwrap();
        insert_seeds_with_status(
            &db,
            "Elenco",
            &[SeedRow {
                url: Some("https://www.parrocchia.example.test".to_owned()),
                ..SeedRow::default()
            }],
            "pending",
            "2026-10-25T10:00:00Z",
        )
        .unwrap();
        let known = KnownSites::load(&db).unwrap();
        for host in [
            "parrocchia.example.test",
            "www.comune.esempio.test",
            "biblioteca.comune.esempio.test",
            "museo.example.test",
        ] {
            assert!(known.contains(host), "{host}");
        }
        assert!(!known.contains("proloco.example.test"));
        assert!(!known.contains("esempio.test"));
    }

    #[test]
    fn lead_candidates_discovery_runs_record_their_credits() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let report = DiscoveryReport {
            accepted: 4,
            credits: 12,
            ..DiscoveryReport::default()
        };
        record_discovery(&db, CHANNEL_SEARCH, &report, &[]).unwrap();
        let (kind, seeds, credits): (String, i64, i64) = db
            .query_row(
                "SELECT kind, seed_count, credits_used FROM search_runs WHERE query_text=?1",
                [CHANNEL_SEARCH],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((kind.as_str(), seeds, credits), ("discovery", 4, 12));
    }

    #[test]
    fn lead_candidates_channel_yield_counts_credits_and_new_addresses() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let now = "2026-10-25T10:00:00Z";
        insert_seeds_with_status(
            &db,
            CHANNEL_SEARCH,
            &[SeedRow {
                url: Some("https://proloco.example.test".to_owned()),
                ..SeedRow::default()
            }],
            "pending",
            now,
        )
        .unwrap();
        db.execute("UPDATE search_seeds SET credits_used=4", [])
            .unwrap();
        record_discovery(
            &db,
            CHANNEL_SEARCH,
            &DiscoveryReport {
                accepted: 1,
                credits: 2,
                ..DiscoveryReport::default()
            },
            &[],
        )
        .unwrap();
        let run: String = db
            .query_row("SELECT id FROM search_runs", [], |row| row.get(0))
            .unwrap();
        for email in ["info@proloco.example.test", "noreply@proloco.example.test"] {
            db.execute(
                "INSERT INTO search_leads(id,run_id,email,email_comparison_key,retrieved_at,website)
                 VALUES(?1,?2,?3,?3,?4,'https://proloco.example.test')",
                params![email, run, email, now],
            )
            .unwrap();
        }
        assert_eq!(
            channel_yields(&db).unwrap(),
            [ChannelYield {
                channel: CHANNEL_SEARCH.to_owned(),
                discovery_credits: 2,
                reading_credits: 4,
                seeds: 1,
                new_emails: 1,
            }]
        );
    }

    #[test]
    fn lead_candidates_search_queries_add_one_per_province_of_the_region() {
        let request = SearchDiscoveryRequest {
            query: Some(" pro loco Lombardia contatti ".to_owned()),
            subcategory: Some("Pro Loco".to_owned()),
            regione: Some("lombardia".to_owned()),
            max_queries: 50,
            ..SearchDiscoveryRequest::default()
        };
        let queries = search_queries(&request);
        assert_eq!(queries[0], "pro loco Lombardia contatti");
        assert!(
            queries.contains(&"Pro Loco Bergamo".to_owned()),
            "{queries:?}"
        );
        assert_eq!(queries.len(), 13, "{queries:?}");
        let capped = search_queries(&SearchDiscoveryRequest {
            max_queries: 3,
            ..request
        });
        assert_eq!(capped.len(), 3);
        assert!(search_queries(&SearchDiscoveryRequest::default()).is_empty());
    }

    #[test]
    fn lead_candidates_booking_pages_are_never_an_organizations_site() {
        let place = |website: &str| MapsPlace {
            name: Some("Pro Loco Esempio".to_owned()),
            city: Some("Esempio".to_owned()),
            website: Some(website.to_owned()),
            ..MapsPlace::default()
        };
        assert_eq!(
            choose_maps_site(
                "Pro Loco Esempio",
                "Esempio",
                &[place("https://www.booking.com/Share-abc")]
            ),
            SiteMatch::NotFound
        );
    }

    #[test]
    fn lead_candidates_name_searches_accept_only_matching_sites() {
        let hits = [
            hit("https://www.facebook.com/proloco", "Pro Loco Clusone", ""),
            hit(
                "https://www.ecodibergamo.example.test/articolo",
                "Festa in piazza",
                "Notizie",
            ),
            hit(
                "https://www.prolococlusone.example.test/",
                "Pro Loco Clusone",
                "Eventi a Clusone",
            ),
        ];
        assert_eq!(
            choose_site("Pro Loco Clusone", "Clusone", &hits),
            SiteMatch::Found("https://www.prolococlusone.example.test/".to_owned())
        );
        // Only generic words in common: the site waits for a person to check it.
        assert_eq!(
            choose_site("Parrocchia San Marco", "Bergamo", &hits[1..2]),
            SiteMatch::ToVerify("https://www.ecodibergamo.example.test/articolo".to_owned())
        );
        assert_eq!(
            choose_site("Pro Loco Clusone", "Clusone", &hits[..1]),
            SiteMatch::NotFound
        );
        assert!(hit_matches_entity(
            "Parrocchia di Sant'Alessandro",
            "",
            &hit("https://x.example.test", "Sant'Alessandro in Colonna", "")
        ));
    }

    #[test]
    fn lead_candidates_maps_accepts_a_place_only_in_the_same_comune() {
        let place = |name: &str, city: &str, website: &str| MapsPlace {
            name: Some(name.to_owned()),
            city: Some(city.to_owned()),
            website: Some(website.to_owned()),
            ..MapsPlace::default()
        };
        let places = [
            place(
                "Pro Loco Clusone",
                "Rovetta",
                "https://rovetta.example.test/",
            ),
            place(
                "Pro Loco Clusone",
                "Clusone",
                "https://prolococlusone.example.test/",
            ),
        ];
        assert_eq!(
            choose_maps_site("Pro Loco Clusone", "Clusone", &places),
            SiteMatch::Found("https://prolococlusone.example.test/".to_owned())
        );
        assert_eq!(
            choose_maps_site("Pro Loco Clusone", "Clusone", &places[..1]),
            SiteMatch::ToVerify("https://rovetta.example.test/".to_owned())
        );
        // Without the comune the city cannot be checked.
        assert!(matches!(
            choose_maps_site("Pro Loco Clusone", "", &places),
            SiteMatch::ToVerify(_)
        ));
        assert_eq!(choose_maps_site("x", "Clusone", &[]), SiteMatch::NotFound);
    }

    #[test]
    fn lead_candidates_found_sites_update_name_only_seeds() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let now = "2026-10-25T10:00:00Z";
        let named = |name: &str| SeedRow {
            name: Some(name.to_owned()),
            ..SeedRow::default()
        };
        insert_seeds_with_status(
            &db,
            "Nomi",
            &[named("A"), named("B"), named("C")],
            "pending",
            now,
        )
        .unwrap();
        let id = |name: &str| -> String {
            db.query_row("SELECT id FROM search_seeds WHERE name=?1", [name], |row| {
                row.get(0)
            })
            .unwrap()
        };
        let found = vec![
            (
                id("A"),
                SiteMatch::Found("https://a.example.test".to_owned()),
            ),
            (
                id("B"),
                SiteMatch::ToVerify("https://b.example.test".to_owned()),
            ),
            (id("C"), SiteMatch::NotFound),
        ];
        assert_eq!(store_found_sites(&db, &found).unwrap(), 1);
        let status = |name: &str| -> (String, Option<String>) {
            db.query_row(
                "SELECT status, host FROM search_seeds WHERE name=?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(
            status("A"),
            ("pending".to_owned(), Some("a.example.test".to_owned()))
        );
        assert_eq!(
            status("B"),
            ("verifica".to_owned(), Some("b.example.test".to_owned()))
        );
        assert_eq!(status("C"), ("failed".to_owned(), None));
    }
}
