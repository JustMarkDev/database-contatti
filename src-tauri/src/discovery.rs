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

#[cfg(test)]
mod crawl_check;
#[cfg(test)]
mod spike;
mod store;

pub(crate) use store::*;

pub(crate) const MAX_SEEDS: usize = 10;
/// Pages read per site, the home included: each one is a context.dev credit.
pub(crate) const MAX_PAGES_PER_DOMAIN: i64 = 6;
pub(crate) const DEFAULT_MAX_PAGES_PER_DOMAIN: i64 = 3;
/// PDFs per site, read only for Istruzione seeds (circolari with roles).
pub(crate) const MAX_PDF_LINKS: i64 = 2;
pub(crate) const DEFAULT_MAX_PDFS: i64 = 0;
const MAX_HOST_ERRORS: usize = 3;
/// Pages per site once the extra pages keep yielding: after the pages the user
/// asked for, a site is read on only while its last page gave a new address.
pub(crate) const EXTEND_UP_TO_PAGES: usize = 10;
/// Candidates the model may choose from, best local score first.
const MAX_AI_CANDIDATES: usize = 40;
/// Links to other sites kept per site read (directories keep them all).
const MAX_OUTBOUND_PER_SITE: usize = 40;
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
static CFEMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:data-cfemail\s*=\s*["']|/cdn-cgi/l/email-protection#)([0-9a-f]{4,})"#)
        .expect("valid cfemail regex")
});
static ROLE_LABEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(referente|responsabile|coordinatore|coordinatrice|dirigente|preside|docente|tutor)[^\n\r:;.,\-–—]{0,48}",
    )
    .expect("valid role label regex")
});
static PERSON_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-ZÀ-ÖØ-Ý][a-zà-öø-ÿ']{1,24})\s+([A-ZÀ-ÖØ-Ý][a-zà-öø-ÿ']{1,24})\b")
        .expect("valid person name regex")
});

const NAME_STOPWORDS: [&str; 41] = [
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
    // Page furniture that looks like a name: "Cookie Policy", "Facebook Vai", "Credits Area".
    "cookie",
    "policy",
    "privacy",
    "facebook",
    "instagram",
    "vai",
    "accetta",
    "tel",
    "via",
    "area",
    "credits",
    "software",
    "informatica",
    "feedback",
    "accessibilità",
    "eventi",
    "avvisi",
    "piazza",
    "la",
    "il",
    "del",
];

#[derive(Debug, Clone)]
pub(crate) struct FetchedPage {
    pub(crate) url: String,
    pub(crate) html: String,
    /// `contextdev` for pages read through context.dev.
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
    /// How the address was found: `open_data`, `contextdev` (older runs: `sito`,
    /// `tinyfish`, `archivio`), with `+ia` when the model filled fields.
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
    #[cfg(test)]
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

    #[cfg(test)]
    pub(crate) fn role_and_person(&self) -> (Option<&str>, Option<&str>) {
        (self.role.as_deref(), self.person_name.as_deref())
    }

    /// The model read the text around the address: its person replaces the page
    /// parser's guess (no person is better than a wrong one), its role wins when it has one.
    pub(crate) fn set_described(&mut self, role: Option<String>, person_name: Option<String>) {
        if role.is_some() {
            self.role = role;
        }
        self.person_name = person_name;
    }
}

/// Run-level fields of a `search_runs` row.
pub(crate) struct RunRecord<'a> {
    /// `web`, `source` (older runs), `directory`, `crawl` or `discovery`.
    pub(crate) kind: &'a str,
    pub(crate) query_text: &'a str,
    pub(crate) source_id: Option<&'a str>,
    pub(crate) seed_count: i64,
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
        let Some(host) = url_host(seed) else {
            return Err(format!("URL non valido: {seed}"));
        };
        // Never sent to context.dev: a rejected request can disable a Free key.
        if is_internal_host(&host) {
            return Err(format!("Indirizzo non pubblico: {seed}"));
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

/// Pages never worth a credit: news, calendars, notice boards, legal pages,
/// logins, feeds, archives by tag, author or page, and files.
static NOISE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)/(?:news|notizi[a-z]*|eventi?|calendari[a-z]*|agenda|albo[a-z-]*|privacy[a-z-]*|cookie[a-z-]*|login|wp-login\.php|wp-admin|wp-content|wp-json|wp-includes|feed|rss|tags?|category|categori[ae]|author|autore|search|cerca|cart|carrello)(?:/|$|[?#.])|\.(?:pdf|jpe?g|png|gif|svg|webp|zip|rar|docx?|xlsx?|pptx?|mp[34]|ics|xml)(?:$|[?#])",
    )
    .expect("valid noise regex")
});

fn is_noise(url: &str) -> bool {
    NOISE_RE.is_match(&url_path(url)) || is_pagination(url)
}

/// Words that point at mailboxes on a site of each category. A first proposal:
/// the spike measures how well each set picks the right pages and corrects it.
static CATEGORY_KEYWORDS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        ("Istruzione", "contatt|segreteria|dirigen|referen|bullism|cyberbull|organigramma|docenti|uffici|personale"),
        ("Pubblica amministrazione", "uffici|organigramma|urp|protocollo|pec|segreteria|sindaco|giunta|contatt"),
        ("Enti religiosi", "parrocchi|parroco|sacerdot|segreteria|consiglio|pastorale|vicari|curia|canonica|oratori|contatt"),
        ("Associazioni", "direttivo|consiglio|president|segreteria|soci|sezion|gruppo|chi.?siamo|contatt"),
        ("Cultura", "contatt|direzione|uffici|staff|informazioni|chi.?siamo"),
        ("Turismo e promozione territoriale", "ufficio|informazioni|contatt|direttivo|chi.?siamo"),
        ("", "contatt|segreteria|chi.?siamo|organigramma|uffici|direzione|staff|president|consiglio"),
    ]
    .into_iter()
    .map(|(category, words)| (category, Regex::new(words).expect("valid keyword regex")))
    .collect()
});

/// Keyword set of a category; unknown categories, Varie and Sanità get the generic one.
fn category_keywords(category: Option<&str>) -> &'static Regex {
    let sets: &'static Vec<(&str, Regex)> = &CATEGORY_KEYWORDS;
    let (_, keywords) = sets
        .iter()
        .find(|(name, _)| category.is_some_and(|category| category.eq_ignore_ascii_case(name)))
        .unwrap_or(&sets[sets.len() - 1]);
    keywords
}

/// Local, free score of a candidate page: category words in the URL count
/// most, then in the link text or the title and description Map returns.
/// Deep paths and query strings rank a page lower but never below a page with
/// no keyword at all: a "Contatti" link to `index.php?id=21` is worth reading.
fn page_score(url: &str, label: &str, keywords: &Regex) -> i32 {
    let lower = url_path(url).to_lowercase();
    let (path, query) = lower.split_once('?').unwrap_or((&lower, ""));
    // The query counts too: old CMSs put the page name there (`?page=contatti`).
    let in_path = keywords.find_iter(&lower).count().min(2) as i32;
    let in_label = keywords.find_iter(&label.to_lowercase()).count().min(2) as i32;
    let depth = path.split('/').filter(|part| !part.is_empty()).count();
    let signal = 3 * in_path + 2 * in_label;
    let penalty = depth.saturating_sub(2) as i32 + if query.is_empty() { 0 } else { 2 };
    if signal > 0 {
        (signal - penalty).max(1)
    } else {
        -penalty
    }
}

/// Query parameters that only switch the language of the same page.
const LANGUAGE_PARAMS: [&str; 5] = ["lang", "language", "locale", "hl", "lng"];

/// Same page whatever the scheme, `www.`, trailing slash or language switch.
fn page_key(url: &str) -> String {
    let host = url_host(url).unwrap_or_default();
    let path = url_path(url);
    let path = path.split('#').next().unwrap_or("");
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let query: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or("").to_lowercase();
            !pair.is_empty() && !LANGUAGE_PARAMS.contains(&name.as_str())
        })
        .collect();
    format!(
        "{}{}{}",
        host.trim_start_matches("www."),
        path.trim_end_matches('/'),
        if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        }
    )
}

fn same_host(left: &str, right: &str) -> bool {
    left.trim_start_matches("www.") == right.trim_start_matches("www.")
}

/// Home links and Map URLs on the site's own host, noise removed, best score
/// first (ties keep home links before Map URLs). `(url, label, score)`.
fn rank_candidates(
    host: &str,
    read: &HashSet<String>,
    anchors: &[(String, String)],
    mapped: &[crate::contextdev::MappedUrl],
    category: Option<&str>,
) -> Vec<(String, String, i32)> {
    let keywords = category_keywords(category);
    let mut seen: HashSet<String> = read.iter().map(|url| page_key(url)).collect();
    let mut ranked: Vec<(String, String, i32)> = anchors
        .iter()
        .map(|(url, text)| (url.clone(), text.clone()))
        .chain(mapped.iter().map(|page| {
            (
                page.url.clone(),
                format!("{} {}", page.title, page.description),
            )
        }))
        .filter(|(url, _)| {
            url_host(url).is_some_and(|link| same_host(&link, host))
                && !is_noise(url)
                && seen.insert(page_key(url))
        })
        .map(|(url, label)| {
            let score = page_score(&url, &label, keywords);
            (strip_fragment(&url), label.trim().to_owned(), score)
        })
        .collect();
    ranked.sort_by_key(|(_, _, score)| std::cmp::Reverse(*score));
    ranked
}

/// The pages to read: the model's choices first, then every page with a
/// positive local score, at most `count`.
fn pick_pages(ranked: &[(String, String, i32)], chosen: &[String], count: usize) -> Vec<String> {
    let mut picked: Vec<String> = Vec::new();
    let positive = ranked
        .iter()
        .filter(|(_, _, score)| *score > 0)
        .map(|(url, _, _)| url);
    for url in chosen.iter().chain(positive) {
        if picked.len() >= count {
            break;
        }
        if !picked.iter().any(|known| page_key(known) == page_key(url)) {
            picked.push(url.clone());
        }
    }
    picked
}

/// PDF links worth a credit on a school site: roles, staff lists, circolari.
static PDF_ROLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)referen|bullism|organigramma|funzionigramma|incarich|ruoli|staff|circolar|nomin|commission")
        .expect("valid pdf role regex")
});

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

    // Cloudflare hides addresses behind `data-cfemail` and `email-protection#`.
    for captures in CFEMAIL_RE.captures_iter(html) {
        let (Some(full), Some(cleaned)) = (captures.get(0), decode_cfemail(&captures[1])) else {
            continue;
        };
        if is_valid_email_syntax(&cleaned) && seen.insert(email_comparison_key(&cleaned)) {
            emails.push(EmailHit {
                email: cleaned,
                context: context_from_html(html, full.start(), full.end()),
            });
        }
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

/// Cloudflare email obfuscation: the first byte is the XOR key of the rest.
fn decode_cfemail(hex: &str) -> Option<String> {
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .map(|index| u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok())
        .collect::<Option<_>>()?;
    let (key, rest) = bytes.split_first()?;
    let decoded: Vec<u8> = rest.iter().map(|byte| byte ^ key).collect();
    String::from_utf8(decoded)
        .ok()
        .map(|text| clean_email(&text))
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

/// Crawl state of one host.
struct HostState {
    host: String,
    cap: usize,
    pdf_cap: usize,
    pdf_count: usize,
    queue: VecDeque<String>,
    visited: HashSet<String>,
    seen_content: HashSet<u64>,
    /// Addresses found so far on this host, and whether the last page added any.
    seen_emails: HashSet<String>,
    last_page_new: bool,
    /// Directory mode: only links under this path (plus pagination) are followed.
    prefix: Option<String>,
    crawl: HostCrawl,
}

/// Result of crawling one host.
#[derive(Debug, Default)]
pub(crate) struct HostCrawl {
    pub(crate) pages: Vec<FetchedPage>,
    pub(crate) errors: Vec<String>,
    pub(crate) pdf_found: usize,
    /// `(url, anchor text)` of links to other sites: candidate new seeds.
    pub(crate) outbound: Vec<(String, String)>,
    /// The host the site answered from after redirects, when it differs.
    pub(crate) site_host: Option<String>,
    /// Nothing read because of context.dev (credits, capacity, key), not the
    /// site: the seed stays pending for the next run.
    pub(crate) retry_later: bool,
    /// Calls sent for this host, one credit each by the price list.
    // shortcut: retries and Maps surcharges are not counted per host (the run's
    // total is exact); count `key_metadata` per call if per-seed figures matter.
    pub(crate) credits: u64,
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
    for hit in &parsed.emails {
        state.last_page_new |= state.seen_emails.insert(email_comparison_key(&hit.email));
    }
    // Directory mode follows the list; a site's pages are chosen up front.
    if let Some(prefix) = &state.prefix {
        for link in parsed.links {
            if state.queue.len() < state.cap * 3
                && !state.visited.contains(&link)
                && url_host(&link).as_deref() == Some(state.host.as_str())
                && (url_path(&link).starts_with(prefix.as_str()) || is_pagination(&link))
                && !state.queue.contains(&link)
            {
                state.queue.push_back(link);
            }
        }
    }
    // Links to other sites (a diocese's parishes, a comune's associations, a
    // directory's members): discovery candidates, at no extra credit.
    let cap = if state.prefix.is_some() {
        usize::MAX
    } else {
        MAX_OUTBOUND_PER_SITE
    };
    for (link, text) in page_anchors(&url, &html) {
        if state.crawl.outbound.len() >= cap {
            break;
        }
        let Some(host) = url_host(&link) else {
            continue;
        };
        if crate::lead_crawl::same_site(host.trim_start_matches("www."), &state.host)
            || is_portal(&host)
            || is_noise(&link)
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

/// Settings shared by every host of a crawl.
pub(crate) struct CrawlSettings<'a> {
    pub(crate) context: &'a crate::contextdev::ContextDev,
    /// Pages per site, the home included (directory mode: pages of the list).
    pub(crate) max_pages: usize,
    /// Ceiling once a site keeps yielding new addresses past `max_pages`
    /// (ignored in directory mode).
    pub(crate) extend_to: usize,
    /// Map every site, even one whose home links already offer good pages.
    pub(crate) map_always: bool,
    /// PDFs per site; only Istruzione seeds use them.
    pub(crate) max_pdfs: usize,
    /// Directory mode: follow links under the start path and keep links to
    /// other sites (`HostCrawl::outbound`).
    pub(crate) directory: bool,
    pub(crate) ai: Option<&'a crate::lead_ai::LeadAi>,
    pub(crate) cancel: &'a std::sync::atomic::AtomicBool,
}

const VIA: &str = "contextdev";

impl CrawlSettings<'_> {
    fn stopped(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
            || self.context.stop_reason().is_some()
    }
}

/// Whether a call reached context.dev and was charged: refusals, credit, key
/// and capacity errors cost nothing.
fn sent<T>(result: &Result<T, crate::contextdev::ContextError>) -> bool {
    use crate::contextdev::ContextError;
    !matches!(
        result,
        Err(ContextError::BudgetReached
            | ContextError::UsageExceeded
            | ContextError::RateLimited(_)
            | ContextError::Unauthorized(_)
            | ContextError::BadRequest(_))
    )
}

/// Record a failed page; a context.dev error (not the site's) that stops the
/// client ends the host. Returns whether to go on.
fn page_failed(
    settings: &CrawlSettings<'_>,
    state: &mut HostState,
    url: &str,
    error: &crate::contextdev::ContextError,
) -> bool {
    state.crawl.errors.push(format!("{url}: {error}"));
    !settings.stopped()
}

/// Map of the site, limited to the seed's path when it has one
/// (`wixsite.com/nome`), otherwise Map would list the whole platform.
async fn map_site(
    settings: &CrawlSettings<'_>,
    state: &mut HostState,
    seed: &str,
    prefix: Option<&str>,
) -> Vec<crate::contextdev::MappedUrl> {
    let path = prefix.map(str::to_owned).unwrap_or_else(|| {
        let path = url_path(seed);
        path.split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_owned()
    });
    let regex = (!path.is_empty() && path != "/")
        .then(|| {
            format!(
                "^https?://{}{}",
                regex::escape(&state.host),
                regex::escape(&path)
            )
        })
        .filter(|regex| regex.len() <= 256);
    let result = settings
        .context
        .map(&state.host, regex.as_deref(), None)
        .await;
    state.crawl.credits += u64::from(sent(&result));
    match result {
        Ok(urls) => urls,
        Err(error) => {
            state
                .crawl
                .errors
                .push(format!("{}: Map: {error}", state.host));
            Vec::new()
        }
    }
}

/// Read one page and ingest it. `None` when the host should stop.
async fn read_page(settings: &CrawlSettings<'_>, state: &mut HostState, url: &str) -> Option<()> {
    if settings.stopped() {
        return None;
    }
    state.visited.insert(url.to_owned());
    state.last_page_new = false;
    let result = settings.context.scrape_html(url).await;
    state.crawl.credits += u64::from(sent(&result));
    match result {
        Ok(page) => {
            let pdfs = ingest_page(state, page.final_url, page.content, VIA);
            state.crawl.pdf_found += pdfs.len();
            Some(())
        }
        Err(error) => page_failed(settings, state, url, &error).then_some(()),
    }
}

/// Up to `pdf_cap` PDFs whose link names a role, staff list or circolare.
async fn read_role_pdfs(settings: &CrawlSettings<'_>, state: &mut HostState) {
    if state.pdf_cap == 0 {
        return;
    }
    let mut links: Vec<(String, String)> = Vec::new();
    for page in &state.crawl.pages {
        for (url, text) in page_anchors(&page.url, &page.html) {
            if is_pdf_url(&url)
                && PDF_ROLE_RE.is_match(&format!("{url} {text}"))
                && !links.iter().any(|(known, _)| *known == url)
            {
                links.push((url, text));
            }
        }
    }
    for (url, _) in links {
        if state.pdf_count >= state.pdf_cap || settings.stopped() {
            break;
        }
        if !state.visited.insert(url.clone()) {
            continue;
        }
        let result = settings.context.scrape_markdown(&url).await;
        state.crawl.credits += u64::from(sent(&result));
        match result {
            Ok(page) => {
                state.pdf_count += 1;
                ingest_page(state, url, page.content, VIA);
            }
            Err(error) => {
                if !page_failed(settings, state, &url, &error) {
                    break;
                }
            }
        }
    }
}

/// Social networks and portals: a site that redirects there is not the organization's own.
pub(crate) fn is_portal(host: &str) -> bool {
    OUTBOUND_NOISE.iter().any(|noise| host.contains(noise))
        || crate::contextdev::EXCLUDED_DOMAINS
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// Read one site through context.dev: the home, then Map of the site, then the
/// best pages by the local score for the seed's category (the model's picks
/// first when enabled), then role PDFs for schools. At most `max_pages`
/// Scrape credits plus one for Map. Directory mode reads the list instead.
pub(crate) async fn crawl_host(
    settings: &CrawlSettings<'_>,
    seeds: &[String],
    category: Option<&str>,
) -> HostCrawl {
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
    let is_school = category.is_some_and(|category| category.eq_ignore_ascii_case("Istruzione"));
    let mut state = HostState {
        host: host.clone(),
        cap: settings.max_pages.max(1),
        pdf_cap: if is_school { settings.max_pdfs } else { 0 },
        pdf_count: 0,
        queue: VecDeque::new(),
        visited: HashSet::from([seed.clone()]),
        seen_content: HashSet::new(),
        seen_emails: HashSet::new(),
        last_page_new: false,
        prefix,
        crawl: HostCrawl::default(),
    };

    if settings.stopped() {
        state.crawl.retry_later = true;
        return state.crawl;
    }
    let mut home = settings.context.scrape_html(&seed).await;
    state.crawl.credits += u64::from(sent(&home));
    // Seeds are stored as https://, but some small sites only answer over plain
    // http (or redirect from it to their real domain): try that once.
    if let (Err(error), Some(rest)) = (&home, seed.strip_prefix("https://"))
        && error.is_access_error()
        && !settings.stopped()
    {
        let plain = settings
            .context
            .scrape_html(&format!("http://{rest}"))
            .await;
        state.crawl.credits += u64::from(sent(&plain));
        if plain.is_ok() {
            home = plain;
        }
    }
    let home = match home {
        Ok(page) => page,
        Err(error) => {
            state.crawl.errors.push(format!("{seed}: {error}"));
            state.crawl.retry_later = !error.is_site_error();
            return state.crawl;
        }
    };
    if let Some(final_host) = url_host(&home.final_url).filter(|final_host| *final_host != host) {
        if is_portal(&final_host) {
            state.crawl.errors.push(format!(
                "{seed}: rimanda a {final_host}, non è il sito dell'ente"
            ));
            return state.crawl;
        }
        state.host = final_host.clone();
        state.crawl.site_host = Some(final_host);
    }
    state.visited.insert(home.final_url.clone());
    let home_url = home.final_url.clone();
    let home_html = home.content.clone();
    let pdfs = ingest_page(&mut state, home.final_url, home.content, VIA);
    state.crawl.pdf_found += pdfs.len();

    // Map costs a credit and its pages rarely hold a mailbox (measured: 0.2 new
    // addresses a page against 1.5 for the home's own links), so a site is only
    // mapped when the home links offer nothing the local score likes.
    let anchors = page_anchors(&home_url, &home_html);
    let directory = state.prefix.is_some();
    let mut ranked = rank_candidates(&state.host, &state.visited, &anchors, &[], category);
    let mapped = if settings.stopped()
        || (!directory && !settings.map_always && ranked.iter().any(|(_, _, score)| *score > 0))
    {
        Vec::new()
    } else {
        let prefix = state.prefix.clone();
        map_site(settings, &mut state, &seed, prefix.as_deref()).await
    };

    if directory {
        crawl_directory(settings, &mut state, mapped).await;
        return state.crawl;
    }

    if !mapped.is_empty() {
        ranked = rank_candidates(&state.host, &state.visited, &anchors, &mapped, category);
    }
    let mut chosen: Vec<String> = seeds[1..].to_vec();
    // Choosing links costs model time and, on sites with no page the local score
    // likes, extra pages that rarely hold a mailbox: the model only steps in when no
    // candidate scores above zero.
    if let Some(ai) = settings
        .ai
        .filter(|_| !ranked.iter().any(|(_, _, score)| *score > 0))
    {
        let offered: Vec<(String, String)> = ranked
            .iter()
            .take(MAX_AI_CANDIDATES)
            .map(|(url, label, _)| (url.clone(), label.clone()))
            .collect();
        match ai.choose_links(&state.host, &offered).await {
            Ok(links) => chosen.extend(links),
            Err(error) => state.crawl.errors.push(format!("{host}: IA link: {error}")),
        }
    }
    let mut streak = 0usize;
    let ceiling = settings.extend_to.max(state.cap);
    for (index, url) in pick_pages(&ranked, &chosen, ceiling - 1)
        .into_iter()
        .enumerate()
    {
        // Past the pages asked for, only a site whose last page gave a new address goes on.
        if index >= state.cap - 1 && !state.last_page_new {
            break;
        }
        let before = state.crawl.errors.len();
        if read_page(settings, &mut state, &url).await.is_none() {
            break;
        }
        streak = if state.crawl.errors.len() > before {
            streak + 1
        } else {
            0
        };
        if streak >= MAX_HOST_ERRORS {
            state
                .crawl
                .errors
                .push(format!("{host}: troppe pagine non lette, mi fermo qui"));
            break;
        }
    }
    read_role_pdfs(settings, &mut state).await;
    state.crawl
}

/// Directory mode: the list's own pages (from Map and from its links, plus
/// pagination) up to `max_pages`, the start page included.
async fn crawl_directory(
    settings: &CrawlSettings<'_>,
    state: &mut HostState,
    mapped: Vec<crate::contextdev::MappedUrl>,
) {
    let prefix = state.prefix.clone().unwrap_or_default();
    for page in mapped {
        if state.queue.len() < state.cap * 3
            && url_host(&page.url).is_some_and(|link| same_host(&link, &state.host))
            && url_path(&page.url).starts_with(prefix.as_str())
            && !state.queue.contains(&page.url)
        {
            state.queue.push_back(page.url);
        }
    }
    let mut streak = 0usize;
    while let Some(url) = state.queue.pop_front() {
        if state.crawl.pages.len() >= state.cap {
            break;
        }
        if state.visited.contains(&url) || is_pdf_url(&url) {
            continue;
        }
        let before = state.crawl.errors.len();
        if read_page(settings, state, &url).await.is_none() {
            break;
        }
        streak = if state.crawl.errors.len() > before {
            streak + 1
        } else {
            0
        };
        if streak >= MAX_HOST_ERRORS {
            break;
        }
    }
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

/// Web search run: the seed URLs found by search are crawled like any other seeds.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn start_search_run(
    app: tauri::AppHandle,
    query: String,
    seed_urls: Vec<String>,
    max_pages_per_domain: Option<i64>,
    max_pdfs: Option<i64>,
    ai_model: Option<String>,
    ai_effort: Option<String>,
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
            directory: None,
            max_credits: 0,
            ai_model: ai_model.unwrap_or_default(),
            ai_effort,
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
            seen_emails: HashSet::new(),
            last_page_new: false,
            prefix: Some("/sezioni/".to_owned()),
            crawl: HostCrawl::default(),
        };
        ingest_page(
            &mut state,
            "https://www.cai.test/sezioni/".to_owned(),
            html.to_owned(),
            VIA,
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
    fn discovery_noise_pages_never_cost_a_credit() {
        for url in [
            "https://example.test/news/2026/festa",
            "https://example.test/notizie",
            "https://example.test/eventi/",
            "https://example.test/albo-pretorio/atto-12",
            "https://example.test/privacy-policy",
            "https://example.test/wp-content/uploads/foto.png",
            "https://example.test/tag/sport/",
            "https://example.test/category/avvisi",
            "https://example.test/feed",
            "https://example.test/page/3/",
            "https://example.test/documenti/circolare.pdf",
        ] {
            assert!(is_noise(url), "atteso rumore: {url}");
        }
        for url in [
            "https://example.test/",
            "https://example.test/contatti",
            "https://example.test/organigramma/uffici",
            "https://example.test/chi-siamo",
            "https://example.test/tagliamento-associazioni",
        ] {
            assert!(!is_noise(url), "atteso utile: {url}");
        }
    }

    #[test]
    fn discovery_language_switches_are_the_same_page_but_other_queries_are_not() {
        assert_eq!(
            page_key("https://www.eisp.it/contacts?locale=it-IT"),
            page_key("https://eisp.it/contacts?locale=en-GB")
        );
        assert_eq!(
            page_key("https://x.example.test/contacts/?lang=en#top"),
            page_key("http://x.example.test/contacts")
        );
        assert_ne!(
            page_key("https://x.example.test/?page_id=94"),
            page_key("https://x.example.test/?page_id=87")
        );
        assert_ne!(
            page_key("https://x.example.test/?page_id=94"),
            page_key("https://x.example.test/")
        );
        assert_eq!(
            page_key("https://x.example.test/p?lang=it&id=3"),
            page_key("https://x.example.test/p?id=3")
        );
    }

    #[test]
    fn discovery_a_keyword_is_never_cancelled_by_depth_or_query_penalties() {
        let keywords = category_keywords(Some("Cultura"));
        // The label alone says "contatti": the query penalty used to bring it to zero.
        assert!(
            page_score(
                "https://x.example.test/index.php?id=21&Itemid=115",
                "contatti",
                keywords
            ) > 0
        );
        // The page name lives in the query string.
        assert!(
            page_score(
                "https://x.example.test/index.php?page=contatti",
                "Vai",
                keywords
            ) > 0
        );
        // A deep path with a keyword stays readable, below the shallow one.
        let deep = page_score("https://x.example.test/a/b/c/d/e/contatti", "", keywords);
        let shallow = page_score("https://x.example.test/contatti", "", keywords);
        assert!(deep > 0 && deep < shallow);
        // No keyword anywhere: still not worth a credit.
        assert!(
            page_score(
                "https://x.example.test/eventi-2026/foto",
                "Galleria",
                keywords
            ) <= 0
        );
    }

    #[test]
    fn discovery_candidates_are_ranked_by_the_seed_category() {
        let anchors = vec![
            (
                "https://www.scuola.example.test/contatti".to_owned(),
                "Contatti".to_owned(),
            ),
            (
                "https://www.scuola.example.test/news/gita".to_owned(),
                "Gita".to_owned(),
            ),
            (
                "https://altro.example.test/contatti".to_owned(),
                "Altro".to_owned(),
            ),
            (
                "https://www.scuola.example.test/didattica".to_owned(),
                "Didattica".to_owned(),
            ),
        ];
        let mapped = vec![
            crate::contextdev::MappedUrl {
                url: "https://www.scuola.example.test/referente-bullismo".to_owned(),
                title: "Referente bullismo e cyberbullismo".to_owned(),
                description: String::new(),
            },
            crate::contextdev::MappedUrl {
                url: "https://scuola.example.test/contatti/".to_owned(),
                title: "Contatti".to_owned(),
                description: String::new(),
            },
        ];
        let read = HashSet::from(["https://www.scuola.example.test/".to_owned()]);
        let ranked = rank_candidates(
            "www.scuola.example.test",
            &read,
            &anchors,
            &mapped,
            Some("Istruzione"),
        );
        let urls: Vec<&str> = ranked.iter().map(|(url, _, _)| url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://www.scuola.example.test/referente-bullismo",
                "https://www.scuola.example.test/contatti",
                "https://www.scuola.example.test/didattica",
            ],
            "{ranked:?}"
        );
        assert_eq!(
            pick_pages(&ranked, &[], 2),
            [
                "https://www.scuola.example.test/referente-bullismo",
                "https://www.scuola.example.test/contatti"
            ]
        );
        // The model's choice goes first; zero-score pages are never read on their own.
        assert_eq!(
            pick_pages(
                &ranked,
                &["https://www.scuola.example.test/didattica".to_owned()],
                5
            ),
            [
                "https://www.scuola.example.test/didattica",
                "https://www.scuola.example.test/referente-bullismo",
                "https://www.scuola.example.test/contatti"
            ]
        );
        // A parish site scores its own words, unknown categories the generic set.
        let parish = page_score(
            "https://p.example.test/parroco",
            "",
            category_keywords(Some("Enti religiosi")),
        );
        assert!(parish > 0);
        assert_eq!(
            page_score(
                "https://p.example.test/parroco",
                "",
                category_keywords(None)
            ),
            0
        );
        assert!(
            page_score(
                "https://p.example.test/a/b/c/d/contatti?id=3",
                "",
                category_keywords(None)
            ) < page_score(
                "https://p.example.test/contatti",
                "",
                category_keywords(None)
            )
        );
    }

    #[test]
    fn discovery_cloudflare_protected_addresses_are_decoded() {
        let key = 0x2a_u8;
        let hex: String = std::iter::once(key)
            .chain("utente@example.test".bytes().map(|byte| byte ^ key))
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(decode_cfemail(&hex).as_deref(), Some("utente@example.test"));
        let html = format!(
            r#"<p>Segreteria <a href="/cdn-cgi/l/email-protection" class="__cf_email__" data-cfemail="{hex}">[email&#160;protected]</a></p>"#
        );
        let parsed = parse_page("https://example.test/", &html);
        let found: Vec<&str> = parsed.emails.iter().map(|hit| hit.email.as_str()).collect();
        assert_eq!(found, ["utente@example.test"]);
        assert_eq!(decode_cfemail("zz"), None);
    }

    #[test]
    fn discovery_a_page_counts_as_new_only_when_it_adds_an_address_to_the_host() {
        let mut state = HostState {
            host: "www.comune.example.test".to_owned(),
            cap: 3,
            pdf_cap: 0,
            pdf_count: 0,
            queue: VecDeque::new(),
            visited: HashSet::new(),
            seen_content: HashSet::new(),
            seen_emails: HashSet::new(),
            last_page_new: false,
            prefix: None,
            crawl: HostCrawl::default(),
        };
        let read = |state: &mut HostState, path: &str, html: &str| {
            state.last_page_new = false;
            ingest_page(
                state,
                format!("https://www.comune.example.test{path}"),
                html.to_owned(),
                "test",
            );
            state.last_page_new
        };
        assert!(read(
            &mut state,
            "/",
            "<p>Segreteria segreteria@comune.example.test</p>"
        ));
        // Another page with the same address, and one without any: nothing new.
        assert!(!read(
            &mut state,
            "/contatti",
            "<p>Scrivi a Segreteria@comune.example.test per info sull'ufficio</p>"
        ));
        assert!(!read(&mut state, "/news", "<p>Nessun indirizzo qui</p>"));
        assert!(read(
            &mut state,
            "/uffici",
            "<p>Tributi tributi@comune.example.test</p>"
        ));
    }

    #[test]
    fn discovery_every_site_keeps_its_links_to_other_sites() {
        let html = r#"<a href="/contatti">Contatti</a>
            <a href="https://www.parrocchia-sanmarco.example.test/">Parrocchia San Marco</a>
            <a href="https://oratorio.diocesi.example.test/">Oratorio</a>
            <a href="https://www.facebook.com/diocesi">Facebook</a>
            <a href="https://www.altra.example.test/news/2026">Notizia</a>"#;
        let mut state = HostState {
            host: "www.diocesi.example.test".to_owned(),
            cap: 3,
            pdf_cap: 0,
            pdf_count: 0,
            queue: VecDeque::new(),
            visited: HashSet::new(),
            seen_content: HashSet::new(),
            seen_emails: HashSet::new(),
            last_page_new: false,
            prefix: None,
            crawl: HostCrawl::default(),
        };
        ingest_page(
            &mut state,
            "https://www.diocesi.example.test/".to_owned(),
            html.to_owned(),
            VIA,
        );
        assert!(state.queue.is_empty(), "a site's pages are chosen up front");
        assert_eq!(
            state.crawl.outbound,
            [(
                "https://www.parrocchia-sanmarco.example.test/".to_owned(),
                "Parrocchia San Marco".to_owned()
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
}
