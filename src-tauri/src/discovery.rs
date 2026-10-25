//! Sandboxed web discovery.
//!
//! Discovery runs and their leads live in `search_runs` / `search_leads`
//! (schema migration 47). Crawl and open-data import never touch `contacts`.
//! Promoting a finished run into the contacts database is an explicit
//! user action (`promote_search_run_to_contacts`).

use crate::imports::publish_export;
use crate::parsing::{
    clean_email, decode_html_entities, email_comparison_key, is_pec_email, is_valid_email_syntax,
    normalize_obfuscated_email_text_with_method,
};
use crate::{
    EMAIL_RE, ExportReport, FieldFilter, MAX_PAGE_SIZE, SearchLeadList, SearchLeadView,
    SearchRunSummary, open_db,
};
use regex::Regex;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

mod archive;
mod pdf;
mod robots;
mod store;

use archive::*;
use pdf::*;
use robots::*;
pub(crate) use store::*;

pub(crate) const MAX_SEEDS: usize = 10;
pub(crate) const MAX_PAGES_PER_DOMAIN: i64 = 20;
pub(crate) const DEFAULT_MAX_PAGES_PER_DOMAIN: i64 = 20;
pub(crate) const MAX_PDF_LINKS: i64 = 100;
pub(crate) const DEFAULT_MAX_PDFS: i64 = 20;
#[cfg(test)]
const FETCH_TIMEOUT_SECONDS: u64 = 30;
const FETCH_PAUSE_MILLIS: u64 = 1500;
const BACKOFF_PAUSE_MILLIS: u64 = 10_000;
const USER_AGENT: &str = "DatabaseContatti/0.1 (+https://github.com/JustMarkDev/database-contatti)";
const ROBOTS_TOKEN: &str = "databasecontatti";
const MAX_CRAWL_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
const MAX_SITEMAP_FILES: usize = 5;
const MAX_HOST_ERRORS: usize = 3;
/// Hosts crawled at the same time; each host stays sequential.
pub(crate) const HOST_CONCURRENCY: usize = 6;
const MAX_PDF_BYTES: usize = 8_000_000;
/// Per-stream cap on decompressed PDF content, against decompression bombs.
const MAX_PDF_DECOMPRESSED_BYTES: usize = 64_000_000;
const MAX_HTML_BYTES: usize = 4_000_000;
const CONTEXT_RADIUS: usize = 160;

static SCRIPT_OR_STYLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>|<style\b[^>]*>.*?</style\s*>")
        .expect("valid script/style regex")
});
static ANY_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").expect("valid tag regex"));
static HREF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)href\s*=\s*["']([^"']+)["']"#).expect("valid href regex"));
static MAILTO_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)mailto:([^"'>?\s]+)"#).expect("valid mailto regex"));
static CONTACT_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(contatt|referen|bullism|cyberbullism|chi-siamo|organizzazione|struttura|uffici|persone|team|staff|dirigen|docenti|scuole|servizi|amministrazione|trasparenza|pec|protocollo)",
    )
    .expect("valid contact path regex")
});
static ROLE_LABEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(referente|responsabile|coordinatore|coordinatrice|dirigente|preside|docente|tutor)[^\n\r:;.,\-–—]{0,48}",
    )
    .expect("valid role label regex")
});
static SITEMAP_LOC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<loc>\s*(?:<!\[CDATA\[)?\s*(.*?)\s*(?:\]\]>)?\s*</loc>")
        .expect("valid sitemap loc regex")
});
static PERSON_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-ZÀ-ÖØ-Ý][a-zà-öø-ÿ']{1,24})\s+([A-ZÀ-ÖØ-Ý][a-zà-öø-ÿ']{1,24})\b")
        .expect("valid person name regex")
});

const NAME_STOPWORDS: [&str; 20] = [
    "referente",
    "bullismo",
    "cyberbullismo",
    "scuola",
    "istituto",
    "comune",
    "regione",
    "lombardia",
    "contatti",
    "servizio",
    "ufficio",
    "dirigente",
    "docente",
    "preside",
    "tutor",
    "responsabile",
    "coordinatore",
    "posta",
    "protocollo",
    "italia",
];

#[derive(Debug, Clone)]
pub(crate) struct FetchedPage {
    pub(crate) url: String,
    pub(crate) html: String,
    /// `sito` (live), `tinyfish` (rendered after a block) or `archivio`
    /// (Wayback Machine copy after a block).
    pub(crate) via: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct EmailHit {
    pub(crate) email: String,
    pub(crate) context: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedPage {
    pub(crate) text: String,
    pub(crate) emails: Vec<EmailHit>,
    pub(crate) pdf_urls: Vec<String>,
    pub(crate) links: Vec<String>,
}

pub(crate) struct LeadAccumulator {
    email: String,
    pub(crate) comparison_key: String,
    source_url: String,
    pages: BTreeSet<String>,
    retrieved_at: String,
    is_pec: bool,
    role: Option<String>,
    person_name: Option<String>,
    pub(crate) context: LeadContext,
    /// How the address was found: `open_data`, `sito`, `tinyfish`, `archivio`,
    /// `elenco`, with `+ia` when the model filled fields.
    pub(crate) method: String,
}

/// Taxonomy, organization and geography known up front for a lead. Open-data
/// sources fill it; web discovery leaves it empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LeadContext {
    pub(crate) category: Option<String>,
    pub(crate) subcategory: Option<String>,
    pub(crate) organization: Option<String>,
    pub(crate) comune: Option<String>,
    pub(crate) provincia: Option<String>,
    pub(crate) regione: Option<String>,
    pub(crate) website: Option<String>,
    pub(crate) official_code: Option<String>,
    pub(crate) department: Option<String>,
    pub(crate) address: Option<String>,
    pub(crate) postal_code: Option<String>,
    pub(crate) paese: Option<String>,
}

impl LeadContext {
    /// Fill every empty field from `other`; values already known win.
    pub(crate) fn fill_from(&mut self, other: &LeadContext) {
        for (target, value) in [
            (&mut self.category, &other.category),
            (&mut self.subcategory, &other.subcategory),
            (&mut self.organization, &other.organization),
            (&mut self.comune, &other.comune),
            (&mut self.provincia, &other.provincia),
            (&mut self.regione, &other.regione),
            (&mut self.website, &other.website),
            (&mut self.official_code, &other.official_code),
            (&mut self.department, &other.department),
            (&mut self.address, &other.address),
            (&mut self.postal_code, &other.postal_code),
            (&mut self.paese, &other.paese),
        ] {
            if target.as_deref().is_none_or(|text| text.trim().is_empty())
                && let Some(text) = value.as_deref().filter(|text| !text.trim().is_empty())
            {
                *target = Some(text.to_owned());
            }
        }
    }
}

impl LeadAccumulator {
    pub(crate) fn from_source(
        email: String,
        is_pec: bool,
        source_url: &str,
        now: &str,
        role: Option<String>,
        person_name: Option<String>,
        context: LeadContext,
    ) -> Self {
        Self {
            comparison_key: email_comparison_key(&email),
            email,
            source_url: source_url.to_owned(),
            pages: BTreeSet::from([source_url.to_owned()]),
            retrieved_at: now.to_owned(),
            is_pec,
            role,
            person_name,
            context,
            method: "open_data".to_owned(),
        }
    }

    pub(crate) fn email(&self) -> &str {
        &self.email
    }

    pub(crate) fn set_role(&mut self, role: Option<String>) {
        if self.role.is_none() {
            self.role = role;
        }
    }

    pub(crate) fn set_person_name(&mut self, person_name: Option<String>) {
        if self.person_name.is_none() {
            self.person_name = person_name;
        }
    }
}

/// Run-level fields of a `search_runs` row.
pub(crate) struct RunRecord<'a> {
    /// `web`, `source`, `directory` or `crawl`.
    pub(crate) kind: &'a str,
    pub(crate) query_text: &'a str,
    pub(crate) source_id: Option<&'a str>,
    pub(crate) seed_count: i64,
    pub(crate) page_count: i64,
    pub(crate) pdf_found: i64,
    pub(crate) errors: &'a [String],
    pub(crate) now: &'a str,
}

/// Validate the seed list before any network traffic. Returns the trimmed,
/// non-empty seeds in input order.
pub(crate) fn validate_seeds(seed_urls: &[String]) -> Result<Vec<String>, String> {
    let seeds: Vec<String> = seed_urls
        .iter()
        .map(|seed| seed.trim().to_owned())
        .filter(|seed| !seed.is_empty())
        .collect();
    if seeds.is_empty() {
        return Err("Selezionare almeno un URL da analizzare".to_owned());
    }
    if seeds.len() > MAX_SEEDS {
        return Err(format!("Massimo {MAX_SEEDS} URL per ricerca"));
    }
    for seed in &seeds {
        let lower = seed.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err(format!("Sono supportati solo URL http o https: {seed}"));
        }
        if url_host(seed).is_none() {
            return Err(format!("URL non valido: {seed}"));
        }
    }
    Ok(seeds)
}

pub(crate) fn html_to_text(html: &str) -> String {
    let without_scripts = SCRIPT_OR_STYLE_RE.replace_all(html, " ");
    let without_tags = ANY_TAG_RE.replace_all(&without_scripts, " ");
    let decoded = decode_html_entities(&without_tags);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn safe_slice(text: &str, start: usize, end: usize) -> &str {
    let mut from = start.min(text.len());
    while from > 0 && !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = end.min(text.len());
    while to < text.len() && !text.is_char_boundary(to) {
        to += 1;
    }
    if from > to {
        return "";
    }
    &text[from..to]
}

fn strip_fragment(url: &str) -> String {
    url.split('#').next().unwrap_or(url).to_owned()
}

pub(crate) fn url_host(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.split(':').next().unwrap_or(authority).trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

fn url_path(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    match rest.find('/') {
        Some(index) => rest[index..].to_owned(),
        None => String::new(),
    }
}

fn resolve_url(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if ["mailto:", "tel:", "javascript:", "data:"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return None;
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(strip_fragment(href));
    }
    let (scheme, rest) = base.split_once("://")?;
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if href.starts_with("//") {
        return Some(strip_fragment(&format!("{scheme}:{href}")));
    }
    if href.starts_with('/') {
        return Some(strip_fragment(&format!("{scheme}://{authority}{href}")));
    }
    let base_path = &rest[authority_end..];
    let directory = base_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    Some(strip_fragment(&format!(
        "{scheme}://{authority}{directory}/{href}"
    )))
}

fn is_pdf_url(url: &str) -> bool {
    let path = url_path(url);
    let path = path.split(['?', '#']).next().unwrap_or("").to_lowercase();
    path.ends_with(".pdf")
}

fn is_contact_like(url: &str) -> bool {
    let path = url_path(url).to_lowercase();
    if path.is_empty() || path == "/" {
        return true;
    }
    CONTACT_PATH_RE.is_match(&path)
}

/// Priority tier for follow-up links. Tier 0 links (role targets and
/// department trees such as `/struttura/*`, where each office lists its own
/// mailbox) jump to the front of the crawl queue so a bounded budget reaches
/// them before generic contact pages.
fn link_priority(url: &str) -> u8 {
    let path = url_path(url).to_lowercase();
    if path.contains("bullism")
        || path.contains("cyberbull")
        || path.contains("referen")
        || path.contains("struttura")
        || path.contains("organizz")
    {
        0
    } else {
        1
    }
}

/// Order-independent digest of a parsed page's visible text plus its email
/// hits. Some school CMSs answer unknown paths with HTTP 200 serving the
/// homepage again; without this, every such soft-404 would inflate the page
/// count while yielding zero new leads. The digest includes the emails so two
/// pages that differ only in `mailto:`-only addresses are never conflated.
fn content_digest(text: &str, emails: &[String]) -> u64 {
    let mut ordered = emails.to_vec();
    ordered.sort();
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    ordered.hash(&mut hasher);
    hasher.finish()
}

fn context_from_html(html: &str, start: usize, end: usize) -> String {
    let from = start.saturating_sub(CONTEXT_RADIUS * 2);
    let to = (end + CONTEXT_RADIUS * 2).min(html.len());
    html_to_text(safe_slice(html, from, to))
}

/// Parse a fetched HTML page into visible text, de-duplicated email hits with a
/// local context window, linked PDF URLs, and candidate follow-up links.
pub(crate) fn parse_page(base_url: &str, html: &str) -> ParsedPage {
    let text = html_to_text(html);
    let normalized = normalize_obfuscated_email_text_with_method(&text).0;
    let mut emails: Vec<EmailHit> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let matches: Vec<regex::Match> = EMAIL_RE.find_iter(&normalized).collect();
    for (index, matched) in matches.iter().enumerate() {
        let cleaned = clean_email(matched.as_str());
        if !is_valid_email_syntax(&cleaned) {
            continue;
        }
        let key = email_comparison_key(&cleaned);
        if !seen.insert(key) {
            continue;
        }
        // Keep the window between neighbouring addresses so a role or name that
        // belongs to one mailbox is never attributed to the next one.
        let previous_end = if index == 0 {
            0
        } else {
            matches[index - 1].end()
        };
        let next_start = matches
            .get(index + 1)
            .map(|next| next.start())
            .unwrap_or(normalized.len());
        let context = safe_slice(
            &normalized,
            previous_end.max(matched.start().saturating_sub(CONTEXT_RADIUS)),
            next_start.min(matched.end().saturating_add(CONTEXT_RADIUS)),
        )
        .to_owned();
        emails.push(EmailHit {
            email: cleaned,
            context,
        });
    }

    // Addresses that only live inside `mailto:` attributes never reach the
    // visible text, so capture them from the raw markup with an HTML context.
    for captures in MAILTO_RE.captures_iter(html) {
        let Some(full) = captures.get(0) else {
            continue;
        };
        let cleaned = clean_email(&decode_html_entities(&captures[1]));
        if !is_valid_email_syntax(&cleaned) {
            continue;
        }
        let key = email_comparison_key(&cleaned);
        if !seen.insert(key) {
            continue;
        }
        emails.push(EmailHit {
            email: cleaned,
            context: context_from_html(html, full.start(), full.end()),
        });
    }

    let mut links: Vec<String> = Vec::new();
    let mut pdfs: BTreeSet<String> = BTreeSet::new();
    for captures in HREF_RE.captures_iter(html) {
        let raw = decode_entities_fixpoint(&captures[1]);
        let Some(resolved) = resolve_url(base_url, &raw) else {
            continue;
        };
        if is_pdf_url(&resolved) {
            pdfs.insert(resolved.clone());
        }
        links.push(resolved);
    }

    ParsedPage {
        text,
        emails,
        pdf_urls: pdfs.into_iter().collect(),
        links,
    }
}

/// Decode URL-bearing attributes to a fixpoint (bounded): school CMSs emit
/// `&amp;` (sometimes doubly-escaped) inside `href`s, and fetching the raw
/// attribute value sends a literal `&amp;` to the server, which rejects it.
/// NOTE: this is intentionally separate from `parsing::decode_html_entities`,
/// which only decodes email-obfuscation entities and must stay untouched.
fn decode_href_entities(raw: &str) -> String {
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

fn decode_entities_fixpoint(raw: &str) -> String {
    let mut decoded = decode_href_entities(raw);
    for _ in 0..2 {
        let again = decode_href_entities(&decoded);
        if again == decoded {
            break;
        }
        decoded = again;
    }
    decoded
}

pub(crate) fn context_role(context: &str) -> Option<String> {
    let captures = ROLE_LABEL_RE.captures(context)?;
    let label = captures[0].split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() { None } else { Some(label) }
}

pub(crate) fn context_person_name(context: &str) -> Option<String> {
    for captures in PERSON_NAME_RE.captures_iter(context) {
        let first = captures[1].to_lowercase();
        let last = captures[2].to_lowercase();
        if NAME_STOPWORDS.contains(&first.as_str()) || NAME_STOPWORDS.contains(&last.as_str()) {
            continue;
        }
        return Some(format!("{} {}", &captures[1], &captures[2]));
    }
    None
}

#[derive(Debug)]
pub(crate) struct FetchError {
    pub(crate) message: String,
    status: Option<u16>,
    /// Set on throttle answers (429, 503, 509): how long the host asked us to
    /// wait, or the default backoff when it sent no `Retry-After`.
    retry_after: Option<Duration>,
}

impl FetchError {
    fn other(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            status: None,
            retry_after: None,
        }
    }
}

/// `Retry-After` is either delta-seconds or an HTTP-date (RFC 9110 §10.2.3).
fn parse_retry_after(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (at.with_timezone(&chrono::Utc) - now)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

/// Loopback, private, link-local and unspecified addresses: never a public site or mail server.
pub(crate) fn is_internal_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_internal_ip(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback()
                    || v6.is_unspecified()
                    || first & 0xfe00 == 0xfc00
                    || first & 0xffc0 == 0xfe80
            }
        },
    }
}

pub(crate) fn is_internal_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let lower = host.to_ascii_lowercase();
    lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || host.parse().is_ok_and(is_internal_ip)
}

/// Redirects never lead into the local network. With `same_site`, a crawl also
/// stays on the registrable domain it was asked to read: robots.txt was checked
/// for that host only. (Paths on that host are not re-checked after a redirect.)
fn client_with(timeout: Duration, same_site: bool) -> reqwest::Result<reqwest::Client> {
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("troppi reindirizzamenti");
        }
        let target = attempt.url().host_str().unwrap_or_default().to_owned();
        if is_internal_host(&target) {
            return attempt.stop();
        }
        let origin = attempt.previous().first().and_then(|url| url.host_str());
        if same_site && origin.is_none_or(|origin| registrable(origin) != registrable(&target)) {
            return attempt.stop();
        }
        attempt.follow()
    });
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .pool_max_idle_per_host(1)
        .redirect(policy)
        .build()
}

/// Client for fixed public data sources.
pub(crate) fn polite_client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    client_with(timeout, false)
}

/// Client for crawling third-party sites: redirects stay on the same site.
pub(crate) fn crawl_client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    client_with(timeout, true)
}

pub(crate) async fn read_text(response: reqwest::Response, max: usize) -> Result<String, String> {
    let body = read_limited(response, max).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Reads at most `max` bytes. `Content-Length` is only a hint (chunked replies
/// have none), so the body is cut off while streaming.
pub(crate) async fn read_limited(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, String> {
    const TOO_BIG: &str = "contenuto troppo grande";
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(TOO_BIG.to_owned());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if body.len() + chunk.len() > max {
            return Err(TOO_BIG.to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) async fn get_ok(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, FetchError> {
    let response = client.get(url).send().await.map_err(FetchError::other)?;
    let status = response.status().as_u16();
    if response.status().is_success() {
        return Ok(response);
    }
    let retry_after = matches!(status, 429 | 503 | 509).then(|| {
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_retry_after(value, chrono::Utc::now()))
            .unwrap_or(Duration::from_millis(BACKOFF_PAUSE_MILLIS))
    });
    Err(FetchError {
        message: format!("HTTP {status}"),
        status: Some(status),
        retry_after,
    })
}

async fn fetch_html(client: &reqwest::Client, url: &str) -> Result<String, FetchError> {
    let response = get_ok(client, url).await?;
    let body = read_limited(response, MAX_HTML_BYTES)
        .await
        .map_err(|error| FetchError::other(error.replace("contenuto", "pagina")))?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

async fn pause(delay: Duration) {
    tokio::time::sleep(delay).await;
}

/// `(is_index, locations)` of a sitemap or sitemap index document.
pub(crate) fn parse_sitemap(xml: &str) -> (bool, Vec<String>) {
    let is_index = xml.to_ascii_lowercase().contains("<sitemapindex");
    let locations = SITEMAP_LOC_RE
        .captures_iter(xml)
        .map(|captures| decode_entities_fixpoint(&captures[1]))
        .filter(|location| !location.is_empty())
        .collect();
    (is_index, locations)
}

/// Child sitemaps that usually hold pages, circolari and staff entries go
/// first, so the small sitemap budget skips blog-post and media sitemaps.
fn sitemap_child_rank(url: &str) -> u8 {
    let lower = url.to_ascii_lowercase();
    if [
        "page", "pagin", "circolar", "struttur", "person", "organizz",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        0
    } else {
        1
    }
}

/// Read the host's sitemaps (from robots.txt, else `/sitemap.xml`, else
/// `/wp-sitemap.xml`) and return the same-host, robots-allowed, role-priority
/// page URLs they list, at most `cap`. A missing sitemap is normal, not an error.
async fn sitemap_priority_links(
    client: &reqwest::Client,
    seed: &str,
    host: &str,
    robots: &Robots,
    cap: usize,
) -> Vec<String> {
    let mut pending: VecDeque<String> = robots
        .sitemaps
        .iter()
        .filter(|url| url_host(url).as_deref() == Some(host))
        .cloned()
        .collect();
    let mut fallback = None;
    if pending.is_empty() {
        pending.extend(resolve_url(seed, "/sitemap.xml"));
        fallback = resolve_url(seed, "/wp-sitemap.xml");
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut links: Vec<String> = Vec::new();
    let mut files = 0;
    while let Some(url) = pending.pop_front() {
        if files >= MAX_SITEMAP_FILES || links.len() >= cap {
            break;
        }
        if url.to_ascii_lowercase().ends_with(".gz")
            || !seen.insert(url.clone())
            || !robots.allows(&url)
        {
            continue;
        }
        pause(robots.delay()).await;
        files += 1;
        let Ok(xml) = fetch_html(client, &url).await else {
            pending.extend(fallback.take());
            continue;
        };
        fallback = None;
        let (is_index, mut locations) = parse_sitemap(&xml);
        if is_index {
            locations.sort_by_key(|location| sitemap_child_rank(location));
            pending.extend(locations);
            continue;
        }
        for location in locations {
            if links.len() >= cap {
                break;
            }
            if url_host(&location).as_deref() == Some(host)
                && !is_pdf_url(&location)
                && link_priority(&location) == 0
                && robots.allows(&location)
            {
                links.push(strip_fragment(&location));
            }
        }
    }
    links
}

/// Crawl state of one host.
struct HostState {
    host: String,
    cap: usize,
    pdf_cap: usize,
    pdf_count: usize,
    queue: VecDeque<String>,
    visited: HashSet<String>,
    seen_content: HashSet<u64>,
    /// Directory mode: only links under this path (plus pagination) are followed.
    prefix: Option<String>,
    /// Set after the host throttles or blocks us for good: the rest of the
    /// pages are read from the Wayback Machine instead.
    archive_only: bool,
    crawl: HostCrawl,
}

/// Result of crawling one host.
#[derive(Debug, Default)]
pub(crate) struct HostCrawl {
    pub(crate) pages: Vec<FetchedPage>,
    pub(crate) errors: Vec<String>,
    pub(crate) pdf_found: usize,
    /// Directory mode: `(url, anchor text)` of links to other sites.
    pub(crate) outbound: Vec<(String, String)>,
}

fn is_pagination(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    [
        "/page/", "?page=", "&page=", "?pag=", "&pag=", "?p=", "paged=", "/pagina/", "start=",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Sites whose links are navigation, not organizations of a directory.
const OUTBOUND_NOISE: [&str; 24] = [
    "facebook.",
    "instagram.",
    "youtube.",
    "linkedin.",
    "twitter.",
    "x.com",
    "wikipedia.",
    "tripadvisor.",
    "paginegialle.",
    "paginebianche.",
    "google.",
    "gstatic.",
    "googleapis.",
    "apple.com",
    "whatsapp.",
    "t.me",
    "telegram.",
    "spotify.",
    "flickr.",
    "issuu.",
    "wordpress.",
    "w3.org",
    "amazon.",
    "tiktok.",
];

fn registrable(host: &str) -> String {
    let labels: Vec<&str> = host.trim_start_matches("www.").split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}

/// Parse a fetched page, skip it when its content duplicates an already
/// collected page on the same host, otherwise queue its follow-up links
/// (priority links first, in document order) and store it. Returns the newly
/// discovered PDF URLs so the caller can fetch them under its own budget.
fn ingest_page(state: &mut HostState, url: String, html: String, via: &'static str) -> Vec<String> {
    let parsed = parse_page(&url, &html);
    let addresses: Vec<String> = parsed.emails.iter().map(|hit| hit.email.clone()).collect();
    if !state
        .seen_content
        .insert(content_digest(&parsed.text, &addresses))
    {
        return Vec::new();
    }
    let mut priority: Vec<String> = Vec::new();
    for link in parsed.links {
        if state.queue.len() >= state.cap * 3 || state.visited.contains(&link) {
            continue;
        }
        if url_host(&link).as_deref() != Some(state.host.as_str()) {
            continue;
        }
        let follow = match &state.prefix {
            Some(prefix) => url_path(&link).starts_with(prefix.as_str()) || is_pagination(&link),
            None => is_contact_like(&link),
        };
        if !follow {
            continue;
        }
        if link_priority(&link) == 0 {
            priority.push(link);
        } else {
            state.queue.push_back(link);
        }
    }
    for link in priority.into_iter().rev() {
        state.queue.push_front(link);
    }
    if state.prefix.is_some() {
        let own = registrable(&state.host);
        for (link, text) in page_anchors(&url, &html) {
            let Some(host) = url_host(&link) else {
                continue;
            };
            if registrable(&host) == own
                || OUTBOUND_NOISE.iter().any(|noise| host.contains(noise))
                || state
                    .crawl
                    .outbound
                    .iter()
                    .any(|(known, _)| url_host(known) == Some(host.clone()))
            {
                continue;
            }
            state.crawl.outbound.push((link, text));
        }
    }
    let pdfs = parsed.pdf_urls.clone();
    state.crawl.pages.push(FetchedPage { url, html, via });
    pdfs
}

static ANCHOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<a\b[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a\s*>"#)
        .expect("valid anchor regex")
});

/// `(absolute url, visible text)` of every link on the page, first occurrence only.
pub(crate) fn page_anchors(base_url: &str, html: &str) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    ANCHOR_RE
        .captures_iter(html)
        .filter_map(|captures| {
            let url = resolve_url(base_url, &decode_entities_fixpoint(&captures[1]))?;
            if !(url.starts_with("http://") || url.starts_with("https://"))
                || !seen.insert(url.clone())
            {
                return None;
            }
            let text: String = html_to_text(&captures[2]).chars().take(120).collect();
            Some((url, text))
        })
        .collect()
}

/// Challenge and block pages served with HTTP 200 (Cloudflare, Sucuri,
/// DDoS-Guard, Imperva, DataDome, PerimeterX).
pub(crate) fn is_bot_wall(html: &str) -> bool {
    let head: String = html.chars().take(20_000).collect::<String>().to_lowercase();
    [
        "<title>just a moment",
        "cf-browser-verification",
        "challenge-platform",
        "cf_chl_opt",
        "attention required! | cloudflare",
        "ddos-guard",
        "sucuri website firewall",
        "_incapsula_resource",
        "captcha-delivery.com",
        "px-captcha",
        "<title>access denied</title>",
    ]
    .iter()
    .any(|marker| head.contains(marker))
}

/// Politeness delay plus up to 50% jitter, so parallel hosts never tick in step.
fn jittered(delay: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    delay + delay.mul_f64((nanos % 500) as f64 / 1000.0)
}

async fn fetch_pdf_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = get_ok(client, url).await.map_err(|error| error.message)?;
    let bytes = read_limited(response, MAX_PDF_BYTES)
        .await
        .map_err(|_| format!("PDF troppo grande (limite {MAX_PDF_BYTES} byte)"))?;
    pdf_bytes_usable(&bytes)?;
    let text = tauri::async_runtime::spawn_blocking(move || pdf_text(&bytes))
        .await
        .map_err(|error| format!("Estrazione PDF interrotta: {error}"))??;
    let trimmed = text.trim().to_owned();
    if trimmed.is_empty() {
        return Err("PDF senza testo estraibile (solo scansioni?)".to_owned());
    }
    Ok(trimmed)
}

/// Fetch newly discovered PDF links under the per-host PDF budget, extract
/// their text and ingest it as pages. Failures are recorded as non-fatal
/// errors (a scanned-only circolare is expected, not exceptional).
async fn fetch_new_pdfs(
    client: &reqwest::Client,
    state: &mut HostState,
    pdfs: Vec<String>,
    robots: &Robots,
) {
    state.crawl.pdf_found += pdfs.len();
    for pdf_url in pdfs {
        if state.pdf_count >= state.pdf_cap || state.archive_only {
            break;
        }
        if !state.visited.insert(pdf_url.clone()) || !robots.allows(&pdf_url) {
            continue;
        }
        pause(jittered(robots.delay())).await;
        match fetch_pdf_text(client, &pdf_url).await {
            Ok(text) => {
                state.pdf_count += 1;
                ingest_page(state, pdf_url, text, "sito");
            }
            Err(error) => state.crawl.errors.push(format!("{pdf_url}: {error}")),
        }
    }
}

/// Settings shared by every host of a crawl.
pub(crate) struct CrawlSettings<'a> {
    pub(crate) client: &'a reqwest::Client,
    /// TinyFish key and client, to render pages the site blocks.
    pub(crate) tinyfish: Option<(&'a str, &'a reqwest::Client)>,
    pub(crate) max_pages: usize,
    pub(crate) max_pdfs: usize,
    /// Directory mode: follow links under the start path and keep links to
    /// other sites (`HostCrawl::outbound`).
    pub(crate) directory: bool,
    pub(crate) ai: Option<&'a crate::lead_ai::LeadAi>,
    pub(crate) cancel: &'a std::sync::atomic::AtomicBool,
}

/// One page: live first; on a block (403, 406, 429, 451, 503 or a challenge
/// page) TinyFish renders it, then the Wayback Machine and Common Crawl copies
/// are tried.
async fn fetch_page(
    settings: &CrawlSettings<'_>,
    state: &mut HostState,
    robots: &Robots,
    url: &str,
) -> Result<(String, &'static str), String> {
    if state.archive_only {
        return fetch_any_archive(settings.client, url).await;
    }
    pause(jittered(robots.delay())).await;
    let mut result = fetch_html(settings.client, url).await;
    if let Some(wait) = result.as_ref().err().and_then(|error| error.retry_after) {
        if wait > MAX_RETRY_AFTER {
            state.crawl.errors.push(format!(
                "{}: chiede di attendere {}s, passo alla copia in archivio",
                state.host,
                wait.as_secs()
            ));
            state.archive_only = true;
            return fetch_any_archive(settings.client, url).await;
        }
        pause(wait.max(robots.delay())).await;
        result = fetch_html(settings.client, url).await;
    }
    let reason = match &result {
        Ok(html) if is_bot_wall(html) => "pagina anti-bot".to_owned(),
        Ok(_) => {
            return result
                .map(|html| (html, "sito"))
                .map_err(|error| error.message);
        }
        Err(FetchError {
            status: Some(status @ (403 | 406 | 429 | 451 | 503)),
            ..
        }) => format!("HTTP {status}"),
        Err(error) => return Err(error.message.clone()),
    };
    if let Some((key, fallback)) = settings.tinyfish {
        match crate::tinyfish::fetch_html(fallback, key, url).await {
            Ok(html) if !is_bot_wall(&html) => return Ok((html, "tinyfish")),
            Ok(_) => {}
            Err(error) => state.crawl.errors.push(format!("{url}: TinyFish: {error}")),
        }
    }
    fetch_any_archive(settings.client, url)
        .await
        .map_err(|error| format!("{reason}, {error}"))
}

/// Wayback Machine first, then Common Crawl: `(html, via)`.
async fn fetch_any_archive(
    client: &reqwest::Client,
    url: &str,
) -> Result<(String, &'static str), String> {
    let wayback = match fetch_archived(client, url).await {
        Ok(html) => return Ok((html, "archivio")),
        Err(error) => error,
    };
    fetch_common_crawl(client, url)
        .await
        .map(|html| (html, "commoncrawl"))
        .map_err(|common_crawl| format!("archivio: {wayback}; Common Crawl: {common_crawl}"))
}

/// Crawl one host from its seed URLs, sequentially and politely: robots.txt
/// rules and Crawl-delay, sitemap role pages first, same-host contact-like
/// links (or, in directory mode, links under the start path). With the model
/// enabled it picks the most promising links of the first page.
pub(crate) async fn crawl_host(settings: &CrawlSettings<'_>, seeds: &[String]) -> HostCrawl {
    let Some(host) = seeds.first().and_then(|seed| url_host(seed)) else {
        return HostCrawl {
            errors: vec!["URL non valido".to_owned()],
            ..HostCrawl::default()
        };
    };
    let seed = seeds[0].clone();
    let prefix = settings.directory.then(|| {
        let path = url_path(&seed);
        let path = path.split(['?', '#']).next().unwrap_or("");
        match path.rsplit_once('/') {
            Some((directory, _)) if !directory.is_empty() => format!("{directory}/"),
            _ => "/".to_owned(),
        }
    });
    let mut state = HostState {
        host: host.clone(),
        cap: settings.max_pages.max(1),
        pdf_cap: settings.max_pdfs,
        pdf_count: 0,
        queue: seeds.iter().cloned().collect(),
        visited: HashSet::new(),
        seen_content: HashSet::new(),
        prefix,
        archive_only: false,
        crawl: HostCrawl::default(),
    };
    let robots = match load_robots(settings.client, &seed).await {
        Ok(robots) => robots,
        Err(error) => {
            state.crawl.errors.push(format!(
                "{host}: robots.txt non raggiungibile ({error}), host saltato"
            ));
            return state.crawl;
        }
    };
    if robots
        .crawl_delay
        .is_some_and(|delay| delay > MAX_CRAWL_DELAY)
    {
        state.crawl.errors.push(format!(
            "{host}: robots.txt chiede più di {}s tra le richieste, host saltato",
            MAX_CRAWL_DELAY.as_secs()
        ));
        return state.crawl;
    }
    if !settings.directory && robots.allows(&seed) {
        let links = sitemap_priority_links(settings.client, &seed, &host, &robots, state.cap).await;
        state.queue.extend(links);
    }
    let mut streak = 0usize;
    let mut asked_model = settings.ai.is_none() || settings.directory;
    let mut fetched = 0usize;
    while let Some(url) = state.queue.pop_front() {
        if fetched >= state.cap || settings.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if !state.visited.insert(url.clone()) {
            continue;
        }
        if !robots.allows(&url) {
            if seeds.contains(&url) {
                state
                    .crawl
                    .errors
                    .push(format!("{url}: escluso da robots.txt"));
            }
            continue;
        }
        match fetch_page(settings, &mut state, &robots, &url).await {
            Ok((html, via)) => {
                streak = 0;
                fetched += 1;
                if via != "sito" {
                    state
                        .crawl
                        .errors
                        .push(format!("{url}: bloccato dal sito, letto via {via}"));
                }
                if !asked_model {
                    asked_model = true;
                    if let Some(ai) = settings.ai {
                        let anchors: Vec<(String, String)> = page_anchors(&url, &html)
                            .into_iter()
                            .filter(|(link, _)| {
                                url_host(link).as_deref() == Some(host.as_str())
                                    && !is_pdf_url(link)
                                    && robots.allows(link)
                            })
                            .take(150)
                            .collect();
                        match ai.choose_links(&host, &anchors).await {
                            Ok(chosen) => {
                                for link in chosen.into_iter().rev() {
                                    state.queue.push_front(link);
                                }
                            }
                            Err(error) => {
                                state.crawl.errors.push(format!("{host}: IA link: {error}"))
                            }
                        }
                    }
                }
                let pdfs = ingest_page(&mut state, url, html, via);
                fetch_new_pdfs(settings.client, &mut state, pdfs, &robots).await;
            }
            Err(error) => {
                state.crawl.errors.push(format!("{url}: {error}"));
                streak += 1;
                if streak >= MAX_HOST_ERRORS {
                    if state.archive_only {
                        state
                            .crawl
                            .errors
                            .push(format!("{host}: troppe risposte negative, mi fermo qui"));
                        break;
                    }
                    state.crawl.errors.push(format!(
                        "{host}: troppe risposte negative, passo alla copia in archivio"
                    ));
                    state.archive_only = true;
                    streak = 0;
                }
            }
        }
    }
    state.crawl
}

/// Crawl the seeds host by host (hosts in parallel). Used by the live smoke test.
#[cfg(test)]
pub(crate) async fn crawl_seeds(
    seeds: &[String],
    max_pages_per_domain: i64,
    max_pdfs_per_domain: i64,
    tinyfish_key: Option<&str>,
) -> (Vec<FetchedPage>, Vec<String>) {
    use futures::StreamExt;
    let client = match crawl_client(Duration::from_secs(FETCH_TIMEOUT_SECONDS)) {
        Ok(client) => client,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("client HTTP non disponibile: {error}")],
            );
        }
    };
    let fallback = crawl_client(crate::tinyfish::FETCH_TIMEOUT).ok();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let settings = CrawlSettings {
        client: &client,
        tinyfish: tinyfish_key.zip(fallback.as_ref()),
        max_pages: max_pages_per_domain.max(1) as usize,
        max_pdfs: max_pdfs_per_domain.max(0) as usize,
        directory: false,
        ai: None,
        cancel: &cancel,
    };
    let mut by_host: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut errors = Vec::new();
    for seed in seeds {
        match url_host(seed) {
            Some(host) => by_host.entry(host).or_default().push(seed.clone()),
            None => errors.push(format!("{seed}: URL non valido")),
        }
    }
    let results: Vec<HostCrawl> = futures::stream::iter(by_host.into_values())
        .map(|urls| {
            let settings = &settings;
            async move { crawl_host(settings, &urls).await }
        })
        .buffer_unordered(HOST_CONCURRENCY)
        .collect()
        .await;
    let mut pages = Vec::new();
    for result in results {
        pages.extend(result.pages);
        errors.extend(result.errors);
    }
    (pages, errors)
}

/// Resolve the optional per-run limits to the clamped values the crawl uses.
pub(crate) fn clamp_run_limits(
    max_pages_per_domain: Option<i64>,
    max_pdfs: Option<i64>,
) -> (i64, i64) {
    let max_pages = max_pages_per_domain
        .unwrap_or(DEFAULT_MAX_PAGES_PER_DOMAIN)
        .clamp(1, MAX_PAGES_PER_DOMAIN);
    let max_pdfs = max_pdfs.unwrap_or(DEFAULT_MAX_PDFS).clamp(0, MAX_PDF_LINKS);
    (max_pages, max_pdfs)
}

/// Web search run: the TinyFish seed URLs are crawled like any other seeds.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn start_search_run(
    app: tauri::AppHandle,
    query: String,
    seed_urls: Vec<String>,
    max_pages_per_domain: Option<i64>,
    max_pdfs: Option<i64>,
    use_ai: Option<bool>,
) -> Result<SearchRunSummary, String> {
    let seeds = validate_seeds(&seed_urls)?;
    let (max_pages, max_pdfs) = clamp_run_limits(max_pages_per_domain, max_pdfs);
    crate::lead_crawl::crawl_urls(
        app,
        crate::lead_crawl::CrawlJob {
            label: query.clone(),
            kind: "web",
            source_id: None,
            max_pages,
            max_pdfs,
            use_ai: use_ai.unwrap_or(false),
            directory: None,
        },
        seeds
            .into_iter()
            .map(|url| crate::lead_crawl::SeedRow {
                url: Some(url),
                name: None,
                context: LeadContext::default(),
            })
            .collect(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn list_search_leads(
    app: tauri::AppHandle,
    filters: LeadFilters,
    limit: usize,
    offset: usize,
) -> Result<SearchLeadList, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        list_sandbox_leads(&db, &filters, limit, offset)
    })
    .await
    .map_err(|error| format!("Caricamento lead interrotto: {error}"))?
}

#[tauri::command]
pub(crate) async fn get_lead_filter_options(
    app: tauri::AppHandle,
    kinds: Vec<String>,
) -> Result<LeadFilterOptions, String> {
    tauri::async_runtime::spawn_blocking(move || lead_filter_options(&*open_db(&app)?, &kinds))
        .await
        .map_err(|error| format!("Caricamento filtri interrotto: {error}"))?
}

#[tauri::command]
pub(crate) async fn list_search_runs(app: tauri::AppHandle) -> Result<Vec<SearchRunView>, String> {
    tauri::async_runtime::spawn_blocking(move || list_runs(&*open_db(&app)?))
        .await
        .map_err(|error| format!("Caricamento ricerche interrotto: {error}"))?
}

#[tauri::command]
pub(crate) async fn delete_search_run(app: tauri::AppHandle, run_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || delete_run(&*open_db(&app)?, &run_id))
        .await
        .map_err(|error| format!("Eliminazione interrotta: {error}"))?
}

#[tauri::command]
pub(crate) async fn export_search_leads_txt(
    app: tauri::AppHandle,
    filters: LeadFilters,
    path: String,
) -> Result<ExportReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        let target = std::path::PathBuf::from(path);
        let rows_written = export_leads_txt_to_path(&db, &filters, &target)?;
        Ok(ExportReport {
            path: target.display().to_string(),
            rows_written,
        })
    })
    .await
    .map_err(|error| format!("Esportazione lead interrotta: {error}"))?
}

#[tauri::command]
pub(crate) async fn promote_search_leads(
    app: tauri::AppHandle,
    filters: LeadFilters,
) -> Result<PromoteSearchReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        promote_leads_inner(&db, &filters)
    })
    .await
    .map_err(|error| format!("Importazione lead interrotta: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(reply.as_bytes());
            // A chunked body that never ends: the reader must give up on its own.
            if reply.contains("chunked") {
                while stream.write_all(b"400\r\n").is_ok()
                    && stream.write_all(&[b'x'; 1024]).is_ok()
                    && stream.write_all(b"\r\n").is_ok()
                {}
            }
        });
        (address, handle)
    }

    #[test]
    fn internal_addresses_are_recognised() {
        for host in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "169.254.1.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "localhost",
            "printer.local",
        ] {
            assert!(is_internal_host(host), "{host}");
        }
        for host in ["93.184.216.34", "2606:2800:220:1::1", "example.it"] {
            assert!(!is_internal_host(host), "{host}");
        }
    }

    #[tokio::test]
    async fn endless_chunked_body_is_cut_at_the_limit() {
        let (url, server) = serve_once("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        let client = polite_client(Duration::from_secs(10)).unwrap();
        let response = get_ok(&client, &url).await.unwrap();
        assert!(read_limited(response, 10_000).await.is_err());
        // Not joined: the connection is closed by a task on this runtime, and the
        // server thread stops once that happens.
        drop(server);
    }

    #[tokio::test]
    async fn redirect_into_the_local_network_is_not_followed() {
        let (url, server) = serve_once(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/secret\r\nContent-Length: 0\r\n\r\n",
        );
        let client = polite_client(Duration::from_secs(10)).unwrap();
        let error = get_ok(&client, &url).await.unwrap_err();
        assert_eq!(error.status, Some(302));
        let _ = server.join();
    }

    /// Locks the discovery result contract: the frontend consumes serde's
    /// default snake_case field names (same as `ContactList`, `LeadList` and
    /// `ExportReport`). If this test changes, `src/types.ts` must change too.
    #[test]
    fn discovery_result_structs_serialize_snake_case() {
        let summary = SearchRunSummary {
            run_id: "run-1".to_owned(),
            lead_count: 2,
            page_count: 3,
            pdf_found: 4,
            errors: Vec::new(),
        };
        let summary_json = serde_json::to_value(&summary).unwrap();
        for key in ["run_id", "lead_count", "page_count", "pdf_found", "errors"] {
            assert!(summary_json.get(key).is_some(), "missing {key}");
        }
        for key in ["runId", "leadCount", "pageCount", "pdfFound"] {
            assert!(summary_json.get(key).is_none(), "unexpected {key}");
        }

        let lead = SearchLeadView {
            email: "utente@example.test".to_owned(),
            email_local_part: "utente".to_owned(),
            email_domain: "example".to_owned(),
            email_tld: ".test".to_owned(),
            email_comparison_key: "utente@example.test".to_owned(),
            website: None,
            macroarea: None,
            source_url: Some("https://example.test/".to_owned()),
            retrieved_at: "2026-09-22T10:00:00Z".to_owned(),
            is_pec: false,
            person_name: None,
            role: None,
            category: Some("Istruzione".to_owned()),
            subcategory: None,
            organization: None,
            comune: Some("Bergamo".to_owned()),
            provincia: None,
            regione: None,
            official_code: None,
            department: Some("Ufficio tributi".to_owned()),
            id: "lead-1".to_owned(),
            run_id: "run-1".to_owned(),
            run_label: Some("Fonte: test".to_owned()),
            address: Some("Via Roma 1".to_owned()),
            postal_code: Some("24100".to_owned()),
            paese: None,
            method: Some("sito+ia".to_owned()),
            source_count: 2,
            in_database: false,
            stato: "nuovo".to_owned(),
        };
        let lead_json = serde_json::to_value(&lead).unwrap();
        for key in [
            "source_url",
            "retrieved_at",
            "is_pec",
            "person_name",
            "subcategory",
            "regione",
            "official_code",
            "department",
            "run_label",
            "postal_code",
            "source_count",
            "in_database",
        ] {
            assert!(lead_json.get(key).is_some(), "missing {key}");
        }
        for key in ["sourceUrl", "retrievedAt", "isPec", "personName"] {
            assert!(lead_json.get(key).is_none(), "unexpected {key}");
        }

        let list = SearchLeadList {
            leads: vec![lead],
            total: 1,
            limit: 100,
            offset: 0,
            has_previous: false,
            has_next: false,
        };
        let list_json = serde_json::to_value(&list).unwrap();
        for key in ["has_previous", "has_next"] {
            assert!(list_json.get(key).is_some(), "missing {key}");
        }
        for key in ["hasPrevious", "hasNext"] {
            assert!(list_json.get(key).is_none(), "unexpected {key}");
        }
    }

    /// `crawl_seeds` owns the same-domain and contact-like gating, but its async
    /// network loop is still covered only by the ignored live smoke test. These
    /// lock the two pure predicates the gating relies on. The loop itself (fetch,
    /// queue wiring and the per-host budget enforcement) remains uncovered
    /// offline until the fetch plumbing is refactored behind a stub.
    #[test]
    fn discovery_url_host_normalizes_authority() {
        assert_eq!(
            url_host("https://Example.TEST/contatti"),
            Some("example.test".to_owned())
        );
        assert_eq!(
            url_host("http://utente@example.test:8080/x"),
            Some("example.test".to_owned())
        );
        assert_eq!(url_host("example.test"), None);
    }

    #[test]
    fn discovery_contact_like_filter_keeps_only_contact_paths() {
        for url in [
            "https://example.test/",
            "https://example.test",
            "https://example.test/contatti",
            "https://example.test/chi-siamo",
            "https://example.test/amministrazione-trasparente",
            "https://example.test/persone",
        ] {
            assert!(is_contact_like(url), "atteso contatto: {url}");
        }
        for url in [
            "https://example.test/blog",
            "https://example.test/news/2026",
            "https://example.test/documenti/piano.pdf",
            "https://example.test/wp-content/uploads/foto.png",
        ] {
            assert!(!is_contact_like(url), "atteso non contatto: {url}");
        }
    }

    #[test]
    fn discovery_content_digest_spots_soft_404_homepage_copies() {
        let html = r#"<html><body><p>Segreteria <a href="mailto:info@example.test">info@example.test</a></p></body></html>"#;
        let first = parse_page("https://example.test/", html);
        let second = parse_page(
            "https://example.test/_sito2020/index.php/scuola/contatti",
            html,
        );
        let digest_of = |parsed: &ParsedPage| {
            content_digest(
                &parsed.text,
                &parsed
                    .emails
                    .iter()
                    .map(|hit| hit.email.clone())
                    .collect::<Vec<_>>(),
            )
        };
        // Same content under another URL: the crawl must treat it as duplicate.
        assert_eq!(digest_of(&first), digest_of(&second));
        // Genuinely different content (or different mailto-only addresses) must not conflate.
        let other = parse_page(
            "https://example.test/contatti",
            r#"<html><body><p>Segreteria <a href="mailto:urp@example.test">urp@example.test</a></p></body></html>"#,
        );
        assert_ne!(digest_of(&first), digest_of(&other));
        // Email order must not matter.
        let reordered = vec!["b@example.test".to_owned(), "a@example.test".to_owned()];
        let ordered = vec!["a@example.test".to_owned(), "b@example.test".to_owned()];
        assert_eq!(
            content_digest("stesso testo", &reordered),
            content_digest("stesso testo", &ordered)
        );
    }

    #[test]
    fn discovery_link_priority_prefers_role_and_department_trees() {
        for url in [
            "https://example.test/bullismo-e-cyberbullismo/",
            "https://example.test/struttura/commissione-cyberbullismo/",
            "https://example.test/struttura/dsga/",
            "https://example.test/organizzazione/dirigenza/",
            "https://example.test/referente-bes/",
        ] {
            assert_eq!(link_priority(url), 0, "atteso prioritario: {url}");
        }
        for url in [
            "https://example.test/contatti",
            "https://example.test/amministrazione-trasparente",
            "https://example.test/",
        ] {
            assert_eq!(link_priority(url), 1, "atteso normale: {url}");
        }
    }

    #[test]
    fn discovery_bot_walls_and_wayback_snapshots_are_recognized() {
        assert!(is_bot_wall(
            "<html><head><title>Just a moment...</title></head></html>"
        ));
        assert!(is_bot_wall(
            "<script src=\"/cdn-cgi/challenge-platform/h/b\"></script>"
        ));
        assert!(!is_bot_wall("<p>Contatti: utente@example.test</p>"));
        let cdx = r#"[["timestamp"],["20260213165752"],["20260520135427"]]"#;
        assert_eq!(wayback_snapshot(cdx).as_deref(), Some("20260520135427"));
        assert_eq!(wayback_snapshot(r#"[["timestamp"]]"#), None);
        assert_eq!(wayback_snapshot("<html>429 Too Many Requests</html>"), None);
        let delay = Duration::from_millis(1000);
        let jitter = jittered(delay);
        assert!(jitter >= delay && jitter < Duration::from_millis(1500));
    }

    #[test]
    fn discovery_common_crawl_index_and_warc_record_are_read() {
        let collinfo = r#"[{"id":"CC-MAIN-2026-39","cdx-api":"https://index.commoncrawl.org/CC-MAIN-2026-39-index"},
            {"id":"CC-MAIN-2026-34","cdx-api":"https://index.commoncrawl.org/CC-MAIN-2026-34-index"},
            {"id":"CC-MAIN-2026-30","cdx-api":"https://index.commoncrawl.org/CC-MAIN-2026-30-index"}]"#;
        assert_eq!(
            common_crawl_apis(collinfo),
            [
                "https://index.commoncrawl.org/CC-MAIN-2026-39-index",
                "https://index.commoncrawl.org/CC-MAIN-2026-34-index"
            ]
        );
        let cdx = concat!(
            r#"{"timestamp":"20260905","status":"200","filename":"crawl-data/a.warc.gz","offset":"10","length":"20"}"#,
            "\n",
            r#"{"timestamp":"20260910","status":"301","filename":"crawl-data/b.warc.gz","offset":"1","length":"2"}"#,
            "\n",
            r#"{"timestamp":"20260912","status":"200","filename":"crawl-data/c.warc.gz","offset":"30","length":"40"}"#,
            "\n",
            r#"{"timestamp":"20260913","status":"200","filename":"../x","offset":"1","length":"2"}"#,
        );
        assert_eq!(
            common_crawl_record(cdx),
            Some(("crawl-data/c.warc.gz".to_owned(), 30, 40))
        );
        assert_eq!(common_crawl_record("<html>504</html>"), None);

        let gzip = |text: &str| {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            std::io::Write::write_all(&mut encoder, text.as_bytes()).unwrap();
            encoder.finish().unwrap()
        };
        let record = gzip(
            "WARC/1.0\r\nWARC-Type: response\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<p>utente@example.test</p>",
        );
        assert_eq!(warc_html(&record).unwrap(), "<p>utente@example.test</p>");
        let redirect = gzip("WARC/1.0\r\n\r\nHTTP/1.1 301 Moved\r\nLocation: /\r\n\r\n");
        assert!(warc_html(&redirect).is_err());
        assert!(warc_html(b"not gzip").is_err());
    }

    #[test]
    fn discovery_directory_mode_follows_the_list_and_collects_outbound_sites() {
        let html = r#"<a href="/sezioni/lombardia/">Lombardia</a>
            <a href="/sezioni/page/2/">2</a>
            <a href="/news/">News</a>
            <a href="https://www.caibergamo.test/">Sezione di Bergamo</a>
            <a href="https://www.facebook.com/cai">Facebook</a>
            <a href="https://shop.cai.test/">Shop</a>"#;
        let mut state = HostState {
            host: "www.cai.test".to_owned(),
            cap: 10,
            pdf_cap: 0,
            pdf_count: 0,
            queue: VecDeque::new(),
            visited: HashSet::new(),
            seen_content: HashSet::new(),
            prefix: Some("/sezioni/".to_owned()),
            archive_only: false,
            crawl: HostCrawl::default(),
        };
        ingest_page(
            &mut state,
            "https://www.cai.test/sezioni/".to_owned(),
            html.to_owned(),
            "sito",
        );
        assert_eq!(
            state.queue.iter().cloned().collect::<Vec<_>>(),
            [
                "https://www.cai.test/sezioni/lombardia/",
                "https://www.cai.test/sezioni/page/2/"
            ]
        );
        assert_eq!(
            state.crawl.outbound,
            [(
                "https://www.caibergamo.test/".to_owned(),
                "Sezione di Bergamo".to_owned()
            )]
        );
    }

    #[test]
    fn discovery_entity_decoding_reaches_a_fixpoint() {
        assert_eq!(
            decode_entities_fixpoint("servizi?id=1&amp;amp;source=x"),
            "servizi?id=1&source=x"
        );
        assert_eq!(decode_entities_fixpoint("a?b=1&amp;c=2"), "a?b=1&c=2");
        assert_eq!(decode_entities_fixpoint("plain"), "plain");
    }

    /// Minimal one-page PDF built in-test (correct xref offsets) so PDF text
    /// extraction is covered offline with a synthetic address only.
    fn tiny_pdf_with_text(line: &str) -> Vec<u8> {
        let escaped = line
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        tiny_pdf_with_content(&format!("BT /F1 24 Tf 100 700 Td ({escaped}) Tj ET"))
    }

    fn tiny_pdf_with_content(content: &str) -> Vec<u8> {
        let bodies = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 5 0 R >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in bodies.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
        }
        let xref_at = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", bodies.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF",
                bodies.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn pdf_text_joins_addresses_split_across_styled_and_kerned_pieces() {
        let pdf = tiny_pdf_with_content(
            "BT /F1 12 Tf 72 700 Td (Scrivere a) Tj 70 0 Td [(mario.ro) -20 (ssi)] TJ \
             /F2 12 Tf (@example.test) Tj 0 -14 Td (Referente) Tj ET",
        );
        assert_eq!(
            pdf_text(&pdf).unwrap(),
            "Scrivere a mario.rossi@example.test\nReferente\n"
        );
    }

    #[test]
    fn discovery_pdf_bytes_gate_rejects_junk_before_extraction() {
        assert!(pdf_bytes_usable(b"%PDF-1.4\nrest").is_ok());
        assert!(pdf_bytes_usable(b"<html>not a pdf").is_err());
        assert!(pdf_bytes_usable(b"").is_err());
        let oversized = vec![b'A'; MAX_PDF_BYTES + 1];
        assert!(pdf_bytes_usable(&oversized).is_err());
    }

    #[test]
    fn discovery_pdf_text_extraction_feeds_email_discovery() {
        let pdf = tiny_pdf_with_text("Referente bullismo Rossi referente@example.test");
        pdf_bytes_usable(&pdf).expect("synthetic pdf passes the gate");
        let text = pdf_text(&pdf).expect("synthetic pdf extracts");
        assert!(
            text.contains("referente@example.test"),
            "testo inatteso: {text}"
        );
        let parsed = parse_page("https://example.test/circolare.pdf", &text);
        let found: Vec<_> = parsed.emails.iter().map(|hit| hit.email.as_str()).collect();
        assert_eq!(found, vec!["referente@example.test"]);
        let role = context_role(&parsed.emails[0].context).expect("ruolo rilevato");
        assert!(
            role.to_lowercase().contains("referente bullismo"),
            "ruolo inatteso: {role}"
        );
    }

    #[test]
    fn discovery_robots_picks_our_group_and_applies_longest_match() {
        let text = "\
User-agent: *
Disallow: /
Crawl-delay: 2

User-agent: Googlebot
User-agent: DatabaseContatti
Disallow: /area-riservata/
Disallow: /*?s=
Disallow: /*.php$
Allow: /area-riservata/contatti
Crawl-delay: 4.5 # secondi
Sitemap: https://example.test/sitemap_index.xml
";
        let robots = parse_robots(text);
        assert_eq!(robots.crawl_delay, Some(Duration::from_millis(4500)));
        assert_eq!(robots.delay(), Duration::from_millis(4500));
        assert_eq!(
            robots.sitemaps,
            vec!["https://example.test/sitemap_index.xml"]
        );
        assert!(robots.allows("https://example.test/"));
        assert!(robots.allows("https://example.test/contatti"));
        assert!(!robots.allows("https://example.test/area-riservata/docenti"));
        assert!(robots.allows("https://example.test/area-riservata/contatti"));
        assert!(!robots.allows("https://example.test/cerca?s=bullismo"));
        assert!(!robots.allows("https://example.test/index.php"));
        assert!(robots.allows("https://example.test/index.php?id=1"));

        let star_only = parse_robots("User-agent: *\nDisallow: /privato\nDisallow:\n");
        assert!(!star_only.allows("https://example.test/privato/x"));
        assert!(star_only.allows("https://example.test/pubblico"));
        assert_eq!(star_only.delay(), Duration::from_millis(FETCH_PAUSE_MILLIS));
        assert!(parse_robots("<html>soft 404</html>").allows("https://example.test/x"));
    }

    #[test]
    fn discovery_retry_after_reads_seconds_and_http_dates() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-23T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            parse_retry_after(" 30 ", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after("Wed, 23 Sep 2026 10:01:30 GMT", now),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            parse_retry_after("Wed, 23 Sep 2026 09:00:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after("presto", now), None);
    }

    #[test]
    fn discovery_sitemap_parses_index_and_urlset() {
        let index = r#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
<sitemap><loc>https://example.test/post-sitemap.xml</loc></sitemap>
<sitemap><loc><![CDATA[https://example.test/page-sitemap.xml]]></loc></sitemap>
</sitemapindex>"#;
        let (is_index, mut children) = parse_sitemap(index);
        assert!(is_index);
        children.sort_by_key(|child| sitemap_child_rank(child));
        assert_eq!(
            children,
            vec![
                "https://example.test/page-sitemap.xml",
                "https://example.test/post-sitemap.xml"
            ]
        );

        let urlset = "<urlset><url><loc>https://example.test/bullismo-e-cyberbullismo/</loc></url>\
<url><loc>https://example.test/?page_id=7&amp;lang=it</loc></url></urlset>";
        let (is_index, pages) = parse_sitemap(urlset);
        assert!(!is_index);
        assert_eq!(
            pages,
            vec![
                "https://example.test/bullismo-e-cyberbullismo/",
                "https://example.test/?page_id=7&lang=it"
            ]
        );
    }

    /// Three verified Lombardia school sites for the first live smoke test.
    const LIVE_SEEDS: [&str; 3] = [
        "https://www.liceobeccaria.edu.it/",
        "https://www.liceogiovio.edu.it/",
        "https://www.liceolussana.edu.it/",
    ];
    /// Polite live-crawl bound: mirrors the UI default of twenty pages per domain.
    const LIVE_MAX_PAGES_PER_DOMAIN: i64 = 20;

    fn looks_like_connectivity_failure(errors: &[String]) -> bool {
        !errors.is_empty()
            && errors.iter().all(|error| {
                let lower = error.to_ascii_lowercase();
                [
                    "dns",
                    "connect",
                    "timed out",
                    "timeout",
                    "network",
                    "tcp",
                    "tls",
                    "error sending request",
                    "client http non disponibile",
                ]
                .iter()
                .any(|needle| lower.contains(needle))
            })
    }

    /// Live directory crawl and Wayback read; prints counts only.
    #[tokio::test]
    #[ignore = "live directory crawl; run: cargo test --manifest-path src-tauri/Cargo.toml discovery_live_directory_and_archive -- --ignored --nocapture"]
    async fn discovery_live_directory_and_archive() {
        let client = polite_client(Duration::from_secs(FETCH_TIMEOUT_SECONDS)).unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let settings = CrawlSettings {
            client: &client,
            tinyfish: None,
            max_pages: 4,
            max_pdfs: 0,
            directory: true,
            ai: None,
            cancel: &cancel,
        };
        let crawl = crawl_host(
            &settings,
            &["https://www.cai.it/sezioni-territoriali/sezioni-e-sottosezioni/".to_owned()],
        )
        .await;
        println!(
            "LIVE directory: pages={} outbound={} errors={:?}",
            crawl.pages.len(),
            crawl.outbound.len(),
            crawl.errors
        );
        assert!(!crawl.pages.is_empty());
        let archived = fetch_archived(&client, "https://www.caritas.it/caritas-diocesane/").await;
        println!("LIVE archive: {:?}", archived.as_ref().map(String::len));
        assert!(archived.is_ok_and(|html| html.len() > 1000));
    }

    /// Live smoke test: crawls the three seed sites and persists the
    /// leads in memory. Synthetic assertions only (counts); no
    /// real address is printed or asserted on.
    #[tokio::test]
    #[ignore = "live web discovery; needs network access. Run: cargo test --manifest-path src-tauri/Cargo.toml search_discovery_live_lombardia_smoke -- --ignored --nocapture"]
    async fn search_discovery_live_lombardia_smoke() {
        let seeds: Vec<String> = LIVE_SEEDS.iter().map(|seed| (*seed).to_owned()).collect();
        let seeds = validate_seeds(&seeds).expect("seed URLs are valid");

        let crawl_started = std::time::Instant::now();
        let (pages, errors) = crawl_seeds(
            &seeds,
            LIVE_MAX_PAGES_PER_DOMAIN,
            DEFAULT_MAX_PDFS,
            std::env::var("TINYFISH_API_KEY").ok().as_deref(),
        )
        .await;
        let crawl_elapsed = crawl_started.elapsed();

        println!(
            "LIVE discovery crawl: pages={} fetch_errors={} elapsed={:.1}s",
            pages.len(),
            errors.len(),
            crawl_elapsed.as_secs_f64()
        );
        for error in &errors {
            println!("LIVE discovery fetch error: {error}");
        }

        if pages.is_empty() && looks_like_connectivity_failure(&errors) {
            eprintln!("LIVE discovery SKIPPED: network unavailable in this environment");
            return;
        }
        assert!(!pages.is_empty(), "no page fetched: {errors:?}");

        let db = Connection::open_in_memory().unwrap();
        crate::schema::setup_schema(&db).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let persist_started = std::time::Instant::now();
        let summary = persist_search_run(
            &db,
            "referente bullismo Lombardia",
            &seeds,
            &pages,
            &now,
            DEFAULT_MAX_PDFS,
            &errors,
        )
        .expect("live run persists");
        let persist_elapsed = persist_started.elapsed();

        let list = list_search_leads_page(&db, &summary.run_id, MAX_PAGE_SIZE, 0).unwrap();
        let pec_count = list.leads.iter().filter(|lead| lead.is_pec).count();
        let blocked = errors
            .iter()
            .filter(|error| error.contains("403") || error.contains("429"))
            .count();

        println!(
            "LIVE discovery result: pages={} leads={} pdf_found={} pec={} blocks={} persist={:.2}s total={:.1}s",
            summary.page_count,
            summary.lead_count,
            summary.pdf_found,
            pec_count,
            blocked,
            persist_elapsed.as_secs_f64(),
            (crawl_elapsed + persist_elapsed).as_secs_f64()
        );

        assert!(summary.page_count > 0);
        assert!(summary.lead_count > 0, "no lead discovered");
        assert!(summary.pdf_found <= DEFAULT_MAX_PDFS);
    }
}
