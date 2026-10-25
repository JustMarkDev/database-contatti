//! context.dev client: Scrape, Map, Search and the credit balance.
//!
//! Only site URLs and search text leave the computer, never the contacts
//! database. Every call shares one semaphore because the plan's concurrency
//! limit counts requests in flight and answers an immediate 429 above it.
//! Credits consumed are summed from `key_metadata`, failed calls included.
//! The client stops itself (every later call fails without being sent) on a
//! 402, a rejected key, three 400s in a row or when the run's credit cap is hit.

use crate::ai_typesafe::TypeSafeKeyView;
use serde::Deserialize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub(crate) const SETTINGS_KEY: &str = "contextdev_api_key";
const ENV_KEY: &str = "CONTEXT_DEV_API_KEY";
const BASE_URL: &str = "https://api.context.dev/v1";
const SCRAPE_TIMEOUT_MS: u64 = 60_000;
const MAP_TIMEOUT_MS: u64 = 30_000;
const SEARCH_TIMEOUT_MS: u64 = 30_000;
/// Above the longest server-side deadline, so it arrives as a clean 408.
const HTTP_TIMEOUT: Duration = Duration::from_secs(90);
const MAP_MAX_LINKS: &str = "1000";
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Social and directory sites never hold an institution's own role mailboxes.
pub(crate) const EXCLUDED_DOMAINS: [&str; 10] = [
    "facebook.com",
    "instagram.com",
    "youtube.com",
    "linkedin.com",
    "x.com",
    "twitter.com",
    "wikipedia.org",
    "tripadvisor.it",
    "paginegialle.it",
    "paginebianche.it",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ContextError {
    /// 402: credits exhausted. Not retried; pending seeds stay pending.
    UsageExceeded,
    /// The run's own credit cap: the call was not sent.
    BudgetReached,
    /// 429: too many requests in flight. Never the site's fault.
    RateLimited(Duration),
    /// 408, 5xx, `INTERNAL_ERROR` or a network failure towards context.dev.
    Transient(String),
    Blocked,
    /// Billed at the base price: never retried.
    NotFound,
    /// Our bug. On the Free plan repeated 400s can disable the key.
    BadRequest(String),
    Unauthorized(String),
    Failed(String),
}

impl ContextError {
    /// Only these mark a seed `failed`; credit and capacity errors leave it pending.
    pub(crate) fn is_site_error(&self) -> bool {
        matches!(self, Self::Blocked | Self::NotFound | Self::Failed(_))
    }
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UsageExceeded => write!(
                f,
                "crediti context.dev esauriti: ricarica o attendi il rinnovo, poi rilancia la ricerca"
            ),
            Self::BudgetReached => write!(f, "tetto di crediti della ricerca raggiunto"),
            Self::RateLimited(_) => write!(f, "troppe richieste context.dev in parallelo"),
            Self::Transient(message) => write!(f, "context.dev non disponibile: {message}"),
            Self::Blocked => write!(f, "pagina bloccata dal sito"),
            Self::NotFound => write!(f, "pagina non trovata"),
            Self::BadRequest(message) => write!(f, "richiesta context.dev non valida: {message}"),
            Self::Unauthorized(message) => {
                write!(
                    f,
                    "chiave context.dev non valida o senza permessi: {message}"
                )
            }
            Self::Failed(message) => write!(f, "{message}"),
        }
    }
}

/// Delay before the next attempt, or `None` to give up: 429 waits as asked,
/// transient failures back off briefly, a blocked page gets one more try.
fn retry_delay(error: &ContextError, attempt: u32) -> Option<Duration> {
    match error {
        ContextError::RateLimited(wait) if attempt < 2 => Some(*wait),
        ContextError::Transient(_) if attempt < 2 => Some(Duration::from_secs(1 << attempt)),
        ContextError::Blocked if attempt < 1 => Some(Duration::from_secs(1)),
        _ => None,
    }
}

#[derive(Deserialize, Default)]
struct ErrorBody {
    message: Option<String>,
    error_data: Option<serde_json::Value>,
}

fn api_error(status: u16, body: &str, retry_after: Option<u64>) -> ContextError {
    let error: ErrorBody = serde_json::from_str(body).unwrap_or_default();
    let message = error.message.unwrap_or_else(|| format!("HTTP {status}"));
    match status {
        402 => ContextError::UsageExceeded,
        429 => {
            let seconds = retry_after
                .or_else(|| error.error_data?.get("retry_after_seconds")?.as_u64())
                .unwrap_or(5);
            ContextError::RateLimited(Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
        }
        400 => ContextError::BadRequest(message),
        401 | 403 => ContextError::Unauthorized(message),
        408 | 500..=599 => ContextError::Transient(message),
        _ => ContextError::Failed(message),
    }
}

fn credits_consumed(body: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("key_metadata")?.get("credits_consumed")?.as_u64())
        .unwrap_or(0)
}

/// Every call counts against the plan and, on Free, a 400 can disable the key:
/// only public http(s) addresses are sent.
fn check_public_host(host: &str) -> Result<(), ContextError> {
    if host.is_empty() || crate::discovery::is_internal_host(host) {
        return Err(ContextError::Failed(format!(
            "indirizzo non pubblico: {host}"
        )));
    }
    Ok(())
}

fn check_public_url(url: &str) -> Result<(), ContextError> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(ContextError::Failed(format!("URL non http: {url}")));
    }
    check_public_host(&crate::discovery::url_host(url).unwrap_or_default())
}

#[derive(Deserialize)]
struct ScrapeOutput {
    success: Option<bool>,
    data: Option<String>,
    error_code: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct ScrapeResponse {
    /// The final URL after redirects.
    url: String,
    html: Option<ScrapeOutput>,
    markdown: Option<ScrapeOutput>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ScrapedPage {
    pub(crate) final_url: String,
    pub(crate) content: String,
}

/// HTTP 200 can still carry a failed output: `success` decides.
fn scraped(body: &str, markdown: bool) -> Result<ScrapedPage, ContextError> {
    let response: ScrapeResponse = serde_json::from_str(body).map_err(|error| {
        ContextError::Failed(format!("risposta context.dev Scrape non valida: {error}"))
    })?;
    let output = if markdown {
        response.markdown
    } else {
        response.html
    }
    .ok_or_else(|| ContextError::Failed("formato mancante nella risposta".to_owned()))?;
    if let (Some(true), Some(content)) = (output.success, output.data) {
        return Ok(ScrapedPage {
            final_url: response.url,
            content,
        });
    }
    let code = output.error_code.unwrap_or_default();
    let message = output.message.unwrap_or_else(|| code.clone());
    Err(match code.as_str() {
        "WEBSITE_BLOCKED" => ContextError::Blocked,
        "NOT_FOUND" => ContextError::NotFound,
        "INTERNAL_ERROR" => ContextError::Transient(message),
        _ => ContextError::Failed(format!("{code}: {message}")),
    })
}

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct MappedUrl {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) description: String,
}

#[derive(Deserialize)]
struct MapResponse {
    urls: Vec<MappedUrl>,
}

fn mapped(body: &str) -> Result<Vec<MappedUrl>, ContextError> {
    serde_json::from_str::<MapResponse>(body)
        .map(|response| response.urls)
        .map_err(|error| {
            ContextError::Failed(format!("risposta context.dev Map non valida: {error}"))
        })
}

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct SearchHit {
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) description: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchHit>,
}

fn searched(body: &str) -> Result<Vec<SearchHit>, ContextError> {
    serde_json::from_str::<SearchResponse>(body)
        .map(|response| response.results)
        .map_err(|error| {
            ContextError::Failed(format!("risposta context.dev Search non valida: {error}"))
        })
}

/// One business from the Google Maps parser (fields used here only).
#[derive(Debug, Default, Deserialize, PartialEq)]
pub(crate) struct MapsPlace {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) website: Option<String>,
    #[serde(default)]
    pub(crate) google_category: Vec<String>,
    #[serde(default)]
    pub(crate) city: Option<String>,
    #[serde(default)]
    pub(crate) address: Option<String>,
    #[serde(default)]
    pub(crate) postal_code: Option<String>,
    #[serde(default)]
    pub(crate) open_status: Option<String>,
}

#[derive(Deserialize)]
struct MapsData {
    #[serde(default)]
    results: Vec<MapsPlace>,
}

#[derive(Deserialize)]
struct MapsOutput {
    success: Option<bool>,
    data: Option<MapsData>,
    error_code: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct MapsResponse {
    parsed: MapsOutput,
}

/// Open businesses of a Maps search; a closed one has no mailbox to find.
fn maps_places(body: &str) -> Result<Vec<MapsPlace>, ContextError> {
    let response: MapsResponse = serde_json::from_str(body).map_err(|error| {
        ContextError::Failed(format!("risposta context.dev Maps non valida: {error}"))
    })?;
    let parsed = response.parsed;
    match (parsed.success, parsed.data) {
        (Some(true), Some(data)) => Ok(data
            .results
            .into_iter()
            .filter(|place| place.open_status.as_deref() != Some("PERMANENTLY_CLOSED"))
            .collect()),
        _ => Err(ContextError::Failed(format!(
            "{}: {}",
            parsed.error_code.unwrap_or_default(),
            parsed.message.unwrap_or_default()
        ))),
    }
}

/// `https://www.google.com/maps/search/<words joined by +>/`, each word percent-encoded.
fn maps_search_url(query: &str) -> String {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|word| {
            word.bytes()
                .map(|byte| match byte {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                        (byte as char).to_string()
                    }
                    _ => format!("%{byte:02X}"),
                })
                .collect()
        })
        .collect();
    format!("https://www.google.com/maps/search/{}/", words.join("+"))
}

#[derive(Debug, Deserialize)]
pub(crate) struct Refill {
    pub(crate) at: String,
    pub(crate) credits: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AutoTopup {
    pub(crate) enabled: bool,
    pub(crate) monthly_budget_credits: Option<i64>,
}

/// `GET /org/usage`, free of charge.
#[derive(Debug, Deserialize)]
pub(crate) struct Usage {
    pub(crate) credits_remaining: i64,
    pub(crate) next_refill: Option<Refill>,
    pub(crate) auto_topup: AutoTopup,
}

impl Usage {
    /// An unbounded Auto-Topup would let a bug in the app spend without limit.
    pub(crate) fn spend_guard(&self) -> Result<(), String> {
        if self.auto_topup.enabled && self.auto_topup.monthly_budget_credits.is_none() {
            return Err("Auto-Topup di context.dev è attivo senza budget mensile: disattivalo o imposta un budget nel pannello context.dev prima di avviare la ricerca.".to_owned());
        }
        Ok(())
    }
}

pub(crate) struct ContextDev {
    http: reqwest::Client,
    key: String,
    slots: tokio::sync::Semaphore,
    concurrency: usize,
    credits_used: AtomicU64,
    /// Credits this client may spend; `None` for no cap.
    budget: Option<u64>,
    halted: Mutex<Option<ContextError>>,
    bad_requests_in_a_row: AtomicU64,
}

/// A Free-plan key can be disabled by request errors: stop well before.
const MAX_BAD_REQUESTS_IN_A_ROW: u64 = 3;

impl ContextDev {
    pub(crate) fn new(
        key: String,
        concurrency: usize,
        budget: Option<u64>,
    ) -> Result<Self, String> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .map_err(|error| error.to_string())?,
            key,
            slots: tokio::sync::Semaphore::new(concurrency.max(1)),
            concurrency: concurrency.max(1),
            credits_used: AtomicU64::new(0),
            budget,
            halted: Mutex::new(None),
            bad_requests_in_a_row: AtomicU64::new(0),
        })
    }

    /// Requests in flight this client allows.
    pub(crate) fn concurrency(&self) -> usize {
        self.concurrency
    }

    /// Credits consumed by this client so far, failed calls included.
    pub(crate) fn credits_used(&self) -> u64 {
        self.credits_used.load(Ordering::Relaxed)
    }

    /// Why a call costing `cost` credits would not be sent.
    fn refusal(&self, cost: u64) -> Option<ContextError> {
        if let Some(error) = self.halted.lock().ok()?.clone() {
            return Some(error);
        }
        self.budget
            .is_some_and(|budget| self.credits_used() + cost > budget)
            .then_some(ContextError::BudgetReached)
    }

    /// Set once the client refuses every call: the run should stop.
    pub(crate) fn stop_reason(&self) -> Option<ContextError> {
        self.refusal(1)
    }

    fn note_status(&self, status: u16, error: Option<&ContextError>) {
        let halt = match (status, error) {
            (400, _) => {
                let streak = self.bad_requests_in_a_row.fetch_add(1, Ordering::Relaxed) + 1;
                (streak >= MAX_BAD_REQUESTS_IN_A_ROW).then(|| {
                    ContextError::BadRequest(format!(
                        "{streak} richieste rifiutate di fila, ricerca fermata"
                    ))
                })
            }
            (_, Some(error @ (ContextError::UsageExceeded | ContextError::Unauthorized(_)))) => {
                Some(error.clone())
            }
            _ => {
                self.bad_requests_in_a_row.store(0, Ordering::Relaxed);
                None
            }
        };
        if let (Some(halt), Ok(mut halted)) = (halt, self.halted.lock()) {
            halted.get_or_insert(halt);
        }
    }

    async fn send_once(&self, request: reqwest::RequestBuilder) -> Result<String, ContextError> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|error| ContextError::Transient(error.to_string()))?;
        let response = request
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(|error| ContextError::Transient(error.to_string()))?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()?.trim().parse().ok());
        let body = response
            .text()
            .await
            .map_err(|error| ContextError::Transient(error.to_string()))?;
        self.credits_used
            .fetch_add(credits_consumed(&body), Ordering::Relaxed);
        let result = if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(api_error(status, &body, retry_after))
        };
        self.note_status(status, result.as_ref().err());
        result
    }

    /// `cost` is the expected price, checked against the cap before sending.
    async fn call<T>(
        &self,
        cost: u64,
        request: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
        parse: impl Fn(&str) -> Result<T, ContextError>,
    ) -> Result<T, ContextError> {
        let mut attempt = 0;
        loop {
            if let Some(error) = self.refusal(cost) {
                return Err(error);
            }
            let error = match self.send_once(request(&self.http)).await {
                Ok(body) => match parse(&body) {
                    Ok(value) => return Ok(value),
                    Err(error) => error,
                },
                Err(error) => error,
            };
            let delay = retry_delay(&error, attempt).ok_or(error)?;
            attempt += 1;
            tokio::time::sleep(delay).await;
        }
    }

    async fn scrape(&self, url: &str, markdown: bool) -> Result<ScrapedPage, ContextError> {
        check_public_url(url)?;
        // Contacts often sit in the footer, so the whole page is kept.
        let mut shared = serde_json::json!({ "country": "it", "mainContentOnly": false });
        if markdown {
            shared["parsers"] = serde_json::json!({ "pdf": { "ocr": "off" } });
        }
        let body = serde_json::json!({
            "url": url,
            "formats": { "html": !markdown, "markdown": markdown },
            "sharedParams": shared,
            "timeoutOpts": { "milliseconds": SCRAPE_TIMEOUT_MS, "behavior": "fail" },
        });
        self.call(
            1,
            |http| http.post(format!("{BASE_URL}/web/scrape")).json(&body),
            |response| scraped(response, markdown),
        )
        .await
    }

    /// One page as HTML, so `parse_page` keeps `mailto:` links and anchors.
    pub(crate) async fn scrape_html(&self, url: &str) -> Result<ScrapedPage, ContextError> {
        self.scrape(url, false).await
    }

    /// A PDF's text layer as Markdown; scans fail with `PDF_IMAGES_ONLY` (no OCR).
    pub(crate) async fn scrape_markdown(&self, url: &str) -> Result<ScrapedPage, ContextError> {
        self.scrape(url, true).await
    }

    /// The site's URLs from its sitemap (only the home links without one).
    /// `search` costs one more credit and is off unless a category proves it useful.
    pub(crate) async fn map(
        &self,
        domain: &str,
        url_regex: Option<&str>,
        search: Option<&str>,
    ) -> Result<Vec<MappedUrl>, ContextError> {
        check_public_host(domain)?;
        let timeout = MAP_TIMEOUT_MS.to_string();
        let mut query = vec![
            ("domain", domain),
            ("maxLinks", MAP_MAX_LINKS),
            ("timeoutOpts[milliseconds]", timeout.as_str()),
            ("timeoutOpts[behavior]", "fail"),
        ];
        query.extend(url_regex.map(|regex| ("urlRegex", regex)));
        query.extend(search.map(|text| ("search", text)));
        self.call(
            if search.is_some() { 2 } else { 1 },
            |http| http.get(format!("{BASE_URL}/web/urls")).query(&query),
            mapped,
        )
        .await
    }

    /// Ten Italian results (one credit), social and directory sites excluded.
    pub(crate) async fn search(&self, query: &str) -> Result<Vec<SearchHit>, ContextError> {
        let body = serde_json::json!({
            "query": query,
            "numResults": 10,
            "country": "it",
            "languages": ["it"],
            "excludeDomains": EXCLUDED_DOMAINS,
            "timeoutOpts": { "milliseconds": SEARCH_TIMEOUT_MS, "behavior": "fail" },
        });
        self.call(
            1,
            |http| http.post(format!("{BASE_URL}/web/search")).json(&body),
            searched,
        )
        .await
    }

    /// Google Maps search through the prebuilt parser (channel 3). Its cost is
    /// not documented: one credit is assumed for the cap, the real charge is counted.
    pub(crate) async fn scrape_maps(&self, query: &str) -> Result<Vec<MapsPlace>, ContextError> {
        let body = serde_json::json!({
            "url": maps_search_url(query),
            "formats": { "parse": true },
            "parseParams": { "type": "prebuilt", "parser": "google-maps" },
            "timeoutOpts": { "milliseconds": SCRAPE_TIMEOUT_MS, "behavior": "fail" },
        });
        self.call(
            1,
            |http| http.post(format!("{BASE_URL}/web/scrape")).json(&body),
            maps_places,
        )
        .await
    }

    pub(crate) async fn usage(&self) -> Result<Usage, ContextError> {
        self.call(
            0,
            |http| http.get(format!("{BASE_URL}/org/usage")),
            |body| {
                serde_json::from_str(body).map_err(|error| {
                    ContextError::Failed(format!("risposta context.dev Usage non valida: {error}"))
                })
            },
        )
        .await
    }
}

/// Google Maps discovery: off until the spike shows it is precise and cheap enough.
pub(crate) fn maps_enabled(db: &rusqlite::Connection) -> Result<bool, String> {
    Ok(crate::ai_registry::load_setting(db, MAPS_KEY)?.as_deref() == Some("1"))
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
pub(crate) struct ContextDevSettings {
    /// Requests in flight the plan allows (Free 2, Developer 10, Pro 100).
    pub(crate) concurrency: usize,
    pub(crate) maps_enabled: bool,
}

fn load_settings(db: &rusqlite::Connection) -> Result<ContextDevSettings, String> {
    Ok(ContextDevSettings {
        concurrency: concurrency(db)?,
        maps_enabled: maps_enabled(db)?,
    })
}

#[tauri::command]
pub(crate) async fn get_contextdev_settings(
    app: tauri::AppHandle,
) -> Result<ContextDevSettings, String> {
    crate::database::blocking(move || load_settings(&*crate::open_db(&app)?)).await
}

#[tauri::command]
pub(crate) async fn set_contextdev_settings(
    app: tauri::AppHandle,
    settings: ContextDevSettings,
) -> Result<ContextDevSettings, String> {
    crate::database::blocking(move || {
        let db = crate::open_db(&app)?;
        let concurrency = settings.concurrency.clamp(1, MAX_CONCURRENCY).to_string();
        crate::ai_registry::save_setting(&db, CONCURRENCY_KEY, &concurrency)?;
        crate::ai_registry::save_setting(
            &db,
            MAPS_KEY,
            if settings.maps_enabled { "1" } else { "0" },
        )?;
        load_settings(&db)
    })
    .await
}

fn env_key() -> Option<String> {
    std::env::var(ENV_KEY)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

const CONCURRENCY_KEY: &str = "contextdev_concurrency";
const MAPS_KEY: &str = "contextdev_maps_enabled";
/// The Free plan's limit; Developer allows 10, Pro 100.
const DEFAULT_CONCURRENCY: usize = 2;
const MAX_CONCURRENCY: usize = 100;

/// Requests in flight the plan allows, from the settings (default: Free plan).
pub(crate) fn concurrency(db: &rusqlite::Connection) -> Result<usize, String> {
    Ok(crate::ai_registry::load_setting(db, CONCURRENCY_KEY)?
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map_or(DEFAULT_CONCURRENCY, |value| value.clamp(1, MAX_CONCURRENCY)))
}

pub(crate) fn resolve_key(db: &rusqlite::Connection) -> Result<Option<String>, String> {
    if let Some(key) = env_key() {
        return Ok(Some(key));
    }
    Ok(crate::ai_registry::load_setting(db, SETTINGS_KEY)?
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty()))
}

fn key_view(app: &tauri::AppHandle) -> Result<TypeSafeKeyView, String> {
    if env_key().is_some() {
        return Ok(TypeSafeKeyView {
            configured: true,
            from_env: true,
        });
    }
    let db = crate::open_db(app)?;
    Ok(TypeSafeKeyView {
        configured: resolve_key(&db)?.is_some(),
        from_env: false,
    })
}

#[tauri::command]
pub(crate) async fn get_contextdev_api_key(
    app: tauri::AppHandle,
) -> Result<TypeSafeKeyView, String> {
    crate::database::blocking(move || key_view(&app)).await
}

#[tauri::command]
pub(crate) async fn set_contextdev_api_key(
    app: tauri::AppHandle,
    api_key: String,
) -> Result<TypeSafeKeyView, String> {
    crate::database::blocking(move || {
        let trimmed = api_key.trim();
        let db = crate::open_db(&app)?;
        if trimmed.is_empty() {
            db.execute(
                "DELETE FROM app_settings WHERE key=?1",
                rusqlite::params![SETTINGS_KEY],
            )
            .map_err(|error| error.to_string())?;
        } else {
            crate::ai_registry::save_setting(&db, SETTINGS_KEY, trimmed)?;
        }
        key_view(&app)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contextdev_scrape_returns_final_url_and_html_or_the_output_error() {
        let ok = r#"{"url":"https://www.scuola.example.test/contatti","request_id":"r",
            "html":{"requested":true,"success":true,"data":"<a href=\"mailto:utente@example.test\">x</a>"},
            "markdown":{"requested":false,"success":null,"data":null},
            "key_metadata":{"credits_consumed":1,"credits_remaining":999}}"#;
        assert_eq!(
            scraped(ok, false).unwrap(),
            ScrapedPage {
                final_url: "https://www.scuola.example.test/contatti".to_owned(),
                content: "<a href=\"mailto:utente@example.test\">x</a>".to_owned(),
            }
        );
        let failed = |code: &str| {
            scraped(
                &format!(
                    r#"{{"url":"https://a.example.test/","html":{{"requested":true,"success":false,"data":null,"error_code":"{code}","message":"m"}}}}"#
                ),
                false,
            )
            .unwrap_err()
        };
        assert_eq!(failed("WEBSITE_BLOCKED"), ContextError::Blocked);
        assert_eq!(failed("NOT_FOUND"), ContextError::NotFound);
        assert_eq!(
            failed("INTERNAL_ERROR"),
            ContextError::Transient("m".to_owned())
        );
        assert_eq!(
            failed("PDF_IMAGES_ONLY"),
            ContextError::Failed("PDF_IMAGES_ONLY: m".to_owned())
        );
        assert!(matches!(
            scraped("<html>", false),
            Err(ContextError::Failed(_))
        ));
    }

    #[test]
    fn contextdev_http_errors_map_to_the_retry_policy() {
        assert_eq!(api_error(402, "{}", None), ContextError::UsageExceeded);
        assert_eq!(
            api_error(429, "{}", Some(7)),
            ContextError::RateLimited(Duration::from_secs(7))
        );
        assert_eq!(
            api_error(429, r#"{"error_data":{"retry_after_seconds":3}}"#, None),
            ContextError::RateLimited(Duration::from_secs(3))
        );
        assert_eq!(
            api_error(429, "", Some(600)),
            ContextError::RateLimited(MAX_RETRY_AFTER)
        );
        assert_eq!(
            api_error(400, r#"{"message":"bad url"}"#, None),
            ContextError::BadRequest("bad url".to_owned())
        );
        assert!(matches!(
            api_error(503, "", None),
            ContextError::Transient(_)
        ));
        assert!(matches!(
            api_error(401, "", None),
            ContextError::Unauthorized(_)
        ));

        let rate = ContextError::RateLimited(Duration::from_secs(4));
        assert_eq!(retry_delay(&rate, 1), Some(Duration::from_secs(4)));
        assert_eq!(retry_delay(&rate, 2), None);
        let transient = ContextError::Transient(String::new());
        assert_eq!(retry_delay(&transient, 0), Some(Duration::from_secs(1)));
        assert_eq!(retry_delay(&transient, 1), Some(Duration::from_secs(2)));
        assert_eq!(retry_delay(&transient, 2), None);
        assert!(retry_delay(&ContextError::Blocked, 0).is_some());
        assert_eq!(retry_delay(&ContextError::Blocked, 1), None);
        for never in [
            ContextError::UsageExceeded,
            ContextError::NotFound,
            ContextError::BadRequest(String::new()),
        ] {
            assert_eq!(retry_delay(&never, 0), None);
        }

        assert!(ContextError::NotFound.is_site_error());
        assert!(!ContextError::UsageExceeded.is_site_error());
        assert!(!rate.is_site_error());
        assert!(!transient.is_site_error());
    }

    #[test]
    fn contextdev_counts_credits_and_parses_map_search_and_usage() {
        assert_eq!(
            credits_consumed(r#"{"key_metadata":{"credits_consumed":2,"credits_remaining":5}}"#),
            2
        );
        assert_eq!(credits_consumed("not json"), 0);

        let urls = mapped(
            r#"{"success":true,"domain":"a.example.test","request_id":"r","urls":[
            {"url":"https://a.example.test/referente-bullismo","title":"Referente bullismo"},
            {"url":"https://a.example.test/"}]}"#,
        )
        .unwrap();
        assert_eq!(urls[0].title, "Referente bullismo");
        assert_eq!(urls[1].description, "");

        let hits = searched(
            r#"{"query":"q","request_id":"r","cache_metadata":{"status":"miss","age_ms":0},"results":[
            {"url":"https://prolocox.example.test/","title":"Pro Loco X","description":"d","relevance":"high",
             "markdown":{"markdown":null,"code":"NOT_REQUESTED"},"highlights":{"highlights":null,"code":"NOT_REQUESTED"}}]}"#,
        )
        .unwrap();
        assert_eq!(hits[0].title, "Pro Loco X");

        let usage = |topup: &str| -> Usage {
            serde_json::from_str(&format!(
                r#"{{"credits_remaining":900,"period":null,"next_refill":{{"at":"2026-11-01T00:00:00Z","credits":1000}},"auto_topup":{topup},"request_id":"r"}}"#
            ))
            .unwrap()
        };
        let unbounded =
            usage(r#"{"enabled":true,"credits_used_this_period":0,"monthly_budget_credits":null}"#);
        assert!(unbounded.spend_guard().is_err());
        assert!(
            usage(r#"{"enabled":true,"credits_used_this_period":0,"monthly_budget_credits":5000}"#)
                .spend_guard()
                .is_ok()
        );
        let off = usage(
            r#"{"enabled":false,"credits_used_this_period":0,"monthly_budget_credits":null}"#,
        );
        assert!(off.spend_guard().is_ok());
        assert_eq!(off.next_refill.unwrap().credits, 1000);
    }

    #[test]
    fn contextdev_client_stops_on_credits_key_bad_requests_and_its_cap() {
        let client = ContextDev::new("k".to_owned(), 2, Some(10)).unwrap();
        assert_eq!(client.stop_reason(), None);
        client.credits_used.store(9, Ordering::Relaxed);
        assert_eq!(client.refusal(1), None);
        assert_eq!(client.refusal(2), Some(ContextError::BudgetReached));
        client.credits_used.store(10, Ordering::Relaxed);
        assert_eq!(client.stop_reason(), Some(ContextError::BudgetReached));

        let client = ContextDev::new("k".to_owned(), 2, None).unwrap();
        client.note_status(400, None);
        client.note_status(400, None);
        client.note_status(200, None);
        client.note_status(400, None);
        client.note_status(400, None);
        assert_eq!(client.stop_reason(), None, "a success resets the streak");
        client.note_status(400, None);
        assert!(matches!(
            client.stop_reason(),
            Some(ContextError::BadRequest(_))
        ));

        let client = ContextDev::new("k".to_owned(), 2, None).unwrap();
        client.note_status(402, Some(&ContextError::UsageExceeded));
        client.note_status(401, Some(&ContextError::Unauthorized(String::new())));
        assert_eq!(client.stop_reason(), Some(ContextError::UsageExceeded));
        assert_eq!(client.refusal(0), Some(ContextError::UsageExceeded));
    }

    #[test]
    fn contextdev_maps_parser_keeps_open_businesses_and_encodes_the_search() {
        let body = r#"{"url":"u","parsed":{"type":"google-maps","requested":true,"success":true,
            "data":{"results":[
              {"place_id":"a","name":"Pro Loco Clusone","website":"https://prolococlusone.example.test/",
               "google_category":["Associazione"],"city":"Clusone","postal_code":"24023","open_status":null},
              {"place_id":"b","name":"Chiusa","website":null,"google_category":[],"city":"Clusone",
               "open_status":"PERMANENTLY_CLOSED"}]}}}"#;
        let places = maps_places(body).unwrap();
        assert_eq!(places.len(), 1);
        assert_eq!(places[0].city.as_deref(), Some("Clusone"));
        assert_eq!(places[0].google_category, ["Associazione"]);
        let failed = r#"{"parsed":{"type":"google-maps","requested":true,"success":false,"data":null,
            "error_code":"WEBSITE_BLOCKED","message":"m"}}"#;
        assert!(matches!(maps_places(failed), Err(ContextError::Failed(_))));
        assert_eq!(
            maps_search_url("Pro Loco Sant'Omobono"),
            "https://www.google.com/maps/search/Pro+Loco+Sant%27Omobono/"
        );
    }

    #[test]
    fn contextdev_never_sends_internal_or_non_http_addresses() {
        for url in [
            "http://localhost:8080/contatti",
            "https://192.168.1.10/",
            "https://intranet.local/",
            "ftp://a.example.test/",
            "https:///percorso",
        ] {
            assert!(
                matches!(check_public_url(url), Err(ContextError::Failed(_))),
                "{url}"
            );
        }
        assert!(check_public_url("https://www.comune.example.test/uffici").is_ok());
        assert!(check_public_host("127.0.0.1").is_err());
    }
}
