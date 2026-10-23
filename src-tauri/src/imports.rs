#[cfg(test)]
use crate::FACET_LOAD_COUNT;
#[cfg(test)]
pub(crate) fn acquire_import_slot_for_test() -> Result<impl Drop, String> {
    ImportSlot::acquire()
}
use crate::{
    ContactList, DELIMITED_EXTENSIONS, DashboardData, EmailStatus, ExportReport, FileReport,
    FilterOptions, ImportContact, ImportFile, ImportProgress, ImportReport, ListFilters,
    SPREADSHEET_EXTENSIONS, count_classification_reviews, display_name, email_status_counts,
    filter_where, is_blacklist_file, load_contacts, load_contacts_page, new_contact_id, open_db,
    parse_file,
};
use futures::{StreamExt, future, stream};
use hickory_resolver::TokioResolver;
use rusqlite::{Connection, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::Emitter;

static IMPORT_PAUSED: AtomicBool = AtomicBool::new(false);
static IMPORT_CANCELLED: AtomicBool = AtomicBool::new(false);
static IMPORT_RUNNING: AtomicBool = AtomicBool::new(false);

pub(crate) fn import_running() -> bool {
    IMPORT_RUNNING.load(Ordering::SeqCst)
}

/// Pause and cancel flags are process-wide, so imports must not overlap: a second
/// import would reset the first one's cancel request.
struct ImportSlot;

impl ImportSlot {
    fn acquire() -> Result<Self, String> {
        IMPORT_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| Self)
            .map_err(|_| "Un'importazione è già in corso".to_owned())
    }
}

impl Drop for ImportSlot {
    fn drop(&mut self) {
        IMPORT_RUNNING.store(false, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct PersistCounts {
    active_occurrences: usize,
    new_contacts: usize,
    existing_contacts_updated: usize,
    source_occurrences_added: usize,
}

fn load_domain_verification_cache(
    db: &Connection,
) -> Result<HashMap<String, crate::parsing::DomainValidation>, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut statement = db
        .prepare(
            "SELECT domain,routing_status,observed_ttl_seconds,dns_rcode,mx_hosts_json,checked_at,expires_at,details_json FROM domain_verification_cache
             WHERE expires_at IS NULL OR expires_at>?1",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![now], |row| {
            let status = match row.get::<_, String>(1)?.as_str() {
                "routing_ok" => EmailStatus::Valid,
                "no_routing" => EmailStatus::DomainMissingMx,
                "temporary_failure" => EmailStatus::DomainUnverified,
                _ => EmailStatus::InvalidSyntax,
            };
            let observed_ttl_seconds = row.get::<_, Option<i64>>(2)?;
            let dns_rcode = row.get::<_, Option<String>>(3)?;
            let mx_hosts =
                serde_json::from_str::<Vec<String>>(&row.get::<_, String>(4)?).unwrap_or_default();
            let dns_checked_at = row.get::<_, Option<String>>(5)?;
            let dns_expires_at = row.get::<_, Option<String>>(6)?;
            let details = row.get::<_, String>(7)?;
            let notes = serde_json::from_str::<serde_json::Value>(&details)
                .ok()
                .and_then(|value| value.get("notes").cloned())
                .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
                .unwrap_or_default();
            Ok((
                row.get::<_, String>(0)?.to_lowercase(),
                crate::parsing::DomainValidation {
                    status,
                    notes,
                    observed_ttl_seconds,
                    dns_rcode,
                    mx_hosts,
                    dns_checked_at,
                    dns_expires_at,
                },
            ))
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(|error| error.to_string())
}

const POSITIVE_CACHE_TTL_MIN_SECONDS: i64 = 300;
const POSITIVE_CACHE_TTL_MAX_SECONDS: i64 = 86_400;
const NEGATIVE_CACHE_TTL_MIN_SECONDS: i64 = 300;
const NEGATIVE_CACHE_TTL_MAX_SECONDS: i64 = 21_600;
const TEMPORARY_CACHE_TTL_SECONDS: i64 = 900;

pub(crate) fn effective_cache_ttl_seconds(
    status: EmailStatus,
    observed_ttl_seconds: Option<i64>,
) -> i64 {
    match status {
        EmailStatus::Valid => observed_ttl_seconds
            .unwrap_or(POSITIVE_CACHE_TTL_MAX_SECONDS)
            .clamp(
                POSITIVE_CACHE_TTL_MIN_SECONDS,
                POSITIVE_CACHE_TTL_MAX_SECONDS,
            ),
        EmailStatus::DomainMissingMx => observed_ttl_seconds
            .unwrap_or(NEGATIVE_CACHE_TTL_MAX_SECONDS)
            .clamp(
                NEGATIVE_CACHE_TTL_MIN_SECONDS,
                NEGATIVE_CACHE_TTL_MAX_SECONDS,
            ),
        _ => TEMPORARY_CACHE_TTL_SECONDS,
    }
}

fn persist_domain_verification_results(
    db: &Connection,
    results: &HashMap<String, crate::parsing::DomainValidation>,
) -> Result<(), String> {
    let mut upsert = db
        .prepare_cached(
            "INSERT INTO domain_verification_cache(
            domain,routing_status,dns_rcode,mx_hosts_json,ttl_seconds,observed_ttl_seconds,
            effective_cache_ttl_seconds,checked_at,expires_at,error_class,details_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(domain) DO UPDATE SET
            routing_status=excluded.routing_status,
            dns_rcode=excluded.dns_rcode,
            mx_hosts_json=excluded.mx_hosts_json,
            ttl_seconds=excluded.ttl_seconds,
            observed_ttl_seconds=excluded.observed_ttl_seconds,
            effective_cache_ttl_seconds=excluded.effective_cache_ttl_seconds,
            checked_at=excluded.checked_at,
            expires_at=excluded.expires_at,
            error_class=excluded.error_class,
            details_json=excluded.details_json",
        )
        .map_err(|error| error.to_string())?;
    for (domain, result) in results {
        let (routing_status, error_class) = match result.status {
            EmailStatus::Valid => ("routing_ok", None),
            EmailStatus::DomainMissingMx => ("no_routing", Some("definitive_dns")),
            _ => ("temporary_failure", Some("temporary_dns")),
        };
        let effective_ttl_seconds =
            effective_cache_ttl_seconds(result.status, result.observed_ttl_seconds);
        let checked_at = result
            .dns_checked_at
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let expires_at = result.dns_expires_at.clone().unwrap_or_else(|| {
            chrono::DateTime::parse_from_rfc3339(&checked_at)
                .map(|value| {
                    (value.with_timezone(&chrono::Utc)
                        + chrono::Duration::seconds(effective_ttl_seconds))
                    .to_rfc3339()
                })
                .unwrap_or_else(|_| chrono::Utc::now().to_rfc3339())
        });
        upsert
            .execute(params![
                domain,
                routing_status,
                result.dns_rcode,
                serde_json::to_string(&result.mx_hosts).unwrap_or_else(|_| "[]".to_owned()),
                effective_ttl_seconds,
                result.observed_ttl_seconds,
                effective_ttl_seconds,
                checked_at,
                expires_at,
                error_class,
                serde_json::json!({
                    "notes": result.notes,
                    "observed_ttl_seconds": result.observed_ttl_seconds,
                    "effective_cache_ttl_seconds": effective_ttl_seconds,
                    "dns_rcode": result.dns_rcode,
                    "mx_hosts": result.mx_hosts,
                })
                .to_string(),
            ])
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn validate_contacts_with_cache(
    resolver: &TokioResolver,
    blacklist: &HashSet<String>,
    cache: &mut HashMap<String, crate::parsing::DomainValidation>,
    contacts: &mut [ImportContact],
) -> Result<HashMap<String, crate::parsing::DomainValidation>, String> {
    if IMPORT_CANCELLED.load(Ordering::SeqCst) {
        return Err("Importazione annullata".to_owned());
    }
    let (immediate, groups) = crate::parsing::group_contacts_for_validation(contacts, blacklist);
    for (index, status, notes) in immediate {
        contacts[index].status = status;
        contacts[index].validation_notes = notes;
    }
    let missing = groups
        .keys()
        .filter(|domain| !cache.contains_key(*domain))
        .cloned()
        .collect::<Vec<_>>();
    let mut fresh = stream::iter(missing)
        .take_while(|_| {
            let cancelled = IMPORT_CANCELLED.load(Ordering::SeqCst);
            future::ready(!cancelled)
        })
        .map(|domain| async move {
            let validation = crate::parsing::validate_domain(resolver, &domain).await;
            (domain, validation)
        })
        .buffer_unordered(50)
        .collect::<HashMap<_, _>>()
        .await;
    if IMPORT_CANCELLED.load(Ordering::SeqCst) {
        return Err("Importazione annullata".to_owned());
    }
    if !fresh.is_empty() {
        let checked_at = chrono::Utc::now();
        for validation in fresh.values_mut() {
            let effective_ttl_seconds =
                effective_cache_ttl_seconds(validation.status, validation.observed_ttl_seconds);
            validation.dns_checked_at = Some(checked_at.to_rfc3339());
            validation.dns_expires_at =
                Some((checked_at + chrono::Duration::seconds(effective_ttl_seconds)).to_rfc3339());
        }
        cache.extend(fresh.clone());
    }
    crate::parsing::apply_domain_results(contacts, &groups, cache);
    Ok(fresh)
}

#[tauri::command]
pub(crate) fn control_import(action: String) -> Result<(), String> {
    match action.as_str() {
        "pause" => IMPORT_PAUSED.store(true, Ordering::SeqCst),
        "resume" => IMPORT_PAUSED.store(false, Ordering::SeqCst),
        "cancel" => {
            IMPORT_CANCELLED.store(true, Ordering::SeqCst);
            IMPORT_PAUSED.store(false, Ordering::SeqCst);
        }
        _ => return Err("Comando importazione non riconosciuto".to_owned()),
    }
    Ok(())
}

async fn wait_for_import_control() -> Result<(), String> {
    loop {
        if IMPORT_CANCELLED.load(Ordering::SeqCst) {
            return Err("Importazione annullata".to_owned());
        }
        if !IMPORT_PAUSED.load(Ordering::SeqCst) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn source_file_was_imported(db: &Connection, file_name: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM imports
            WHERE file_name = ?1 COLLATE BINARY AND status = 'completed'
        )",
        params![file_name],
        |row| row.get(0),
    )
    .map_err(|err| err.to_string())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn source_file_matches_import(
    db: &Connection,
    relative_path: &str,
    content_sha256: &str,
) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM imports WHERE relative_path=?1 COLLATE BINARY
             AND content_sha256=?2 AND hash_algorithm='sha256' AND status='completed')",
        params![relative_path, content_sha256],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn candidate_disposition(
    contact: &ImportContact,
    blacklisted: bool,
) -> (&'static str, f64, &'static str) {
    if crate::parsing::non_contact_mailbox_reason(&contact.normalized_email).is_some() {
        ("suppressed", 0.0, "non_contact_mailbox")
    } else if blacklisted || matches!(contact.status, EmailStatus::Blacklisted) {
        ("suppressed", 0.0, "blacklist")
    } else {
        match contact.status {
            EmailStatus::Valid => ("linked", 0.9, "routing_ok"),
            EmailStatus::InvalidSyntax => ("invalid_syntax", 0.05, "syntax_invalid"),
            EmailStatus::DomainMissingMx => ("no_routing", 0.15, "domain_no_routing"),
            EmailStatus::DomainUnverified => ("temporary_dns_failure", 0.35, "dns_unverified"),
            EmailStatus::Blacklisted => ("suppressed", 0.0, "blacklist"),
        }
    }
}

async fn import_contact_files(
    app: tauri::AppHandle,
    files: Vec<ImportFile>,
    extra_category: String,
) -> Result<ImportReport, String> {
    import_contact_files_inner(app, files, extra_category)
        .await
        .inspect_err(|error| eprintln!("[import][errore] {error}"))
}

#[tauri::command]
pub(crate) async fn import_contact_path(
    app: tauri::AppHandle,
    path: String,
    extra_category: String,
) -> Result<ImportReport, String> {
    let path_for_read = path.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let file_path = PathBuf::from(&path_for_read);
        import_file_entry(&file_path, None)
    })
    .await
    .map_err(|error| format!("Lettura file interrotta: {error}"))?;
    let file = match result {
        Ok(file) => file,
        Err(error) => {
            eprintln!("[import][errore] {error}");
            return Err(error);
        }
    };
    import_contact_files(app, vec![file], extra_category).await
}

#[tauri::command]
pub(crate) async fn import_contact_paths(
    app: tauri::AppHandle,
    paths: Vec<String>,
    extra_category: String,
) -> Result<ImportReport, String> {
    let files = tauri::async_runtime::spawn_blocking(move || {
        let mut collected = Vec::new();
        for path in paths {
            let root = PathBuf::from(&path);
            if root.is_dir() {
                collect_import_files(&root, &root, &mut collected)?;
            } else if root.is_file() {
                collected.push(import_file_entry(&root, None)?);
            }
        }

        collected.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        Ok::<_, String>(collected)
    })
    .await
    .map_err(|error| format!("Scansione cartelle interrotta: {error}"))??;
    import_contact_files(app, files, extra_category).await
}

fn collect_import_files(
    base: &PathBuf,
    root: &PathBuf,
    files: &mut Vec<ImportFile>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(root)
        .map_err(|error| format!("Impossibile leggere {}: {error}", root.display()))?
    {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            collect_import_files(base, &path, files)?;
        } else if path.is_file() && is_supported_import_file(&path) {
            files.push(import_file_entry(&path, Some(base))?);
        }
    }
    Ok(())
}

fn is_supported_import_file(path: &Path) -> bool {
    let extension = format!(
        ".{}",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    );
    SPREADSHEET_EXTENSIONS.contains(&extension.as_str())
        || DELIMITED_EXTENSIONS.contains(&extension.as_str())
}

pub(crate) fn import_file_entry(path: &Path, base: Option<&PathBuf>) -> Result<ImportFile, String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_owned();

    let relative_path = base
        .and_then(|base| path.strip_prefix(base).ok())
        .map(|relative| {
            relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .filter(|relative| !relative.is_empty())
        .unwrap_or_else(|| standalone_relative_path(path, &name));
    Ok(ImportFile {
        path: Some(path.to_path_buf()),
        name,
        relative_path,
        bytes: Vec::new(),
    })
}

pub(crate) fn standalone_relative_path(path: &Path, fallback_name: &str) -> String {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if let Some(root_index) = components
        .iter()
        .rposition(|component| component.eq_ignore_ascii_case("email"))
    {
        let relative = components[root_index + 1..].join("/");
        if !relative.is_empty() {
            return relative;
        }
    }
    if components.len() >= 2 {
        components[components.len() - 2..].join("/")
    } else {
        fallback_name.to_owned()
    }
}

fn persist_failed_file_manifest(
    app: &tauri::AppHandle,
    file: &ImportFile,
    error: &str,
) -> Result<(), String> {
    let mut db = open_db(app)?;
    let tx = db.transaction().map_err(|db_error| db_error.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    let attempt_id = new_contact_id();
    let import_id = new_contact_id();
    let digest = sha256_hex(&file.bytes);
    tx.execute(
        "INSERT INTO import_attempts(
            id,status,parser_version,started_at,completed_at,report_json,error
         ) VALUES (?1,'failed','parser-v2',?2,?2,?3,?4)",
        params![
            attempt_id,
            now,
            serde_json::json!({"file_error":error}).to_string(),
            error,
        ],
    )
    .map_err(|db_error| db_error.to_string())?;
    tx.execute(
        "INSERT INTO imports(
            id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at,
            import_attempt_id,content_sha256,hash_algorithm,file_size_bytes,parser_version,
            encoding,delimiter,sheet_count,started_at,completed_at,error_count,report_json
         ) VALUES (?1,?2,?3,?4,'failed',0,0,?5,?6,?4,'sha256',?7,'parser-v2','unknown',NULL,0,?5,?5,1,?8)",
        params![
            import_id,
            file.name,
            file.relative_path,
            digest,
            now,
            attempt_id,
            file.bytes.len() as i64,
            serde_json::json!({"file_error":error}).to_string(),
        ],
    )
    .map_err(|db_error| db_error.to_string())?;
    tx.execute(
        "INSERT INTO source_rows(
            id,import_id,source_sheet,row_number,raw_context,parse_status,error_message,
            candidate_count,created_at
         ) VALUES (?1,?2,'',1,'','failed',?3,0,?4)",
        params![format!("{import_id}::1"), import_id, error, now],
    )
    .map_err(|db_error| db_error.to_string())?;
    tx.commit().map_err(|db_error| db_error.to_string())
}

pub(crate) async fn import_contact_files_inner(
    app: tauri::AppHandle,
    files: Vec<ImportFile>,
    extra_category: String,
) -> Result<ImportReport, String> {
    let _slot = ImportSlot::acquire()?;
    IMPORT_CANCELLED.store(false, Ordering::SeqCst);
    IMPORT_PAUSED.store(false, Ordering::SeqCst);
    let resolver = TokioResolver::builder_tokio()
        .map_err(|err| format!("Resolver DNS non disponibile: {err}"))?
        .build()
        .map_err(|err| format!("Resolver DNS non disponibile: {err}"))?;
    let total_files = files.iter().filter(|file| !is_blacklist_file(file)).count();
    let overall_start = std::time::Instant::now();
    println!(
        "[import][start] files_total={} contacts_files={} blacklist_files={} extra_category=\"{}\"",
        files.len(),
        total_files,
        files.len().saturating_sub(total_files),
        extra_category
    );
    let mut imported = 0;
    let mut candidate_occurrences = 0usize;
    let mut unique_addresses = HashSet::new();
    let mut new_canonical_contacts = 0usize;
    let mut existing_contacts_updated = 0usize;
    let mut source_occurrences_added = 0usize;
    let mut syntax_invalid_candidates = 0usize;
    let mut no_routing_candidates = 0usize;
    let mut temporary_dns_failures = 0usize;
    let mut suppressed_blacklisted_candidates = 0usize;
    let mut rows_without_address = 0usize;
    let mut malformed_rows_total = 0usize;
    let mut failed_sheets_total = 0usize;
    let mut reports = Vec::new();
    let mut completed_import_ids = Vec::new();
    let app_for_import = app.clone();
    let blacklist_files: Vec<ImportFile> = files
        .iter()
        .filter(|file| is_blacklist_file(file))
        .cloned()
        .collect();
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        let mut db = open_db(&app_for_import)?;
        if !blacklist_files.is_empty() {
            let blacklist_files = blacklist_files
                .into_iter()
                .map(ImportFile::load)
                .collect::<Result<Vec<_>, _>>()?;
            crate::blacklist::import_blacklist_files_transaction(&mut db, blacklist_files, false)?;
        }
        let blacklist = crate::blacklist::load_blacklist_set_from_db(&db)?;
        let domain_cache = load_domain_verification_cache(&db)?;
        let completed_imports = {
            let mut statement = db.prepare(
                "SELECT relative_path,content_sha256 FROM imports
                 WHERE status='completed' AND hash_algorithm='sha256' AND content_sha256 IS NOT NULL",
            ).map_err(|error| error.to_string())?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            }).map_err(|error| error.to_string())?;
            rows.collect::<Result<HashSet<_>, _>>().map_err(|error| error.to_string())?
        };



        let derivation_backfill_import_ids = {
            let mut statement = db.prepare(
                "SELECT DISTINCT sr.import_id
                 FROM source_rows sr
                 JOIN email_candidates ec ON ec.source_row_id=sr.id
                 WHERE ec.routing_status<>'routing_ok' AND NOT EXISTS (
                     SELECT 1 FROM verification_events ve
                     WHERE ve.candidate_id=ec.id AND ve.method='suppression'
                 )
                 UNION
                 SELECT DISTINCT cs.import_id
                 FROM contact_sources cs
                 JOIN contacts c ON c.id=cs.contact_id
                 WHERE cs.import_id IS NOT NULL AND NOT EXISTS (
                     SELECT 1 FROM quality_score_snapshots q
                     WHERE q.contact_id=c.id AND q.score_version='initial-v1'
                 )",
            ).map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        Ok::<_, String>((
            db,
            blacklist,
            domain_cache,
            completed_imports,
            derivation_backfill_import_ids,
        ))
    })
    .await
    .map_err(|err| format!("Preparazione blacklist non riuscita: {err}"))??;
    let (db, blacklist, mut domain_cache, mut completed_imports, derivation_backfill_import_ids) =
        prepared;
    completed_import_ids.extend(derivation_backfill_import_ids);
    let mut import_db = Some(db);
    println!(
        "[import][prep] blacklist_entries={} domain_cache_entries={} completed_imports_cached={}",
        blacklist.len(),
        domain_cache.len(),
        completed_imports.len()
    );
    let _ = app.emit(
        "import-progress",
        ImportProgress {
            phase: "start".to_owned(),
            file: None,
            processed_files: 0,
            total_files,
            imported,
        },
    );

    for (processed_index, file) in files
        .into_iter()
        .filter(|file| !is_blacklist_file(file))
        .enumerate()
    {
        wait_for_import_control().await?;
        let file = tauri::async_runtime::spawn_blocking(move || file.load())
            .await
            .map_err(|error| format!("Lettura file interrotta: {error}"))??;
        let file_name = display_name(&file);
        let source_hash = sha256_hex(&file.bytes);
        let completed_import_key = (file.relative_path.clone(), source_hash.clone());
        let already_imported = completed_imports.contains(&completed_import_key);
        if already_imported {
            println!(
                "[import][file:skip] index={}/{} file=\"{}\" path=\"{}\" sha256={} reason=\"already_imported\"",
                processed_index + 1,
                total_files,
                file_name,
                file.relative_path,
                &source_hash[..8.min(source_hash.len())]
            );
            reports.push(FileReport {
                name: file_name.clone(),
                parsed_rows: 0,
                contacts_found: 0,
                unique_normalized_addresses: 0,
                new_canonical_contacts: 0,
                existing_contacts_updated: 0,
                source_occurrences_added: 0,
                syntax_invalid_candidates: 0,
                no_routing_candidates: 0,
                temporary_dns_failures: 0,
                suppressed_blacklisted_candidates: 0,
                rows_without_address: 0,
                malformed_rows: 0,
                failed_sheets: 0,
                warnings: vec!["File già importato: importazione ignorata.".to_owned()],
            });
            let _ = app.emit(
                "import-progress",
                ImportProgress {
                    phase: "skipped".to_owned(),
                    file: Some(file_name),
                    processed_files: processed_index + 1,
                    total_files,
                    imported,
                },
            );
            continue;
        }
        let file_start = std::time::Instant::now();
        println!(
            "[import][file:start] index={}/{} file=\"{}\" path=\"{}\" bytes={}",
            processed_index + 1,
            total_files,
            file_name,
            file.relative_path,
            file.bytes.len()
        );
        let _ = app.emit(
            "import-progress",
            ImportProgress {
                phase: "parsing".to_owned(),
                file: Some(file_name.clone()),
                processed_files: processed_index,
                total_files,
                imported,
            },
        );
        let (file, parse_result) = tauri::async_runtime::spawn_blocking(move || {
            let parsed = parse_file(&file);
            (file, parsed)
        })
        .await
        .map_err(|err| format!("{file_name}: parsing interrotto: {err}"))?;
        let parsed = match parse_result {
            Ok(parsed) => parsed,
            Err(error) => {
                let app_for_failure = app.clone();
                let failed_file = file.clone();
                let failure_message = error.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    persist_failed_file_manifest(&app_for_failure, &failed_file, &failure_message)
                })
                .await
                .map_err(|join_error| {
                    format!("{file_name}: registrazione errore interrotta: {join_error}")
                })??;
                reports.push(FileReport {
                    name: file_name.clone(),
                    parsed_rows: 0,
                    contacts_found: 0,
                    unique_normalized_addresses: 0,
                    new_canonical_contacts: 0,
                    existing_contacts_updated: 0,
                    source_occurrences_added: 0,
                    syntax_invalid_candidates: 0,
                    no_routing_candidates: 0,
                    temporary_dns_failures: 0,
                    suppressed_blacklisted_candidates: 0,
                    rows_without_address: 0,
                    malformed_rows: 0,
                    failed_sheets: 0,
                    warnings: vec![format!("File non leggibile: {error}")],
                });
                let _ = app.emit(
                    "import-progress",
                    ImportProgress {
                        phase: "failed".to_owned(),
                        file: Some(file_name),
                        processed_files: processed_index + 1,
                        total_files,
                        imported,
                    },
                );
                continue;
            }
        };
        wait_for_import_control().await?;
        let parse_elapsed = file_start.elapsed();
        println!(
            "[import][file:parsed] file=\"{}\" rows={} candidates={} warnings={} errors={} parse_ms={:.2}",
            file_name,
            parsed.parsed_rows,
            parsed.contacts.len(),
            parsed.warnings.len(),
            parsed.row_errors.len(),
            parse_elapsed.as_secs_f64() * 1000.0
        );
        for warning in &parsed.warnings {
            eprintln!(
                "[import][file:warning] file=\"{}\" message=\"{}\"",
                file_name, warning
            );
        }
        let mut file_contacts = parsed.contacts;
        let dns_start = std::time::Instant::now();
        let fresh_dns_results = validate_contacts_with_cache(
            &resolver,
            &blacklist,
            &mut domain_cache,
            &mut file_contacts,
        )
        .await?;
        wait_for_import_control().await?;
        let dns_elapsed = dns_start.elapsed();
        let status_counts = email_status_counts(&file_contacts);
        let contacts_found = file_contacts.len();
        let file_unique_addresses = file_contacts
            .iter()
            .map(|contact| contact.comparison_key.clone())
            .collect::<HashSet<_>>();
        unique_addresses.extend(file_unique_addresses.iter().cloned());
        let unresolved = status_counts.domain_unverified;
        let invalid_count = status_counts.invalid_syntax;
        let no_routing_count = status_counts.domain_missing_mx;
        let blacklisted_count = status_counts.blacklisted;
        let import_status = "completed";
        let valid_contacts = file_contacts
            .iter()
            .filter(|contact| {
                matches!(contact.status, EmailStatus::Valid)
                    && !blacklist.contains(&contact.comparison_key)
            })
            .count();
        let skipped = contacts_found.saturating_sub(valid_contacts);
        println!(
            "[import][file:dns] file=\"{}\" candidates={} unique_emails={} fresh_lookups={} valid={} no_routing={} unverified={} blacklisted={} invalid_syntax={} skipped_inactive={} dns_ms={:.2}",
            file_name,
            contacts_found,
            file_unique_addresses.len(),
            fresh_dns_results.len(),
            status_counts.valid,
            status_counts.domain_missing_mx,
            status_counts.domain_unverified,
            status_counts.blacklisted,
            status_counts.invalid_syntax,
            skipped,
            dns_elapsed.as_secs_f64() * 1000.0
        );
        let extra_category_for_db = extra_category.clone();
        let import_id = new_contact_id();
        let import_id_for_db = import_id.clone();
        let import_file_name = file_name.clone();
        let relative_path = file.relative_path.clone();
        let file_size = file.bytes.len();
        let parser_contacts = file_contacts;
        let parsed_rows = parsed.parsed_rows;
        let row_errors = parsed.row_errors;
        let rows_without_candidates = parsed.rows_without_candidates;
        let source_rows = parsed.source_rows;
        let encoding = parsed.encoding;
        let delimiter = parsed.delimiter;
        let sheet_count = parsed.sheet_count;
        let physical_rows = parsed.physical_rows;
        let successful_rows = parsed.successful_rows;
        let malformed_rows = parsed.malformed_rows;
        let malformed_row_count = malformed_rows;
        let failed_sheets = parsed.failed_sheets;
        let failed_sheet_count = failed_sheets.len();
        let warnings_count = parsed.warnings.len();
        if physical_rows != successful_rows.saturating_add(malformed_rows) {
            return Err(format!(
                "{file_name}: riconciliazione righe non valida ({physical_rows} fisiche != {successful_rows} lette + {malformed_rows} malformate)"
            ));
        }
        let blacklist_for_db = blacklist.clone();
        let failed_manifest_app = app.clone();
        let failed_manifest_file = file;
        let db_for_file = import_db
            .take()
            .ok_or_else(|| "Connessione import non disponibile".to_owned())?;
        let saved_result = tauri::async_runtime::spawn_blocking(move || {
            let mut db = db_for_file;
            let tx = db.transaction().map_err(|err| err.to_string())?;
            persist_domain_verification_results(&tx, &fresh_dns_results)?;
            let now = chrono::Utc::now().to_rfc3339();
            let attempt_id = new_contact_id();
            tx.execute(
                "INSERT INTO import_attempts(id,status,parser_version,started_at) VALUES (?1,'running','parser-v2',?2)",
                params![attempt_id, now],
            ).map_err(|err| err.to_string())?;
            tx.execute(
                "INSERT INTO imports(id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at,
                    import_attempt_id,content_sha256,hash_algorithm,file_size_bytes,parser_version,encoding,delimiter,sheet_count,started_at,completed_at,error_count,report_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?4,'sha256',?10,'parser-v2',?11,?12,?13,?8,?8,?14,?15)",
                params![import_id_for_db, import_file_name, relative_path, source_hash, import_status,
                    parsed_rows as i64, parser_contacts.len() as i64, now, attempt_id, file_size as i64,
                    encoding, delimiter, sheet_count as i64, row_errors.len() as i64,
                    serde_json::json!({"warnings":warnings_count,"rows_without_candidates":rows_without_candidates,"physical_rows":physical_rows,"successful_rows":successful_rows,"malformed_rows":malformed_rows,"failed_sheets":failed_sheets}).to_string()],
            ).map_err(|err| err.to_string())?;



            let row_error_messages = row_errors.iter().map(|error| {
                ((error.sheet.as_deref().unwrap_or("").to_owned(), error.source_row), error.message.as_str())
            }).collect::<HashMap<_, _>>();
            let mut insert_source_row = tx.prepare_cached(
                "INSERT OR IGNORE INTO source_rows(id,import_id,source_sheet,row_number,raw_context,parse_status,error_message,candidate_count,created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
            ).map_err(|err| err.to_string())?;
            for source in &source_rows {
                if IMPORT_CANCELLED.load(Ordering::SeqCst) {
                    return Err("Importazione annullata".to_owned());
                }
                if source.source_row == 0 { continue; }
                let row_id = format!("{}:{}:{}", import_id_for_db, source.sheet.as_deref().unwrap_or(""), source.source_row);
                let parse_status = match source.parse_status.as_str() {
                    "malformed" => "malformed",
                    "failed" => "failed",
                    "header" => "header",
                    _ => if source.candidate_count > 0 { "parsed" } else { "empty" },
                };
                let error_message = row_error_messages.get(&(
                    source.sheet.as_deref().unwrap_or("").to_owned(),
                    source.source_row,
                )).copied();
                insert_source_row.execute(
                    params![row_id, import_id_for_db, source.sheet.as_deref().unwrap_or(""), source.source_row as i64, source.raw_context, parse_status, source.error.as_deref().or(error_message), source.candidate_count as i64, now],
                ).map_err(|err| err.to_string())?;
            }
            drop(insert_source_row);
            let mut counts = PersistCounts::default();
            let mut updated_contact_ids = HashSet::new();
            let mut contact_cache = crate::contacts::ContactImportCache::default();
            crate::contacts::suspend_contact_triggers(&tx)?;
            let mut current_source_row = (String::new(), usize::MAX, String::new());
            let mut occurrence_indexes = HashMap::<String, usize>::new();
            let mut insert_candidate = tx.prepare_cached(
                "INSERT INTO email_candidates
                 (id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
                  extraction_method,contact_id,status,syntax_status,routing_status,mailbox_status,
                  candidate_status,current_disposition,quality_score,score_factors_json,first_seen_at,last_seen_at,
                  disposition_reason,category,subcategory,source_url,occurrence_index,outreach_eligible)
                 VALUES (?1,?2,?3,?4,?5,?6,'parser',?7,?8,?9,?10,'unknown',?8,?8,?11,?12,?13,?13,?14,?15,?16,?17,?18,?19)"
            ).map_err(|err| err.to_string())?;
            let mut insert_disposition = tx.prepare_cached(
                "INSERT INTO candidate_dispositions(id,candidate_id,disposition,reason_code,created_at)
                 VALUES (lower(hex(randomblob(16))),?1,?2,?3,?4)"
            ).map_err(|err| err.to_string())?;
            let score_factors_ok = r#"{"routing":0.9,"source":1}"#;
            let score_factors_bad = r#"{"routing":0.1,"source":1}"#;
            for contact in &parser_contacts {
                if IMPORT_CANCELLED.load(Ordering::SeqCst) {
                    return Err("Importazione annullata".to_owned());
                }
                let is_blacklisted = blacklist_for_db.contains(&contact.comparison_key);
                let (disposition, score, reason) = candidate_disposition(contact, is_blacklisted);
                let source_sheet = contact.source_sheet.as_deref().unwrap_or("");
                if current_source_row.0 != source_sheet || current_source_row.1 != contact.source_row {
                    current_source_row = (
                        source_sheet.to_owned(),
                        contact.source_row,
                        format!("{}:{}:{}", import_id_for_db, source_sheet, contact.source_row),
                    );
                    occurrence_indexes.clear();
                }
                let source_row_id = &current_source_row.2;
                let occurrence_index = occurrence_indexes.entry(contact.comparison_key.clone()).or_default();
                let candidate_occurrence_index = *occurrence_index;
                *occurrence_index += 1;
                let candidate_id = new_contact_id();
                let candidate_status = disposition;
                let score_factors = if matches!(contact.status, EmailStatus::Valid) {
                    score_factors_ok
                } else {
                    score_factors_bad
                };
                if disposition == "linked" {
                    let outcome = crate::contacts::save_contact_for_import(
                        &tx,
                        contact,
                        &extra_category_for_db,
                        Some(&import_id_for_db),
                        Some(source_row_id),
                        &now,
                        &mut contact_cache,
                    )?;
                    let contact_id = outcome.contact_id;
                    if outcome.is_new {
                        counts.new_contacts += 1;
                    } else if outcome.existing_before_import {
                        updated_contact_ids.insert(contact_id.clone());
                    }
                    insert_candidate.execute(
                        params![candidate_id, source_row_id, contact.raw_email, contact.normalized_email,
                            contact.comparison_key, contact.normalization_method, contact_id, candidate_status,
                            if matches!(contact.status, EmailStatus::InvalidSyntax) { "invalid_syntax" } else { "syntax_ok" },
                            if matches!(contact.status, EmailStatus::Valid) { "routing_ok" } else { contact.status.as_str() }, score,
                            score_factors,
                            now, reason, contact.category, contact.subcategory, contact.source_url,
                            candidate_occurrence_index as i64, 1i64],
                    ).map_err(|err| err.to_string())?;
                    crate::contacts::insert_classification_claim_deferred(&tx, &now, crate::contacts::ClassificationClaim { candidate_id: Some(&candidate_id), contact_id: Some(&contact_id), source_row_id: Some(source_row_id), category: &contact.category, subcategory: contact.subcategory.as_deref(), confidence: contact.classification_confidence, method: &contact.classification_method, evidence: &contact.classification_evidence })?;
                    if !extra_category_for_db.trim().is_empty() {
                        let extra_claim = crate::contacts::standard_category([extra_category_for_db.as_str()].into_iter());
                        if extra_claim != contact.category {
                            crate::contacts::insert_classification_claim_deferred(&tx, &now, crate::contacts::ClassificationClaim { candidate_id: Some(&candidate_id), contact_id: Some(&contact_id), source_row_id: Some(source_row_id), category: &extra_claim, subcategory: None, confidence: 0.65, method: "import_context", evidence: &["import-selected category".to_owned()] })?;
                        }
                    }
                    insert_disposition.execute(params![candidate_id, "linked", "routing_ok", now]).map_err(|err| err.to_string())?;
                    counts.active_occurrences += 1;
                } else {
                    insert_candidate.execute(
                        params![candidate_id, source_row_id, contact.raw_email, contact.normalized_email,
                            contact.comparison_key, contact.normalization_method, Option::<String>::None, candidate_status,
                            if matches!(contact.status, EmailStatus::InvalidSyntax) { "invalid_syntax" } else { "syntax_ok" },
                            if matches!(contact.status, EmailStatus::Valid) { "routing_ok" } else { contact.status.as_str() }, score,
                            score_factors,
                            now, reason, contact.category, contact.subcategory, contact.source_url,
                            candidate_occurrence_index as i64, 0i64],
                    ).map_err(|err| err.to_string())?;
                    if disposition == "suppressed"
                        && reason == "non_contact_mailbox"
                    {
                        crate::contacts::suppress_non_contact_mailbox(
                            &tx,
                            &contact.comparison_key,
                            &contact.normalized_email,
                            contact.source_file.as_ref(),
                            &now,
                        )?;
                    }
                    crate::contacts::insert_classification_claim_deferred(&tx, &now, crate::contacts::ClassificationClaim { candidate_id: Some(&candidate_id), contact_id: None, source_row_id: Some(source_row_id), category: &contact.category, subcategory: contact.subcategory.as_deref(), confidence: contact.classification_confidence, method: &contact.classification_method, evidence: &contact.classification_evidence })?;
                    if !extra_category_for_db.trim().is_empty() {
                        let extra_claim = crate::contacts::standard_category([extra_category_for_db.as_str()].into_iter());
                        if extra_claim != contact.category {
                            crate::contacts::insert_classification_claim_deferred(&tx, &now, crate::contacts::ClassificationClaim { candidate_id: Some(&candidate_id), contact_id: None, source_row_id: Some(source_row_id), category: &extra_claim, subcategory: None, confidence: 0.65, method: "import_context", evidence: &["import-selected category".to_owned()] })?;
                        }
                    }
                    insert_disposition.execute(params![candidate_id, disposition, reason, now]).map_err(|err| err.to_string())?;
                }
            }
            drop(insert_candidate);
            drop(insert_disposition);
            counts.source_occurrences_added =
                crate::contacts::flush_contact_import_cache(&tx, &mut contact_cache, &now)?;
            counts.existing_contacts_updated = updated_contact_ids.len();
            crate::contacts::recompute_imported_classifications(&tx, &import_id_for_db)?;
            crate::contacts::resume_contact_triggers(&tx)?;
            let persisted_rows: usize = tx.query_row(
                "SELECT COUNT(*) FROM source_rows WHERE import_id=?1",
                params![import_id_for_db], |row| Ok(row.get::<_, i64>(0)? as usize),
            ).map_err(|err| err.to_string())?;
            let persisted_candidates: usize = tx.query_row(
                "SELECT COUNT(*) FROM email_candidates ec JOIN source_rows sr ON sr.id=ec.source_row_id WHERE sr.import_id=?1",
                params![import_id_for_db], |row| Ok(row.get::<_, i64>(0)? as usize),
            ).map_err(|err| err.to_string())?;
            let undisposed_candidates: usize = tx.query_row(
                "SELECT COUNT(*) FROM email_candidates ec
                 JOIN source_rows sr ON sr.id=ec.source_row_id
                 WHERE sr.import_id=?1 AND NOT EXISTS (
                    SELECT 1 FROM candidate_dispositions cd
                    WHERE cd.candidate_id=ec.id AND cd.is_current=1
                 )",
                params![import_id_for_db], |row| Ok(row.get::<_, i64>(0)? as usize),
            ).map_err(|err| err.to_string())?;
            if persisted_rows != source_rows.len()
                || persisted_candidates != parser_contacts.len()
                || undisposed_candidates != 0
            {
                return Err(format!(
                    "Riconciliazione import non valida: righe {persisted_rows}/{}, candidati {persisted_candidates}/{}, senza disposizione {undisposed_candidates}",
                    source_rows.len(), parser_contacts.len()
                ));
            }
            let unique_count = parser_contacts.iter()
                .map(|contact| contact.comparison_key.as_str())
                .collect::<HashSet<_>>().len();
            let reconciliation = serde_json::json!({
                "physical_rows": physical_rows,
                "successfully_read_rows": successful_rows,
                "malformed_rows": malformed_rows,
                "failed_sheets": failed_sheets,
                "rows_without_address": rows_without_candidates,
                "candidate_occurrences": parser_contacts.len(),
                "unique_normalized_addresses": unique_count,
                "new_canonical_contacts": counts.new_contacts,
                "existing_contacts_updated": counts.existing_contacts_updated,
                "source_occurrences_added": counts.source_occurrences_added,
                "syntax_invalid_candidates": invalid_count,
                "no_routing_candidates": no_routing_count,
                "temporary_dns_failures": unresolved,
                "suppressed_blacklisted_candidates": blacklisted_count,
            }).to_string();
            tx.execute(
                "UPDATE imports SET report_json=?1 WHERE id=?2",
                params![reconciliation, import_id_for_db],
            ).map_err(|err| err.to_string())?;
            tx.execute(
                "UPDATE import_attempts SET status=?1,completed_at=?2,report_json=?3 WHERE id=?4",
                params![import_status, now, reconciliation, attempt_id],
            ).map_err(|err| err.to_string())?;
            if IMPORT_CANCELLED.load(Ordering::SeqCst) {
                return Err("Importazione annullata".to_owned());
            }
            tx.commit().map_err(|err| err.to_string())?;
            Ok::<(crate::database::PooledConnection, PersistCounts), String>((db, counts))
        })
        .await;
        let saved = match saved_result {
            Ok(Ok((db, saved))) => {
                import_db = Some(db);
                saved
            }
            Ok(Err(error)) => {
                if error == "Importazione annullata" {
                    return Err(error);
                }
                let message = format!("salvataggio transazionale non riuscito: {error}");
                let file_for_manifest = failed_manifest_file.clone();
                let message_for_manifest = message.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    persist_failed_file_manifest(
                        &failed_manifest_app,
                        &file_for_manifest,
                        &message_for_manifest,
                    )
                })
                .await
                .map_err(|join_error| {
                    format!("{file_name}: registrazione errore interrotta: {join_error}")
                })??;
                return Err(format!("{file_name}: {message}"));
            }
            Err(join_error) => {
                let message = format!("salvataggio interrotto: {join_error}");
                let message_for_manifest = message.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    persist_failed_file_manifest(
                        &failed_manifest_app,
                        &failed_manifest_file,
                        &message_for_manifest,
                    )
                })
                .await
                .map_err(|manifest_join_error| {
                    format!("{file_name}: registrazione errore interrotta: {manifest_join_error}")
                })??;
                return Err(format!("{file_name}: {message}"));
            }
        };
        if import_status == "completed" {
            completed_imports.insert(completed_import_key);
        }
        imported += saved.active_occurrences;
        completed_import_ids.push(import_id);
        candidate_occurrences += contacts_found;
        new_canonical_contacts += saved.new_contacts;
        existing_contacts_updated += saved.existing_contacts_updated;
        source_occurrences_added += saved.source_occurrences_added;
        syntax_invalid_candidates += invalid_count;
        no_routing_candidates += no_routing_count;
        temporary_dns_failures += unresolved;
        suppressed_blacklisted_candidates += blacklisted_count;
        rows_without_address += rows_without_candidates;
        malformed_rows_total += malformed_row_count;
        failed_sheets_total += failed_sheet_count;
        let file_total_elapsed = file_start.elapsed();
        println!(
            "[import][file:persisted] file=\"{}\" active_saved={} new_contacts={} updated_contacts={} source_occurrences={} total_imported_so_far={} file_total_ms={:.2}",
            file_name,
            saved.active_occurrences,
            saved.new_contacts,
            saved.existing_contacts_updated,
            saved.source_occurrences_added,
            imported,
            file_total_elapsed.as_secs_f64() * 1000.0
        );
        let mut file_warnings = parsed.warnings;

        if invalid_count > 0 {
            file_warnings.push(format!(
                "{} email separate dai contatti attivi perché sintatticamente non valide; conservate nei candidati.",
                invalid_count
            ));
        }
        if no_routing_count > 0 {
            file_warnings.push(format!(
                "{} email separate dai contatti attivi perché il dominio non accetta posta (nessun MX/A); conservate nei candidati.",
                no_routing_count
            ));
        }
        if blacklisted_count > 0 {
            file_warnings.push(format!(
                "{} email soppresse perché presenti in blacklist/cancellami; evidenza conservata nei candidati.",
                blacklisted_count
            ));
        }
        if unresolved > 0 {
            file_warnings.push(format!(
                "{unresolved} email con dominio non verificabile (DNS): conservate nei candidati; «Rivalida solo DNS» in Verifica caselle le recupera quando il dominio risponde."
            ));
        }
        reports.push(FileReport {
            name: file_name.clone(),
            parsed_rows: parsed.parsed_rows,
            contacts_found,
            unique_normalized_addresses: file_unique_addresses.len(),
            new_canonical_contacts: saved.new_contacts,
            existing_contacts_updated: saved.existing_contacts_updated,
            source_occurrences_added: saved.source_occurrences_added,
            syntax_invalid_candidates: invalid_count,
            no_routing_candidates: no_routing_count,
            temporary_dns_failures: unresolved,
            suppressed_blacklisted_candidates: blacklisted_count,
            rows_without_address: rows_without_candidates,
            malformed_rows: malformed_row_count,
            failed_sheets: failed_sheet_count,
            warnings: file_warnings,
        });
        let _ = app.emit(
            "import-progress",
            ImportProgress {
                phase: "saved".to_owned(),
                file: Some(file_name),
                processed_files: processed_index + 1,
                total_files,
                imported,
            },
        );
    }
    let db_for_derivation = import_db
        .take()
        .ok_or_else(|| "Connessione import non disponibile".to_owned())?;
    let deriv_start = std::time::Instant::now();
    println!(
        "[import][derivation:start] touched_imports={}",
        completed_import_ids.len()
    );
    tauri::async_runtime::spawn_blocking(move || {
        if completed_import_ids.is_empty() {
            return Ok::<(), String>(());
        }
        let mut db = db_for_derivation;
        let tx = db.transaction().map_err(|error| error.to_string())?;
        derive_imported_contacts(&tx, &completed_import_ids)?;
        tx.commit().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("Derivazione post-import interrotta: {error}"))??;
    let total_elapsed = overall_start.elapsed();
    println!(
        "[import][derivation:complete] duration_ms={:.2}",
        deriv_start.elapsed().as_secs_f64() * 1000.0
    );
    println!(
        "[import][summary] total_time_ms={:.2} files_processed={}/{} total_active_imported={} new_canonical={} existing_updated={} source_occurrences={} candidates_total={} unique_emails={} invalid_syntax={} no_routing={} temporary_dns_failures={} blacklisted={}",
        total_elapsed.as_secs_f64() * 1000.0,
        total_files,
        total_files,
        imported,
        new_canonical_contacts,
        existing_contacts_updated,
        source_occurrences_added,
        candidate_occurrences,
        unique_addresses.len(),
        syntax_invalid_candidates,
        no_routing_candidates,
        temporary_dns_failures,
        suppressed_blacklisted_candidates
    );

    Ok(ImportReport {
        imported,
        candidate_occurrences,
        unique_normalized_addresses: unique_addresses.len(),
        new_canonical_contacts,
        existing_contacts_updated,
        source_occurrences_added,
        syntax_invalid_candidates,
        no_routing_candidates,
        temporary_dns_failures,
        suppressed_blacklisted_candidates,
        rows_without_address,
        malformed_rows: malformed_rows_total,
        failed_sheets: failed_sheets_total,
        files: reports,
    })
}

#[tauri::command]
pub(crate) async fn list_contacts(
    app: tauri::AppHandle,
    filters: ListFilters,
    limit: usize,
    offset: Option<usize>,
    include_total: Option<bool>,
) -> Result<ContactList, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        load_contacts_page(
            &db,
            &filters,
            limit,
            offset.unwrap_or(0),
            include_total.unwrap_or(true),
        )
    })
    .await
    .map_err(|error| format!("Caricamento contatti interrotto: {error}"))?
}

/// Post-import derivation for the given imports: source counts, PEC flags, DNS
/// and suppression evidence of the touched contacts. Their quality score follows
/// from the `contacts` score triggers as these statements update them.
pub(crate) fn derive_imported_contacts(
    tx: &Connection,
    import_ids: &[String],
) -> Result<(), String> {
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS current_import_ids(id TEXT PRIMARY KEY);
         DELETE FROM current_import_ids;",
    )
    .map_err(|error| error.to_string())?;
    {
        let mut insert = tx
            .prepare_cached("INSERT OR IGNORE INTO current_import_ids(id) VALUES (?1)")
            .map_err(|error| error.to_string())?;
        for import_id in import_ids {
            insert
                .execute(params![import_id])
                .map_err(|error| error.to_string())?;
        }
    }
    tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS touched_contacts(contact_id TEXT PRIMARY KEY);
             DELETE FROM touched_contacts;
             INSERT INTO touched_contacts(contact_id)
                 SELECT DISTINCT cs.contact_id FROM contact_sources cs
                 JOIN current_import_ids ci ON ci.id=cs.import_id;

             UPDATE contacts SET
                   source_occurrence_count=(SELECT COUNT(*) FROM contact_sources WHERE contact_id=contacts.id),
                   independent_source_count=(SELECT COUNT(DISTINCT COALESCE(i.relative_path || char(31) || COALESCE(i.content_sha256,i.file_hash,''), cs.source_file)) FROM contact_sources cs LEFT JOIN imports i ON i.id=cs.import_id WHERE cs.contact_id=contacts.id)
             WHERE id IN (SELECT contact_id FROM touched_contacts);

             UPDATE contacts SET is_pec = 1
             WHERE id IN (SELECT contact_id FROM touched_contacts)
               AND is_pec = 0
               AND (
                 lower(email) LIKE '%@pec.%'
                 OR lower(email) LIKE '%.pec.%'
                 OR lower(email) LIKE '%legalmail%'
                 OR lower(email) LIKE '%postacert%'
                 OR lower(email) LIKE '%cert.%'
                 OR EXISTS (
                   SELECT 1 FROM domain_verification_cache d
                   JOIN json_each(d.mx_hosts_json) mx
                   WHERE d.domain = lower(substr(contacts.email, instr(contacts.email, '@') + 1))
                     AND (
                       lower(mx.value) LIKE '%.pec.%'
                       OR lower(mx.value) LIKE 'pec.%'
                       OR lower(mx.value) LIKE '%postacert%'
                       OR lower(mx.value) LIKE '%legalmail%'
                       OR lower(mx.value) LIKE '%cert.%'
                       OR lower(mx.value) LIKE '%sicurezzapostale.it%'
                       OR lower(mx.value) LIKE '%postecert.it%'
                       OR lower(mx.value) LIKE '%telecompost.it%'
                       OR lower(mx.value) LIKE '%telecompec.it%'
                       OR lower(mx.value) LIKE '%pec-email.com%'
                       OR lower(mx.value) LIKE '%asmecal.it%'
                       OR lower(mx.value) LIKE '%asmecam.it%'
                       OR lower(mx.value) LIKE '%asmez.it%'
                       OR lower(mx.value) LIKE '%gespec.it%'
                       OR lower(mx.value) LIKE '%twtcert.it%'
                       OR lower(mx.value) LIKE '%mypec.eu%'
                       OR lower(mx.value) LIKE '%asmepec.it%'
                     )
                 )
               );

             INSERT INTO verification_events(
                   id,comparison_key,candidate_id,contact_id,method,provider,routing_status,
                   ttl_seconds,observed_ttl_seconds,effective_cache_ttl_seconds,mx_hosts_json,
                   dns_rcode,dns_checked_at,dns_expires_at,mailbox_status,error_class,details_json,checked_at)
                 SELECT lower(hex(randomblob(16))), ec.comparison_key, ec.id, ec.contact_id,
                   'suppression',
                   CASE WHEN ec.routing_status IN ('routing_ok','no_routing','temporary_failure') THEN 'hickory' END,
                   CASE ec.routing_status WHEN 'routing_ok' THEN 'routing_ok' WHEN 'no_routing' THEN 'no_routing' ELSE 'temporary_failure' END,
                   d.effective_cache_ttl_seconds, d.observed_ttl_seconds, d.effective_cache_ttl_seconds,
                   coalesce(d.mx_hosts_json,'[]'), d.dns_rcode, d.checked_at, d.expires_at, 'unknown',
                   CASE ec.routing_status WHEN 'no_routing' THEN 'definitive_dns' WHEN 'temporary_failure' THEN 'temporary_dns' WHEN 'not_checked' THEN 'invalid_syntax' END,
                   json_object('source','post_import_derivation','routing_status',ec.routing_status), coalesce(d.checked_at,CURRENT_TIMESTAMP)
                 FROM email_candidates ec
                 JOIN source_rows sr ON sr.id=ec.source_row_id
                 JOIN current_import_ids ci ON ci.id=sr.import_id
                 LEFT JOIN domain_verification_cache d ON d.domain=lower(substr(ec.canonical_email,instr(ec.canonical_email,'@')+1))
                 WHERE ec.routing_status<>'routing_ok'
                   AND NOT EXISTS (SELECT 1 FROM verification_events ve WHERE ve.candidate_id=ec.id AND ve.method='suppression');"
    )
    .map_err(|error| error.to_string())
}

/// Spreadsheets run cells that start with these characters as formulas. Imported
/// text comes from third parties, so descriptive CSV columns are neutralized with a
/// leading apostrophe. Email columns stay untouched for mailing tools.
pub(crate) fn spreadsheet_safe(value: String) -> String {
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value
    }
}

pub(crate) fn publish_export<F>(target: &std::path::Path, write_rows: F) -> Result<usize, String>
where
    F: FnOnce(&mut BufWriter<std::fs::File>) -> Result<usize, String>,
{
    if target.exists() {
        return Err("Il file selezionato esiste già; scegliere un nuovo nome".to_owned());
    }
    let parent = target
        .parent()
        .ok_or_else(|| "Percorso di esportazione non valido".to_owned())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "Nome file di esportazione non valido".to_owned())?;
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(file);
        let count = write_rows(&mut writer)?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_all().map_err(|e| e.to_string())?;
        drop(writer);

        // A hard link publishes without ever replacing an existing file. exFAT/FAT
        // drives and some network shares have no hard links: fall back to a rename
        // after re-checking the target.
        // ponytail: rename fallback can race a file created in the same instant.
        if std::fs::hard_link(&temporary, target).is_ok() {
            let _ = std::fs::remove_file(&temporary);
        } else if target.exists() {
            return Err("Il file selezionato esiste già; scegliere un nuovo nome".to_owned());
        } else {
            std::fs::rename(&temporary, target).map_err(|e| e.to_string())?;
        }
        Ok(count)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
pub(crate) fn export_contacts_to_path(
    db: &Connection,
    filters: &ListFilters,
    target: &std::path::Path,
) -> Result<usize, String> {
    export_contacts_to_path_with_options(db, filters, target, true)
}

pub(crate) fn export_contacts_to_path_with_options(
    db: &Connection,
    filters: &ListFilters,
    target: &std::path::Path,
    include_untrusted_classifications: bool,
) -> Result<usize, String> {
    publish_export(target, |output| {
        let mut csv = csv::WriterBuilder::new()
            .delimiter(b';')
            .from_writer(output);
        csv.write_record([
            "email",
            "file_origine",
            "categoria",
            "sottocategoria",
            "tipo_email",
            "tipo_contatto",
            "macroarea",
            "regione",
            "provincia",
            "comune",
            "paese",
            "url_sorgente",
            "metodo_classificazione",
        ])
        .map_err(|e| e.to_string())?;
        let (mut where_sql, values) = filter_where(filters);
        if !include_untrusted_classifications {
            where_sql.push_str(
                " AND contacts.classification_state NOT IN \
                 ('conflicting','taxonomy_gap')",
            );
        }
        let mut stmt = db
            .prepare(&format!(
                "SELECT email,source_file,category,subcategory,
                        CASE WHEN is_pec=1 THEN 'pec' ELSE 'standard' END,
                        contact_kind,macroarea,regione,provincia,comune,paese,
                        source_url,classification_method
                 FROM contacts {where_sql} ORDER BY lower(email),id"
            ))
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(values.iter()))
            .map_err(|e| e.to_string())?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let record = (0..13)
                .map(|index| {
                    row.get::<_, Option<String>>(index).map(|value| {
                        let value = value.unwrap_or_default();
                        if index == 0 {
                            value
                        } else {
                            spreadsheet_safe(value)
                        }
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            csv.write_record(record).map_err(|e| e.to_string())?;
            count += 1;
        }
        csv.flush().map_err(|e| e.to_string())?;
        Ok(count)
    })
}

#[cfg(test)]
pub(crate) fn export_contact_emails_to_path(
    db: &Connection,
    filters: &ListFilters,
    target: &std::path::Path,
) -> Result<usize, String> {
    export_contact_emails_to_path_with_options(db, filters, target, true)
}

pub(crate) fn export_contact_emails_to_path_with_options(
    db: &Connection,
    filters: &ListFilters,
    target: &std::path::Path,
    include_untrusted_classifications: bool,
) -> Result<usize, String> {
    publish_export(target, |output| {
        let mut csv = csv::WriterBuilder::new()
            .has_headers(false)
            .from_writer(output);
        let (mut where_sql, values) = filter_where(filters);
        if !include_untrusted_classifications {
            where_sql.push_str(
                " AND contacts.classification_state NOT IN \
                 ('conflicting','taxonomy_gap')",
            );
        }
        let mut stmt = db
            .prepare(&format!(
                "SELECT email FROM contacts {where_sql} ORDER BY lower(email),id"
            ))
            .map_err(|e| e.to_string())?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(values.iter()))
            .map_err(|e| e.to_string())?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            csv.write_record([row.get::<_, String>(0).map_err(|e| e.to_string())?])
                .map_err(|e| e.to_string())?;
            count += 1;
        }
        csv.flush().map_err(|e| e.to_string())?;
        Ok(count)
    })
}

#[tauri::command]
pub(crate) async fn export_contacts_csv(
    app: tauri::AppHandle,
    filters: ListFilters,
    path: String,
    include_untrusted_classifications: bool,
) -> Result<ExportReport, String> {
    crate::database::blocking(move || {
        let target = PathBuf::from(path);
        let db = open_db(&app)?;
        let rows_written = export_contacts_to_path_with_options(
            &db,
            &filters,
            &target,
            include_untrusted_classifications,
        )?;
        Ok(ExportReport {
            path: target.display().to_string(),
            rows_written,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn export_contact_emails_csv(
    app: tauri::AppHandle,
    filters: ListFilters,
    path: String,
    include_untrusted_classifications: bool,
) -> Result<ExportReport, String> {
    crate::database::blocking(move || {
        let target = PathBuf::from(path);
        let db = open_db(&app)?;
        let rows_written = export_contact_emails_to_path_with_options(
            &db,
            &filters,
            &target,
            include_untrusted_classifications,
        )?;
        Ok(ExportReport {
            path: target.display().to_string(),
            rows_written,
        })
    })
    .await
}

pub(crate) fn export_blacklist_to_path(
    db: &Connection,
    query: &str,
    target: &std::path::Path,
) -> Result<usize, String> {
    publish_export(target, |output| {
        output
            .write_all(b"\xEF\xBB\xBF")
            .map_err(|e| e.to_string())?;
        let mut csv = csv::WriterBuilder::new()
            .delimiter(b';')
            .from_writer(output);
        csv.write_record(["email", "file_origine", "data_importazione"])
            .map_err(|e| e.to_string())?;
        let pattern = format!("%{}%", query.trim());
        let mut stmt = db
            .prepare(
                "SELECT normalized_email,source_file,imported_at FROM blacklist
                 WHERE normalized_email LIKE ?1 COLLATE NOCASE OR source_file LIKE ?1 COLLATE NOCASE
                 ORDER BY lower(normalized_email),normalized_email",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query(params![pattern]).map_err(|e| e.to_string())?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            csv.write_record([
                row.get::<_, String>(0).map_err(|e| e.to_string())?,
                spreadsheet_safe(row.get::<_, String>(1).map_err(|e| e.to_string())?),
                spreadsheet_safe(row.get::<_, String>(2).map_err(|e| e.to_string())?),
            ])
            .map_err(|e| e.to_string())?;
            count += 1;
        }
        csv.flush().map_err(|e| e.to_string())?;
        Ok(count)
    })
}

pub(crate) fn export_blacklist_emails_to_path(
    db: &Connection,
    query: &str,
    target: &std::path::Path,
) -> Result<usize, String> {
    publish_export(target, |output| {
        let mut csv = csv::WriterBuilder::new()
            .has_headers(false)
            .from_writer(output);
        let pattern = format!("%{}%", query.trim());
        let mut stmt = db
            .prepare(
                "SELECT normalized_email FROM blacklist
                 WHERE normalized_email LIKE ?1 COLLATE NOCASE OR source_file LIKE ?1 COLLATE NOCASE
                 ORDER BY lower(normalized_email),normalized_email",
            )
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query(params![pattern]).map_err(|e| e.to_string())?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            csv.write_record([row.get::<_, String>(0).map_err(|e| e.to_string())?])
                .map_err(|e| e.to_string())?;
            count += 1;
        }
        csv.flush().map_err(|e| e.to_string())?;
        Ok(count)
    })
}

#[tauri::command]
pub(crate) async fn export_blacklist_csv(
    app: tauri::AppHandle,
    query: Option<String>,
    path: String,
) -> Result<ExportReport, String> {
    crate::database::blocking(move || {
        let target = PathBuf::from(path);
        let db = open_db(&app)?;
        let rows_written = export_blacklist_to_path(&db, &query.unwrap_or_default(), &target)?;
        Ok(ExportReport {
            path: target.display().to_string(),
            rows_written,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn export_blacklist_emails_csv(
    app: tauri::AppHandle,
    query: Option<String>,
    path: String,
) -> Result<ExportReport, String> {
    crate::database::blocking(move || {
        let target = PathBuf::from(path);
        let db = open_db(&app)?;
        let rows_written =
            export_blacklist_emails_to_path(&db, &query.unwrap_or_default(), &target)?;
        Ok(ExportReport {
            path: target.display().to_string(),
            rows_written,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_filter_options(app: tauri::AppHandle) -> Result<FilterOptions, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        load_filter_options(&db)
    })
    .await
}

pub(crate) fn load_filter_options(db: &Connection) -> Result<FilterOptions, String> {
    #[cfg(test)]
    FACET_LOAD_COUNT.fetch_add(1, Ordering::SeqCst);
    crate::contacts::facet_options(db)
}

#[tauri::command]
pub(crate) async fn refresh_dashboard(
    app: tauri::AppHandle,
    filters: ListFilters,
    limit: usize,
    offset: Option<usize>,
    include_facets: Option<bool>,
) -> Result<DashboardData, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        let filtered = load_contacts(&db, &filters, limit, offset.unwrap_or(0))?;
        let database_total = db
            .query_row(
                "SELECT COUNT(*) FROM contacts WHERE status='valid' AND coalesce(suppression_status,'active')='active'",
                [],
                |row| Ok(row.get::<_, i64>(0)? as usize),
            )
            .map_err(|err| err.to_string())?;
        let filter_options = if include_facets.unwrap_or(true) {
            load_filter_options(&db)?
        } else {
            FilterOptions::default()
        };
        let pending_review_count = count_classification_reviews(&db, "pending")?;
        let blacklist_count = crate::blacklist::load_blacklist_stats(&db)?;
        Ok(DashboardData {
            filtered,
            database_total,
            filters: filter_options,
            blacklist_count,
            pending_review_count,
        })
    })
    .await
    .map_err(|error| format!("Aggiornamento dashboard interrotto: {error}"))?
}

#[tauri::command]
pub(crate) async fn delete_contact(app: tauri::AppHandle, id: String) -> Result<(), String> {
    crate::database::blocking(move || {
        let delete_start = std::time::Instant::now();
        let mut db = open_db(&app)?;
        let tx = db.transaction().map_err(|e| e.to_string())?;
        let candidate_ids = {
            let mut statement = tx
                .prepare("SELECT id FROM email_candidates WHERE contact_id=?1")
                .map_err(|error| error.to_string())?;

            statement
                .query_map(params![id], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        let now = chrono::Utc::now().to_rfc3339();
        for candidate_id in &candidate_ids {
            tx.execute(
                "UPDATE candidate_dispositions SET is_current=0
             WHERE candidate_id=?1 AND is_current=1",
                params![candidate_id],
            )
            .map_err(|error| error.to_string())?;
            tx.execute(
                "INSERT INTO candidate_dispositions(
                id,candidate_id,disposition,reason_code,is_current,created_at
             ) VALUES (?1,?2,'quarantined','manual_contact_removal',1,?3)",
                params![new_contact_id(), candidate_id, now],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.execute("UPDATE email_candidates SET contact_id=NULL,status='quarantined',current_disposition='quarantined',candidate_status='quarantined',outreach_eligible=0,disposition_reason='manual_contact_removal' WHERE contact_id=?1", params![id])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM contacts WHERE id = ?1", params![id])
            .map_err(|err| err.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        println!(
            "[delete][contact] id=\"{}\" candidate_records_quarantined={} duration_ms={:.2}",
            id,
            candidate_ids.len(),
            delete_start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn delete_all_invalid(app: tauri::AppHandle) -> Result<usize, String> {
    crate::database::blocking(move || {
        let delete_start = std::time::Instant::now();
        let mut db = open_db(&app)?;
        let tx = db.transaction().map_err(|error| error.to_string())?;

        let ids = {
            let mut statement = tx
                .prepare(
                    "SELECT id FROM contacts c
                 WHERE c.status IN ('invalid_syntax','domain_missing_mx','blacklisted')
                   AND EXISTS (SELECT 1 FROM email_candidates ec WHERE ec.contact_id=c.id)",
                )
                .map_err(|error| error.to_string())?;

            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mut total_quarantined = 0usize;
        for contact_id in &ids {
            let candidate_ids = {
                let mut statement = tx
                    .prepare("SELECT id FROM email_candidates WHERE contact_id=?1")
                    .map_err(|error| error.to_string())?;

                statement
                    .query_map(params![contact_id], |row| row.get::<_, String>(0))
                    .map_err(|error| error.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?
            };
            total_quarantined += candidate_ids.len();
            for candidate_id in candidate_ids {
                tx.execute(
                    "UPDATE candidate_dispositions SET is_current=0
                 WHERE candidate_id=?1 AND is_current=1",
                    params![candidate_id],
                )
                .map_err(|error| error.to_string())?;
                tx.execute(
                    "INSERT INTO candidate_dispositions(
                    id,candidate_id,disposition,reason_code,is_current,created_at
                 ) VALUES (?1,?2,'quarantined','legacy_invalid_cleanup',1,?3)",
                    params![new_contact_id(), candidate_id, now],
                )
                .map_err(|error| error.to_string())?;
            }
            tx.execute(
                "UPDATE email_candidates
             SET contact_id=NULL,status='quarantined',candidate_status='quarantined',
                 current_disposition='quarantined',outreach_eligible=0,
                 disposition_reason='legacy_invalid_cleanup'
             WHERE contact_id=?1",
                params![contact_id],
            )
            .map_err(|error| error.to_string())?;
            tx.execute("DELETE FROM contacts WHERE id=?1", params![contact_id])
                .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
        let deleted = ids.len();
        println!(
            "[delete][invalid_contacts] deleted_contacts={} candidate_records_quarantined={} duration_ms={:.2}",
            deleted,
            total_quarantined,
            delete_start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(deleted)
    })
    .await
}

#[tauri::command]
pub(crate) async fn clear_database(app: tauri::AppHandle) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || clear_database_inner(&app))
        .await
        .map_err(|error| format!("Pulizia database interrotta: {error}"))?
}

pub(crate) fn clear_database_inner(app: &tauri::AppHandle) -> Result<usize, String> {
    let mut db = open_db(app)?;
    crate::mailbox_verification::cancel_jobs_for_database_clear(&db)?;
    clear_database_connection(&mut db)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn clear_database_connection(db: &mut Connection) -> Result<usize, String> {
    let clear_start = std::time::Instant::now();
    println!("[delete][database:start] purging all contact and import tables...");

    db.pragma_update(None, "foreign_keys", false)
        .map_err(|err| err.to_string())?;
    let clear_result = (|| {
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|err| err.to_string())?;
        let deleted = tx
            .query_row("SELECT COUNT(*) FROM contacts", [], |row| {
                Ok(row.get::<_, i64>(0)? as usize)
            })
            .unwrap_or(0);
        tx.execute_batch(
            "DELETE FROM mailbox_verification_attempts;
             DELETE FROM mailbox_verification_tasks;
             DELETE FROM mailbox_verification_jobs;
             DELETE FROM mailbox_verification_target_state;
             UPDATE mailbox_verification_control SET action='cancelled',updated_at=CURRENT_TIMESTAMP WHERE id=1;
             DELETE FROM quality_score_components;
             DELETE FROM quality_score_snapshots;
             DELETE FROM candidate_dispositions;
             DELETE FROM classification_claims;
             DELETE FROM entity_contact_points;
             DELETE FROM verification_events;
             DELETE FROM email_candidates;
             DELETE FROM source_rows;
             DELETE FROM ai_field_changes;
             DELETE FROM ai_run_batches;
             DELETE FROM ai_run_retries;
             DELETE FROM ai_run_candidates;
             DELETE FROM classification_follow_up_queue;
             DELETE FROM classification_system_changes;
             DELETE FROM deterministic_recheck_queue;
             DELETE FROM deterministic_rule_attributions;
             DELETE FROM taxonomy_change_candidate_contacts;
             UPDATE taxonomy_change_candidates SET origin_contact_id=NULL,origin_run_id=NULL;
             DELETE FROM ai_run_artifacts;
             DELETE FROM ai_applied_provenance;
             DELETE FROM classification_reviews;
             DELETE FROM contact_sources;
             DELETE FROM ai_classification_runs;
             DELETE FROM imports;
             DELETE FROM contacts;
             DELETE FROM organizations;
             DELETE FROM import_attempts;
             DELETE FROM domain_verification_cache;
             DELETE FROM suppressions WHERE reason<>'blacklist';",
        )
        .map_err(|err| err.to_string())?;
        // Checked before commit: a table missing from the list above rolls the
        // whole clear back instead of leaving orphans behind.
        let violations: i64 = tx
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|err| err.to_string())?;
        if violations != 0 {
            return Err(format!(
                "Pulizia database non valida: {violations} riferimenti orfani"
            ));
        }
        tx.commit().map_err(|err| err.to_string())?;
        Ok::<usize, String>(deleted)
    })();
    db.pragma_update(None, "foreign_keys", true)
        .map_err(|err| err.to_string())?;
    let deleted = clear_result?;
    let _ = db.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    println!(
        "[delete][database:complete] contacts_cleared={} wal_checkpoint=passive duration_ms={:.2}",
        deleted,
        clear_start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(deleted)
}
