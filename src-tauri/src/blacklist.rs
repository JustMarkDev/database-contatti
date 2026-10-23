use crate::parsing::{canonicalize_email, is_valid_email_syntax};
use crate::{
    BlacklistEntry, BlacklistImportReport, BlacklistList, ImportFile, MAX_PAGE_SIZE, display_name,
    has_table, new_contact_id, open_db, parse_file,
};
use rusqlite::{Connection, TransactionBehavior, params};
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

pub(crate) fn suppress_blacklisted_email(
    db: &Connection,
    comparison_key: &str,
    source_file: &str,
    created_at: &str,
) -> Result<usize, String> {
    let key = comparison_key.trim().to_lowercase();
    db.execute(
        "INSERT INTO suppressions(
            comparison_key,canonical_email,reason,source_file,source_authority,created_at,lifted_at
         ) VALUES (?1,?1,'blacklist',?2,'user_blacklist',?3,NULL)
         ON CONFLICT(comparison_key) DO UPDATE SET
            canonical_email=excluded.canonical_email,
            reason='blacklist',
            source_file=excluded.source_file,
            source_authority='user_blacklist',
            lifted_at=NULL
         WHERE suppressions.reason<>'non_contact_mailbox' OR suppressions.lifted_at IS NOT NULL",
        params![key, source_file, created_at],
    )
    .map_err(|err| err.to_string())?;

    let candidate_ids = {
        let mut statement = db
            .prepare(
                "SELECT id FROM email_candidates
                 WHERE (comparison_key=?1 COLLATE NOCASE OR canonical_email=?1 COLLATE NOCASE)
                   AND coalesce(disposition_reason,'')<>'non_contact_mailbox'",
            )
            .map_err(|err| err.to_string())?;

        statement
            .query_map(params![key], |row| row.get::<_, String>(0))
            .map_err(|err| err.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?
    };
    for candidate_id in &candidate_ids {
        db.execute(
            "UPDATE candidate_dispositions SET is_current=0
             WHERE candidate_id=?1 AND is_current=1",
            params![candidate_id],
        )
        .map_err(|err| err.to_string())?;
        db.execute(
            "INSERT INTO candidate_dispositions(
                id,candidate_id,disposition,reason_code,details_json,is_current,created_at
             ) VALUES (?1,?2,'suppressed','blacklist','{}',1,?3)",
            params![new_contact_id(), candidate_id, created_at],
        )
        .map_err(|err| err.to_string())?;
    }
    db.execute(
        r#"UPDATE email_candidates
         SET status='suppressed',candidate_status='suppressed',current_disposition='suppressed',
             disposition_reason='blacklist',contact_id=NULL,outreach_eligible=0,quality_score=0,
             score_factors_json='{"suppression":"blacklist"}',last_seen_at=?2
         WHERE (comparison_key=?1 COLLATE NOCASE OR canonical_email=?1 COLLATE NOCASE)
           AND coalesce(disposition_reason,'')<>'non_contact_mailbox'"#,
        params![key, created_at],
    )
    .map_err(|err| err.to_string())?;

    db.execute(
        "DELETE FROM contacts
         WHERE email_comparison_key=?1 COLLATE NOCASE OR email=?1 COLLATE NOCASE",
        params![key],
    )
    .map_err(|err| err.to_string())
}

/// Candidates suppressed because of the blacklist, not for another reason such as
/// an automated sender mailbox. The disposition row covers rows written before
/// `disposition_reason` was set on blacklist suppression.
const BLACKLIST_SUPPRESSED_SQL: &str = "ec.current_disposition IN ('suppressed','blacklisted')
     AND (ec.disposition_reason='blacklist'
          OR EXISTS (SELECT 1 FROM candidate_dispositions cd
                     WHERE cd.candidate_id=ec.id AND cd.is_current=1 AND cd.reason_code='blacklist'))";

const LIFTED_CANDIDATE_UPDATE_SQL: &str = r#"UPDATE email_candidates
     SET status='quarantined',candidate_status='quarantined',current_disposition='quarantined',
         disposition_reason='blacklist_lifted',outreach_eligible=0,quality_score=0.35,
         score_factors_json='{"suppression":"lifted","requires_reverification":true}'
     WHERE id=?1"#;

fn lift_blacklist_suppression(db: &Connection, key: &str, lifted_at: &str) -> Result<(), String> {
    db.execute(
        "UPDATE suppressions SET lifted_at=?2
         WHERE comparison_key=?1 COLLATE NOCASE AND reason='blacklist' AND lifted_at IS NULL",
        params![key, lifted_at],
    )
    .map_err(|err| err.to_string())?;
    let candidate_ids = {
        let mut statement = db
            .prepare(&format!(
                "SELECT id FROM email_candidates ec
                     WHERE (comparison_key=?1 COLLATE NOCASE OR canonical_email=?1 COLLATE NOCASE)
                       AND {}",
                BLACKLIST_SUPPRESSED_SQL
            ))
            .map_err(|err| err.to_string())?;

        statement
            .query_map(params![key], |row| row.get::<_, String>(0))
            .map_err(|err| err.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?
    };
    for candidate_id in candidate_ids {
        db.execute(
            "UPDATE candidate_dispositions SET is_current=0
             WHERE candidate_id=?1 AND is_current=1",
            params![candidate_id],
        )
        .map_err(|err| err.to_string())?;
        db.execute(
            "INSERT INTO candidate_dispositions(
                id,candidate_id,disposition,reason_code,details_json,is_current,created_at
             ) VALUES (?1,?2,'quarantined','blacklist_lifted','{}',1,?3)",
            params![new_contact_id(), candidate_id, lifted_at],
        )
        .map_err(|err| err.to_string())?;
        db.execute(LIFTED_CANDIDATE_UPDATE_SQL, params![candidate_id])
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub(crate) fn consolidate_legacy_blacklist(
    db: &mut Connection,
    legacy_path: &std::path::Path,
    inject_before_commit: bool,
) -> Result<(), String> {
    if !legacy_path.exists() {
        return Ok(());
    }
    let already_migrated: bool = db
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM blacklist_legacy_migrations WHERE legacy_file='blacklist.sqlite3'
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|err| err.to_string())?;
    if already_migrated {
        return Ok(());
    }

    let legacy = Connection::open(legacy_path).map_err(|err| {
        format!(
            "Apertura blacklist legacy non riuscita ({}): {err}",
            legacy_path.display()
        )
    })?;
    if !has_table(&legacy, "blacklist").map_err(|err| {
        format!(
            "Lettura blacklist legacy non riuscita ({}): {err}. Conservare il file e ripristinarlo da backup.",
            legacy_path.display()
        )
    })? {
        return Err(format!(
            "Blacklist legacy senza tabella blacklist: {}",
            legacy_path.display()
        ));
    }
    let rows = {
        let mut statement = legacy
            .prepare(
                "SELECT normalized_email,source_file,imported_at,note
                 FROM blacklist",
            )
            .map_err(|err| {
                format!(
                    "Lettura blacklist legacy non riuscita ({}): {err}. Conservare il file e ripristinarlo da backup.",
                    legacy_path.display()
                )
            })?;

        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|err| {
                format!(
                    "Lettura blacklist legacy non riuscita ({}): {err}. Conservare il file e ripristinarlo da backup.",
                    legacy_path.display()
                )
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| {
                format!(
                    "Lettura blacklist legacy non riuscita ({}): {err}. Conservare il file e ripristinarlo da backup.",
                    legacy_path.display()
                )
            })?
    };

    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| err.to_string())?;
    let already_migrated_in_tx: bool = tx
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM blacklist_legacy_migrations WHERE legacy_file='blacklist.sqlite3'
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|err| err.to_string())?;
    if already_migrated_in_tx {
        tx.commit().map_err(|err| err.to_string())?;
        return Ok(());
    }
    let normalized_keys = rows
        .iter()
        .map(|(email, _, _, _)| email.trim().to_lowercase())
        .collect::<HashSet<_>>();
    for (email, source_file, imported_at, note) in &rows {
        tx.execute(
            "INSERT OR IGNORE INTO blacklist(normalized_email,source_file,imported_at,note)
             VALUES (?1,?2,?3,?4)",
            params![email.trim().to_lowercase(), source_file, imported_at, note],
        )
        .map_err(|err| err.to_string())?;
    }
    if inject_before_commit {
        return Err(
            "errore blacklist legacy iniettato dopo insert e prima della cancellazione contatti"
                .to_owned(),
        );
    }
    for email in &normalized_keys {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM blacklist WHERE normalized_email=?1 COLLATE NOCASE)",
                params![email],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())?;
        if !exists {
            return Err(
                "Consolidamento blacklist legacy incompleto: chiave normalizzata non persistita"
                    .to_string(),
            );
        }
    }
    let mut removed = 0;
    for (email, source_file, imported_at, _) in &rows {
        removed += suppress_blacklisted_email(&tx, email, source_file, imported_at)?;
    }
    tx.execute(
        "INSERT INTO blacklist_legacy_migrations(legacy_file,imported_at,row_count)
         VALUES ('blacklist.sqlite3',?1,?2)",
        params![chrono::Utc::now().to_rfc3339(), rows.len() as i64],
    )
    .map_err(|err| err.to_string())?;
    tx.commit().map_err(|err| err.to_string())?;
    println!(
        "[blacklist] Consolidamento legacy completato: righe={}, contatti_rimossi={removed}",
        rows.len()
    );
    Ok(())
}

pub(crate) fn load_blacklist_set_from_db(db: &Connection) -> Result<HashSet<String>, String> {
    let mut stmt = db
        .prepare("SELECT normalized_email FROM blacklist")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|err| err.to_string())
}

fn persist_blacklist_manifest(
    db: &Connection,
    file: &ImportFile,
    parsed: &crate::parsing::ParsedFile,
) -> Result<(), String> {
    let digest = crate::imports::sha256_hex(&file.bytes);
    let already_recorded: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM imports
             WHERE relative_path=?1 COLLATE BINARY AND content_sha256=?2
               AND hash_algorithm='sha256' AND status='completed')",
            params![file.relative_path, digest],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if already_recorded {
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339();
    let import_id = new_contact_id();
    let attempt_id = new_contact_id();
    let status = "completed";
    let report = serde_json::json!({
        "source_kind": "blacklist",
        "physical_rows": parsed.physical_rows,
        "successfully_read_rows": parsed.successful_rows,
        "malformed_rows": parsed.malformed_rows,
        "failed_sheets": parsed.failed_sheets,
        "candidate_occurrences": parsed.contacts.len(),
        "suppressed_blacklisted_candidates": parsed.contacts.iter()
            .filter(|contact| is_valid_email_syntax(&contact.normalized_email)).count(),
    })
    .to_string();
    db.execute(
        "INSERT INTO import_attempts(
            id,status,parser_version,started_at,completed_at,report_json
         ) VALUES (?1,?2,'parser-v2',?3,?3,?4)",
        params![attempt_id, status, now, report],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "INSERT INTO imports(
            id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at,
            import_attempt_id,content_sha256,hash_algorithm,file_size_bytes,parser_version,
            encoding,delimiter,sheet_count,started_at,completed_at,error_count,report_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?4,'sha256',?10,'parser-v2',?11,?12,?13,?8,?8,?14,?15)",
        params![
            import_id,
            file.name,
            file.relative_path,
            digest,
            status,
            parsed.parsed_rows as i64,
            parsed.contacts.len() as i64,
            now,
            attempt_id,
            file.bytes.len() as i64,
            parsed.encoding,
            parsed.delimiter,
            parsed.sheet_count as i64,
            parsed.row_errors.len() as i64,
            report,
        ],
    )
    .map_err(|error| error.to_string())?;
    for source in &parsed.source_rows {
        let parse_status = match source.parse_status.as_str() {
            "parsed" | "header" | "empty" | "malformed" | "failed" => source.parse_status.as_str(),
            "no_candidate" => "empty",
            _ => "malformed",
        };
        let row_id = format!(
            "{}:{}:{}",
            import_id,
            source.sheet.as_deref().unwrap_or(""),
            source.source_row
        );
        db.execute(
            "INSERT INTO source_rows(
                id,import_id,source_sheet,row_number,raw_context,parse_status,error_message,
                candidate_count,created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                row_id,
                import_id,
                source.sheet.as_deref().unwrap_or(""),
                source.source_row as i64,
                source.raw_context,
                parse_status,
                source.error,
                source.candidate_count as i64,
                now,
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    let mut occurrences = HashMap::<(String, String), usize>::new();
    for contact in &parsed.contacts {
        let source_row_id = format!(
            "{}:{}:{}",
            import_id,
            contact.source_sheet.as_deref().unwrap_or(""),
            contact.source_row
        );
        let key = (source_row_id.clone(), contact.comparison_key.clone());
        let occurrence = occurrences.entry(key).or_default();
        let occurrence_index = *occurrence;
        *occurrence += 1;
        let syntax_ok = is_valid_email_syntax(&contact.normalized_email);
        let disposition = if syntax_ok {
            "suppressed"
        } else {
            "invalid_syntax"
        };
        let reason = if syntax_ok {
            "blacklist"
        } else {
            "syntax_invalid"
        };
        let candidate_id = new_contact_id();
        db.execute(
            "INSERT INTO email_candidates(
                id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
                extraction_method,status,syntax_status,routing_status,mailbox_status,
                candidate_status,current_disposition,disposition_reason,outreach_eligible,
                quality_score,score_factors_json,occurrence_index,first_seen_at,last_seen_at
             ) VALUES (?1,?2,?3,?4,?5,?6,'parser',?7,?8,'not_checked','unknown',
                       ?7,?7,?9,0,?10,?11,?12,?13,?13)",
            params![
                candidate_id,
                source_row_id,
                contact.raw_email,
                contact.normalized_email,
                contact.comparison_key,
                contact.normalization_method,
                disposition,
                if syntax_ok {
                    "syntax_ok"
                } else {
                    "invalid_syntax"
                },
                reason,
                if syntax_ok { 0.0 } else { 0.05 },
                serde_json::json!({"suppression":"blacklist","routing":"not_checked"}).to_string(),
                occurrence_index as i64,
                now,
            ],
        )
        .map_err(|error| error.to_string())?;
        db.execute(
            "INSERT INTO candidate_dispositions(
                id,candidate_id,disposition,reason_code,created_at
             ) VALUES (?1,?2,?3,?4,?5)",
            params![new_contact_id(), candidate_id, disposition, reason, now],
        )
        .map_err(|error| error.to_string())?;
        db.execute(
            "INSERT INTO verification_events(
                id,comparison_key,candidate_id,method,routing_status,mailbox_status,
                error_class,details_json,checked_at
             ) VALUES (?1,?2,?3,'suppression','not_checked','unknown',?4,?5,?6)",
            params![
                new_contact_id(),
                contact.comparison_key,
                candidate_id,
                reason,
                serde_json::json!({"source_kind":"blacklist"}).to_string(),
                now,
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn persist_failed_blacklist_manifest(
    db: &Connection,
    file: &ImportFile,
    error: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let import_id = new_contact_id();
    let attempt_id = new_contact_id();
    let digest = crate::imports::sha256_hex(&file.bytes);
    let report = serde_json::json!({"source_kind":"blacklist","file_error":error}).to_string();
    db.execute(
        "INSERT INTO import_attempts(
            id,status,parser_version,started_at,completed_at,report_json,error
         ) VALUES (?1,'failed','parser-v2',?2,?2,?3,?4)",
        params![attempt_id, now, report, error],
    )
    .map_err(|db_error| db_error.to_string())?;
    db.execute(
        "INSERT INTO imports(
            id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at,
            import_attempt_id,content_sha256,hash_algorithm,file_size_bytes,parser_version,
            encoding,delimiter,sheet_count,started_at,completed_at,error_count,report_json
         ) VALUES (?1,?2,?3,?4,'failed',0,0,?5,?6,?4,'sha256',?7,'parser-v2',
                   'unknown',NULL,0,?5,?5,1,?8)",
        params![
            import_id,
            file.name,
            file.relative_path,
            digest,
            now,
            attempt_id,
            file.bytes.len() as i64,
            report,
        ],
    )
    .map_err(|db_error| db_error.to_string())?;
    db.execute(
        "INSERT INTO source_rows(
            id,import_id,source_sheet,row_number,raw_context,parse_status,error_message,
            candidate_count,created_at
         ) VALUES (?1,?2,'',1,'','failed',?3,0,?4)",
        params![format!("{import_id}::1"), import_id, error, now],
    )
    .map_err(|db_error| db_error.to_string())?;
    Ok(())
}

pub(crate) fn import_blacklist_files_transaction(
    db: &mut Connection,
    files: Vec<ImportFile>,
    inject_before_commit: bool,
) -> Result<BlacklistImportReport, String> {
    let import_start = std::time::Instant::now();
    let tx = db.transaction().map_err(|err| err.to_string())?;
    let completed_imports = {
        let mut statement = tx
            .prepare(
                "SELECT relative_path,content_sha256 FROM imports
             WHERE status='completed' AND hash_algorithm='sha256' AND content_sha256 IS NOT NULL",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<HashSet<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    let mut inserted = 0;
    let mut duplicates = 0;
    let mut invalid = 0;
    let mut removed_contacts = 0;
    for file in files {
        let source_hash = crate::imports::sha256_hex(&file.bytes);
        let completed_import_key = (file.relative_path.clone(), source_hash.clone());
        if completed_imports.contains(&completed_import_key) {
            println!(
                "[import][file:skip] file=\"{}\" path=\"{}\" sha256={} reason=\"already_imported\"",
                display_name(&file),
                file.relative_path,
                &source_hash[..8.min(source_hash.len())]
            );
            continue;
        }
        let parsed = match parse_file(&file) {
            Ok(parsed) => parsed,
            Err(error) => {
                persist_failed_blacklist_manifest(&tx, &file, &error)?;
                invalid += 1;
                continue;
            }
        };
        persist_blacklist_manifest(&tx, &file, &parsed)?;
        if parsed.contacts.is_empty() {
            invalid += 1;
        }
        for contact in &parsed.contacts {
            let canonical = canonicalize_email(&contact.normalized_email);
            if !is_valid_email_syntax(&canonical) {
                invalid += 1;
                continue;
            }
            let email = canonical.to_lowercase();
            let imported_at = chrono::Utc::now().to_rfc3339();
            let changed = tx.execute(
                "INSERT OR IGNORE INTO blacklist(normalized_email,source_file,imported_at) VALUES (?1,?2,?3)",
                params![email, display_name(&file), imported_at],
            ).map_err(|err| err.to_string())?;
            if changed == 1 {
                inserted += 1;
            } else {
                duplicates += 1;
            }
            removed_contacts +=
                suppress_blacklisted_email(&tx, &email, &display_name(&file), &imported_at)?;
        }
    }
    if inject_before_commit {
        return Err("errore blacklist iniettato prima del commit".to_owned());
    }
    tx.commit().map_err(|err| err.to_string())?;
    println!(
        "[import][blacklist] inserted={} duplicates={} invalid={} suppressed_contacts={} duration_ms={:.2}",
        inserted,
        duplicates,
        invalid,
        removed_contacts,
        import_start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(BlacklistImportReport {
        inserted,
        duplicates,
        invalid,
        removed_contacts,
        failed_files: Vec::new(),
    })
}

#[tauri::command]
pub(crate) async fn import_blacklist_paths(
    app: tauri::AppHandle,
    paths: Vec<String>,
) -> Result<BlacklistImportReport, String> {
    crate::database::blocking(move || {
        let mut total = BlacklistImportReport {
            inserted: 0,
            duplicates: 0,
            invalid: 0,
            removed_contacts: 0,
            failed_files: Vec::new(),
        };
        let requested = paths.len();
        for path in paths {
            let file_path = PathBuf::from(&path);
            let result = std::fs::read(&file_path)
                .map_err(|error| format!("Impossibile leggere {}: {error}", file_path.display()))
                .map(|bytes| {
                    let name = file_path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or(&path)
                        .to_owned();
                    ImportFile {
                        path: None,
                        name: name.clone(),
                        relative_path: crate::imports::standalone_relative_path(&file_path, &name),
                        bytes,
                    }
                })
                .and_then(|file| {
                    let mut db = open_db(&app)?;
                    import_blacklist_files_transaction(&mut db, vec![file], false)
                });
            match result {
                Ok(report) => {
                    total.inserted += report.inserted;
                    total.duplicates += report.duplicates;
                    total.invalid += report.invalid;
                    total.removed_contacts += report.removed_contacts;
                }
                Err(error) => {
                    eprintln!("[blacklist][errore] {error}");
                    total.failed_files.push(format!("{path}: {error}"));
                }
            }
        }
        if requested > 0 && total.failed_files.len() == requested {
            return Err(total.failed_files.join("\n"));
        }
        Ok(total)
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_blacklist(
    app: tauri::AppHandle,
    query: Option<String>,
    limit: usize,
    offset: Option<usize>,
) -> Result<BlacklistList, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        load_blacklist_page(
            &db,
            query.as_deref().unwrap_or_default(),
            limit,
            offset.unwrap_or(0),
        )
    })
    .await
}

pub(crate) fn load_blacklist_page(
    db: &Connection,
    query: &str,
    limit: usize,
    offset: usize,
) -> Result<BlacklistList, String> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let pattern = format!("%{}%", query.trim());
    let total = db
        .query_row(
            "SELECT COUNT(*) FROM blacklist
             WHERE normalized_email LIKE ?1 COLLATE NOCASE OR source_file LIKE ?1 COLLATE NOCASE OR note LIKE ?1 COLLATE NOCASE",
            params![pattern],
            |row| Ok(row.get::<_, i64>(0)? as usize),
        )
        .map_err(|err| err.to_string())?;
    let mut stmt = db
        .prepare(
            "SELECT normalized_email,source_file,imported_at,note FROM blacklist
         WHERE normalized_email LIKE ?1 COLLATE NOCASE OR source_file LIKE ?1 COLLATE NOCASE OR note LIKE ?1 COLLATE NOCASE
         ORDER BY lower(normalized_email),normalized_email LIMIT ?2 OFFSET ?3",
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(params![pattern, limit as i64, offset as i64], |row| {
            Ok(BlacklistEntry {
                email: row.get(0)?,
                source_file: row.get(1)?,
                imported_at: row.get(2)?,
                note: row.get(3)?,
            })
        })
        .map_err(|err| err.to_string())?;
    let entries = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    Ok(BlacklistList {
        has_previous: offset > 0,
        has_next: offset.saturating_add(entries.len()) < total,
        entries,
        total,
        limit,
        offset,
    })
}

#[tauri::command]
pub(crate) async fn add_blacklist_entry(
    app: tauri::AppHandle,
    email: String,
    note: Option<String>,
) -> Result<bool, String> {
    crate::database::blocking(move || {
        let add_start = std::time::Instant::now();
        let canonical = canonicalize_email(&email);
        if !is_valid_email_syntax(&canonical) {
            return Err("Indirizzo email non valido".to_owned());
        }
        let key = canonical.to_lowercase();
        let mut db = open_db(&app)?;
        let tx = db.transaction().map_err(|err| err.to_string())?;
        let now = chrono::Utc::now().to_rfc3339();
        let note_clean = note.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO blacklist(normalized_email,source_file,imported_at,note)
         VALUES (?1,'manual',?2,?3)",
                params![key, now, note_clean],
            )
            .map_err(|err| err.to_string())?;
        let suppressed = suppress_blacklisted_email(&tx, &key, "manual", &now)?;
        tx.commit().map_err(|err| err.to_string())?;
        println!(
            "[import][blacklist_entry] email=\"{}\" inserted={} suppressed_contacts={} duration_ms={:.2}",
            key,
            changed == 1,
            suppressed,
            add_start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(changed == 1)
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_blacklist_entry(
    app: tauri::AppHandle,
    email: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        let delete_start = std::time::Instant::now();
        let mut db = open_db(&app)?;
        let tx = db.transaction().map_err(|err| err.to_string())?;
        let key = email.trim().to_lowercase();
        tx.execute(
            "DELETE FROM blacklist WHERE normalized_email=?1 COLLATE NOCASE",
            params![key],
        )
        .map_err(|err| err.to_string())?;
        lift_blacklist_suppression(&tx, &key, &chrono::Utc::now().to_rfc3339())?;
        tx.commit().map_err(|err| err.to_string())?;
        println!(
            "[delete][blacklist_entry] email=\"{}\" suppression_lifted=true duration_ms={:.2}",
            key,
            delete_start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn clear_blacklist(app: tauri::AppHandle) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || clear_blacklist_inner(&app))
        .await
        .map_err(|error| format!("Cancellazione blacklist interrotta: {error}"))?
}

pub(crate) fn clear_blacklist_inner(app: &tauri::AppHandle) -> Result<usize, String> {
    let mut db = open_db(app)?;
    clear_blacklist_connection(&mut db)
}

pub(crate) fn clear_blacklist_connection(db: &mut Connection) -> Result<usize, String> {
    let delete_start = std::time::Instant::now();
    let tx = db.transaction().map_err(|err| err.to_string())?;
    let deleted = tx
        .execute("DELETE FROM blacklist", [])
        .map_err(|err| err.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    lift_all_blacklist_suppressions(&tx, &now)?;
    let blacklist_import_ids = {
        let mut statement = tx
            .prepare(
                "SELECT id FROM imports
                 WHERE report_json LIKE '%\"source_kind\":\"blacklist\"%'
                    OR report_json LIKE '%\"source_kind\": \"blacklist\"%'",
            )
            .map_err(|err| err.to_string())?;

        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|err| err.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?
    };
    for import_id in &blacklist_import_ids {
        tx.execute(
            "DELETE FROM email_candidates WHERE source_row_id IN (
                SELECT id FROM source_rows WHERE import_id=?1
            )",
            params![import_id],
        )
        .map_err(|err| err.to_string())?;
        tx.execute(
            "DELETE FROM source_rows WHERE import_id=?1",
            params![import_id],
        )
        .map_err(|err| err.to_string())?;
        tx.execute("DELETE FROM imports WHERE id=?1", params![import_id])
            .map_err(|err| err.to_string())?;
    }
    tx.execute(
        "DELETE FROM import_attempts
         WHERE report_json LIKE '%\"source_kind\":\"blacklist\"%'
            OR report_json LIKE '%\"source_kind\": \"blacklist\"%'",
        [],
    )
    .map_err(|err| err.to_string())?;
    tx.commit().map_err(|err| err.to_string())?;
    println!(
        "[delete][blacklist_clear] entries_deleted={} imports_cleared={} duration_ms={:.2}",
        deleted,
        blacklist_import_ids.len(),
        delete_start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(deleted)
}

fn lift_all_blacklist_suppressions(db: &Connection, lifted_at: &str) -> Result<(), String> {
    db.execute(
        "UPDATE suppressions SET lifted_at=?1
         WHERE reason='blacklist' AND lifted_at IS NULL",
        params![lifted_at],
    )
    .map_err(|err| err.to_string())?;
    let candidate_ids = {
        let mut statement = db
            .prepare(&format!(
                "SELECT id FROM email_candidates ec WHERE {BLACKLIST_SUPPRESSED_SQL}"
            ))
            .map_err(|err| err.to_string())?;

        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|err| err.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())?
    };
    for candidate_id in candidate_ids {
        db.execute(
            "UPDATE candidate_dispositions SET is_current=0
             WHERE candidate_id=?1 AND is_current=1",
            params![candidate_id],
        )
        .map_err(|err| err.to_string())?;
        db.execute(
            "INSERT INTO candidate_dispositions(
                id,candidate_id,disposition,reason_code,details_json,is_current,created_at
             ) VALUES (lower(hex(randomblob(16))),?1,'quarantined','blacklist_lifted','{}',1,?2)",
            params![candidate_id, lifted_at],
        )
        .map_err(|err| err.to_string())?;
        db.execute(LIFTED_CANDIDATE_UPDATE_SQL, params![candidate_id])
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn get_blacklist_stats(app: tauri::AppHandle) -> Result<usize, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        load_blacklist_stats(&db)
    })
    .await
}

pub(crate) fn load_blacklist_stats(db: &Connection) -> Result<usize, String> {
    db.query_row("SELECT COUNT(*) FROM blacklist", [], |row| {
        Ok(row.get::<_, i64>(0)? as usize)
    })
    .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::setup_schema;

    fn seeded_database() -> Connection {
        let db = Connection::open_in_memory().expect("database");
        setup_schema(&db).expect("schema");
        db.execute(
            "INSERT INTO imports(
                id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at
             ) VALUES ('import-1','legacy.csv','Scuole/legacy.csv','legacy','completed',1,1,'2026-01-01T00:00:00Z')",
            [],
        )
        .expect("import");
        db.execute(
            "INSERT INTO source_rows(
                id,import_id,source_sheet,row_number,raw_context,parse_status,created_at
             ) VALUES ('row-1','import-1','',1,'{\"email\":\"keep@example.test\"}','parsed','2026-01-01T00:00:00Z')",
            [],
        )
        .expect("source row");
        db.execute(
            "INSERT INTO contacts(
                id,email,email_local_part,email_domain,email_tld,status,validation_notes,
                source_file,category,email_comparison_key
             ) VALUES ('contact-1','keep@example.test','keep','example.test','.test','valid','[]',
                       'legacy.csv','Scuole','keep@example.test')",
            [],
        )
        .expect("contact");
        db.execute(
            "INSERT INTO email_candidates(
                id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
                status,candidate_status,current_disposition,contact_id,first_seen_at,last_seen_at
             ) VALUES ('candidate-1','row-1','Keep@Example.test','keep@example.test','keep@example.test','lowercase',
                       'linked','linked','linked','contact-1','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            [],
        )
        .expect("candidate");
        db
    }

    #[test]
    fn blacklist_suppression_removes_contact_but_preserves_source_evidence() {
        let db = seeded_database();
        let removed = suppress_blacklisted_email(
            &db,
            "keep@example.test",
            "user.csv",
            "2026-02-01T00:00:00Z",
        )
        .expect("suppression");
        assert_eq!(removed, 1);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM contacts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM email_candidates", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM source_rows", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let state: (String, String, String) = db
            .query_row(
                "SELECT status,current_disposition,disposition FROM email_candidates ec
                 JOIN candidate_dispositions cd ON cd.candidate_id=ec.id AND cd.is_current=1
                 WHERE ec.id='candidate-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            state,
            (
                "suppressed".into(),
                "suppressed".into(),
                "suppressed".into()
            )
        );
        assert!(db
            .query_row(
                "SELECT lifted_at IS NULL FROM suppressions WHERE comparison_key='keep@example.test'",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
    }

    #[test]
    fn clearing_blacklist_keeps_automated_mailboxes_suppressed() {
        let db = seeded_database();
        db.execute(
            "INSERT INTO email_candidates(
                id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
                status,candidate_status,current_disposition,disposition_reason,first_seen_at,last_seen_at
             ) VALUES ('candidate-auto','row-1','noreply@example.test','noreply@example.test',
                       'noreply@example.test','lowercase','linked','linked','linked',NULL,
                       '2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        crate::contacts::suppress_non_contact_mailbox(
            &db,
            "noreply@example.test",
            "noreply@example.test",
            "legacy.csv",
            "2026-01-02T00:00:00Z",
        )
        .unwrap();
        for key in ["keep@example.test", "noreply@example.test"] {
            suppress_blacklisted_email(&db, key, "user.csv", "2026-02-01T00:00:00Z").unwrap();
        }
        lift_all_blacklist_suppressions(&db, "2026-03-01T00:00:00Z").unwrap();

        let state = |id: &str| -> (String, Option<String>) {
            db.query_row(
                "SELECT current_disposition,disposition_reason FROM email_candidates WHERE id=?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(
            state("candidate-auto"),
            ("suppressed".into(), Some("non_contact_mailbox".into()))
        );
        assert_eq!(
            state("candidate-1"),
            ("quarantined".into(), Some("blacklist_lifted".into()))
        );
        let automated: (String, bool) = db
            .query_row(
                "SELECT reason,lifted_at IS NULL FROM suppressions WHERE comparison_key='noreply@example.test'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(automated, ("non_contact_mailbox".into(), true));
    }

    #[test]
    fn removing_blacklist_lifts_to_quarantine_without_recreating_contact() {
        let db = seeded_database();
        suppress_blacklisted_email(&db, "keep@example.test", "user.csv", "2026-02-01T00:00:00Z")
            .expect("suppression");
        lift_blacklist_suppression(&db, "keep@example.test", "2026-03-01T00:00:00Z").expect("lift");

        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM contacts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "lifting a suppression must not recreate an exportable contact"
        );
        let state: (String, String, f64) = db
            .query_row(
                "SELECT status,current_disposition,quality_score FROM email_candidates WHERE id='candidate-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state.0, "quarantined");
        assert_eq!(state.1, "quarantined");
        assert!(
            state.2 > 0.0 && state.2 < 1.0,
            "quarantined candidate is scored but not exportable"
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM candidate_dispositions
                 WHERE candidate_id='candidate-1' AND disposition='quarantined' AND is_current=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        assert!(db
            .query_row(
                "SELECT lifted_at IS NOT NULL FROM suppressions WHERE comparison_key='keep@example.test'",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM source_rows", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn blacklist_manifest_retains_non_email_rows_without_rejecting_the_file() {
        let mut db = Connection::open_in_memory().expect("database");
        setup_schema(&db).expect("schema");
        let file = ImportFile {
            path: None,
            name: "blacklist.txt".to_owned(),
            relative_path: "blacklist/blacklist.txt".to_owned(),
            bytes: b"annotazione senza indirizzo\nblocked@example.test\n".to_vec(),
        };

        let report = import_blacklist_files_transaction(&mut db, vec![file], false)
            .expect("blacklist import");

        assert_eq!(report.inserted, 1);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM source_rows WHERE parse_status='empty'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM email_candidates", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
    }
}
