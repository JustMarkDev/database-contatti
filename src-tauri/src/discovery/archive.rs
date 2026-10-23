use super::*;

pub(super) const WAYBACK_CDX: &str = "https://web.archive.org/cdx/search/cdx";
pub(super) const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(90);
/// ponytail: one global gate keeps every host together under ~40 archive
/// requests a minute; per-endpoint budgets if archive.org changes its limits.
pub(super) const ARCHIVE_INTERVAL: Duration = Duration::from_millis(1500);
pub(super) static ARCHIVE_GATE: LazyLock<futures::lock::Mutex<Option<std::time::Instant>>> =
    LazyLock::new(|| futures::lock::Mutex::new(None));

pub(super) async fn archive_turn() {
    let mut last = ARCHIVE_GATE.lock().await;
    if let Some(at) = *last {
        pause(ARCHIVE_INTERVAL.saturating_sub(at.elapsed())).await;
    }
    *last = Some(std::time::Instant::now());
}

/// Timestamp of the newest HTTP 200 snapshot, from a CDX answer
/// (`output=json&fl=timestamp`: a header row, then one row per capture).
pub(crate) fn wayback_snapshot(body: &str) -> Option<String> {
    let rows: Vec<Vec<String>> = serde_json::from_str(body).ok()?;
    rows.into_iter()
        .skip(1)
        .filter_map(|row| row.into_iter().next())
        .filter(|timestamp| {
            timestamp.len() >= 8
                && timestamp
                    .chars()
                    .all(|character| character.is_ascii_digit())
        })
        .max()
}

/// The page as archived by the Wayback Machine (`id_` = original bytes, no
/// toolbar). Used only for robots-allowed pages the live site blocks.
pub(crate) async fn fetch_archived(client: &reqwest::Client, url: &str) -> Result<String, String> {
    archive_turn().await;
    // The CDX index answers slowly (20-30s is normal), so it gets its own timeout.
    let response = client
        .get(WAYBACK_CDX)
        .query(&[
            ("url", url.split('#').next().unwrap_or(url)),
            ("output", "json"),
            ("fl", "timestamp"),
            ("filter", "statuscode:200"),
            ("limit", "-3"),
        ])
        .timeout(ARCHIVE_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("archive.org: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("archive.org: HTTP {}", response.status().as_u16()));
    }
    let body = read_text(response, MAX_HTML_BYTES).await?;
    let timestamp =
        wayback_snapshot(&body).ok_or_else(|| "nessuna copia in archivio".to_owned())?;
    archive_turn().await;
    let html = fetch_html(
        client,
        &format!("https://web.archive.org/web/{timestamp}id_/{url}"),
    )
    .await
    .map_err(|error| error.message)?;
    if is_bot_wall(&html) {
        return Err("anche la copia in archivio è una pagina anti-bot".to_owned());
    }
    Ok(html)
}

pub(super) const COMMON_CRAWL_COLLECTIONS: &str = "https://index.commoncrawl.org/collinfo.json";
pub(super) const COMMON_CRAWL_DATA: &str = "https://data.commoncrawl.org/";
/// Newest crawls searched per page; older crawls rarely add a page the newer ones miss.
pub(super) const COMMON_CRAWL_INDEXES: usize = 2;
pub(super) static COMMON_CRAWL_APIS: LazyLock<futures::lock::Mutex<Vec<String>>> =
    LazyLock::new(|| futures::lock::Mutex::new(Vec::new()));

/// `cdx-api` URLs of the newest crawls, from `collinfo.json` (newest first).
pub(crate) fn common_crawl_apis(collinfo: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(collinfo)
        .unwrap_or_default()
        .iter()
        .filter_map(|collection| collection["cdx-api"].as_str())
        .filter(|api| api.starts_with("https://index.commoncrawl.org/"))
        .take(COMMON_CRAWL_INDEXES)
        .map(str::to_owned)
        .collect()
}

/// `(WARC file, offset, length)` of the newest HTTP 200 capture in a CDX
/// answer (one JSON object per line).
pub(crate) fn common_crawl_record(body: &str) -> Option<(String, u64, u64)> {
    body.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| record["status"].as_str() == Some("200"))
        .filter_map(|record| {
            let filename = record["filename"].as_str()?;
            let offset = record["offset"].as_str()?.parse().ok()?;
            let length: u64 = record["length"].as_str()?.parse().ok()?;
            let timestamp = record["timestamp"].as_str().unwrap_or("").to_owned();
            (!filename.contains("..") && length > 0 && length <= MAX_HTML_BYTES as u64)
                .then(|| (timestamp, (filename.to_owned(), offset, length)))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .map(|(_, record)| record)
}

/// HTML body of one gzip-compressed WARC response record: the WARC headers,
/// then the HTTP headers, then the page.
pub(crate) fn warc_html(record: &[u8]) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(
            flate2::read::GzDecoder::new(record),
            (MAX_HTML_BYTES * 2) as u64,
        ),
        &mut bytes,
    )
    .map_err(|error| format!("record WARC non valido: {error}"))?;
    let split = |data: &[u8]| {
        data.windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
    };
    let http_start = split(&bytes).ok_or("record WARC senza intestazioni")?;
    let http = &bytes[http_start..];
    if !http.starts_with(b"HTTP/")
        || !http
            .split(|byte| *byte == b'\n')
            .next()
            .is_some_and(|line| {
                String::from_utf8_lossy(line).split_whitespace().nth(1) == Some("200")
            })
    {
        return Err("la copia in Common Crawl non è una pagina HTTP 200".to_owned());
    }
    let body_start = split(http).ok_or("risposta HTTP senza corpo")?;
    Ok(String::from_utf8_lossy(&http[body_start..]).into_owned())
}

/// The page as stored by Common Crawl: CDX lookup on the newest crawls, then
/// a byte-range read of the WARC record from data.commoncrawl.org.
pub(crate) async fn fetch_common_crawl(
    client: &reqwest::Client,
    url: &str,
) -> Result<String, String> {
    let apis = {
        let mut cached = COMMON_CRAWL_APIS.lock().await;
        if cached.is_empty() {
            archive_turn().await;
            let collinfo = client
                .get(COMMON_CRAWL_COLLECTIONS)
                .timeout(ARCHIVE_TIMEOUT)
                .send()
                .await
                .map_err(|error| format!("index.commoncrawl.org: {error}"))?;
            let collinfo = read_text(collinfo, MAX_HTML_BYTES).await?;
            *cached = common_crawl_apis(&collinfo);
        }
        cached.clone()
    };
    let mut last_error = "nessuna copia".to_owned();
    for api in apis {
        archive_turn().await;
        let response = client
            .get(&api)
            .query(&[
                ("url", url.split('#').next().unwrap_or(url)),
                ("output", "json"),
                ("limit", "5"),
            ])
            .timeout(ARCHIVE_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("index.commoncrawl.org: {error}"))?;
        let status = response.status().as_u16();
        if status == 404 {
            continue;
        }
        if !response.status().is_success() {
            last_error = format!("indice HTTP {status}");
            continue;
        }
        let body = read_text(response, MAX_HTML_BYTES).await?;
        let Some((filename, offset, length)) = common_crawl_record(&body) else {
            continue;
        };
        archive_turn().await;
        let record = client
            .get(format!("{COMMON_CRAWL_DATA}{filename}"))
            .header(
                reqwest::header::RANGE,
                format!("bytes={offset}-{}", offset + length - 1),
            )
            .timeout(ARCHIVE_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("data.commoncrawl.org: {error}"))?;
        let record = read_limited(record, MAX_HTML_BYTES).await?;
        let html = warc_html(&record)?;
        if is_bot_wall(&html) {
            last_error = "anche la copia è una pagina anti-bot".to_owned();
            continue;
        }
        return Ok(html);
    }
    Err(last_error)
}
