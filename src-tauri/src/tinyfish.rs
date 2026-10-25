//! Optional TinyFish Search and Fetch for the sandboxed lead search.
//!
//! Search turns a free-form request into seed URLs the user reviews before
//! crawling. Fetch reads a page only when the site answers our own crawler with
//! a block (403, 406, 451); robots.txt exclusions are never routed around.
//! Both APIs are free: Search allows 30 requests/min, Fetch 150 URLs/min.

use crate::ai_typesafe::TypeSafeKeyView;
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

pub(crate) const SETTINGS_KEY: &str = "tinyfish_api_key";
const ENV_KEY: &str = "TINYFISH_API_KEY";
const SEARCH_URL: &str = "https://api.search.tinyfish.ai";
const FETCH_URL: &str = "https://api.fetch.tinyfish.ai";
/// Fetch renders JavaScript server-side for up to 110s per URL; the docs ask
/// clients to wait at least 150s so CDN timeouts arrive as clean errors.
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(150);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SEED_URLS: usize = 10;

fn env_key() -> Option<String> {
    std::env::var(ENV_KEY)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn resolve_key(db: &rusqlite::Connection) -> Result<Option<String>, String> {
    if let Some(key) = env_key() {
        return Ok(Some(key));
    }
    Ok(crate::ai_registry::load_setting(db, SETTINGS_KEY)?
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty()))
}

fn api_error(status: u16) -> String {
    match status {
        401 => "chiave TinyFish non valida".to_owned(),
        402 => "l'account TinyFish non ha accesso a questa API".to_owned(),
        429 => "limite TinyFish raggiunto, riprova tra un minuto".to_owned(),
        _ => format!("TinyFish ha risposto HTTP {status}"),
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    url: String,
}

/// One http(s) URL per host, in ranking order, at most the crawl's seed limit.
pub(crate) fn seed_urls(body: &str) -> Result<Vec<String>, String> {
    let response: SearchResponse = serde_json::from_str(body)
        .map_err(|error| format!("risposta TinyFish Search non valida: {error}"))?;
    let mut hosts = HashSet::new();
    Ok(response
        .results
        .into_iter()
        .map(|result| result.url)
        .filter(|url| {
            let lower = url.to_ascii_lowercase();
            (lower.starts_with("https://") || lower.starts_with("http://"))
                && crate::discovery::url_host(url).is_some_and(|host| hosts.insert(host))
        })
        .take(MAX_SEED_URLS)
        .collect())
}

#[derive(Deserialize)]
struct FetchResponse {
    #[serde(default)]
    results: Vec<FetchResult>,
    #[serde(default)]
    errors: Vec<FetchFailure>,
}

#[derive(Deserialize)]
struct FetchResult {
    text: Option<String>,
}

#[derive(Deserialize)]
struct FetchFailure {
    error: String,
}

pub(crate) fn fetched_html(body: &str) -> Result<String, String> {
    let response: FetchResponse = serde_json::from_str(body)
        .map_err(|error| format!("risposta TinyFish Fetch non valida: {error}"))?;
    if let Some(text) = response.results.into_iter().find_map(|result| result.text) {
        return Ok(text);
    }
    Err(response.errors.first().map_or_else(
        || "nessun contenuto".to_owned(),
        |failure| failure.error.clone(),
    ))
}

async fn read_ok(response: reqwest::Response) -> Result<String, String> {
    let status = response.status();
    let body = response.text().await.map_err(|error| error.to_string())?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(api_error(status.as_u16()))
    }
}

pub(crate) async fn fetch_html(
    client: &reqwest::Client,
    key: &str,
    url: &str,
) -> Result<String, String> {
    let response = client
        .post(FETCH_URL)
        .header("X-API-Key", key)
        .json(&serde_json::json!({ "urls": [url], "format": "html" }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    fetched_html(&read_ok(response).await?)
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
pub(crate) async fn get_tinyfish_api_key(app: tauri::AppHandle) -> Result<TypeSafeKeyView, String> {
    crate::database::blocking(move || key_view(&app)).await
}

#[tauri::command]
pub(crate) async fn set_tinyfish_api_key(
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

/// Web search for the request, returning seed URLs for the user to review.
#[tauri::command]
pub(crate) async fn find_seed_urls(
    app: tauri::AppHandle,
    query: String,
) -> Result<Vec<String>, String> {
    let query = query.trim().to_owned();
    if query.is_empty() {
        return Err("Scrivi cosa cercare prima di trovare i siti.".to_owned());
    }
    let key = tauri::async_runtime::spawn_blocking(move || {
        let db = crate::open_db(&app)?;
        resolve_key(&db)
    })
    .await
    .map_err(|error| error.to_string())??
    .ok_or_else(|| "Imposta la chiave TinyFish per cercare sul web.".to_owned())?;
    let client =
        crate::discovery::polite_client(SEARCH_TIMEOUT).map_err(|error| error.to_string())?;
    search(&client, &key, &query).await
}

/// TinyFish Search: up to ten URLs, one per host, social and directory sites excluded.
pub(crate) async fn search(
    client: &reqwest::Client,
    key: &str,
    query: &str,
) -> Result<Vec<String>, String> {
    let response = client
        .get(SEARCH_URL)
        .header("X-API-Key", key)
        .query(&[
            ("query", query),
            ("location", "IT"),
            ("language", "it"),
            (
                "exclude_domains",
                &crate::contextdev::EXCLUDED_DOMAINS.join(","),
            ),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?;
    seed_urls(&read_ok(response).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tinyfish_search_keeps_one_http_url_per_host_in_rank_order() {
        let body = r#"{"query":"q","page":0,"results":[
            {"position":1,"url":"https://www.scuola.example.test/bullismo","title":"a"},
            {"position":2,"url":"https://www.scuola.example.test/contatti","title":"b"},
            {"position":3,"url":"ftp://files.example.test/x","title":"c"},
            {"position":4,"url":"https://ic.example.test/","title":"d"}
        ]}"#;
        assert_eq!(
            seed_urls(body).unwrap(),
            [
                "https://www.scuola.example.test/bullismo",
                "https://ic.example.test/"
            ]
        );
        let many: String = (0..15)
            .map(|index| format!(r#"{{"url":"https://s{index}.example.test/"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            seed_urls(&format!(r#"{{"results":[{many}]}}"#))
                .unwrap()
                .len(),
            10
        );
        assert!(seed_urls("<html>").is_err());
    }

    #[test]
    fn tinyfish_fetch_returns_page_html_or_the_per_url_error() {
        let ok = r#"{"results":[{"url":"https://a.example.test/","text":"<p>utente@example.test</p>"}],"errors":[]}"#;
        assert_eq!(fetched_html(ok).unwrap(), "<p>utente@example.test</p>");
        let blocked =
            r#"{"results":[],"errors":[{"url":"https://a.example.test/","error":"bot_blocked"}]}"#;
        assert_eq!(fetched_html(blocked).unwrap_err(), "bot_blocked");
        assert_eq!(
            api_error(429),
            "limite TinyFish raggiunto, riprova tra un minuto"
        );
    }
}
