//! Run lifecycle commands: status, pause, cancel, rollback, history.

use super::*;

#[tauri::command]
pub(crate) async fn get_ai_run_status(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<AiRunStatus, String> {
    crate::database::blocking(move || {
        open_db(&app)?
            .query_row(
                "SELECT id,model,profile,status,total,processed,updated,reviews_created,skipped,errors,interruption_reason,started_at,provider,parent_run_id
         FROM ai_classification_runs WHERE id=?1",
                params![run_id],
                |r| {
                    Ok(AiRunStatus {
                        id: r.get(0)?,
                        model: r.get(1)?,
                        profile: r.get(2)?,
                        status: r.get(3)?,
                        total: r.get::<_, i64>(4)? as usize,
                        processed: r.get::<_, i64>(5)? as usize,
                        updated: r.get::<_, i64>(6)? as usize,
                        reviews_created: r.get::<_, i64>(7)? as usize,
                        skipped: r.get::<_, i64>(8)? as usize,
                        errors: r.get::<_, i64>(9)? as usize,
                        interruption_reason: r.get(10)?,
                        started_at: r.get(11)?,
                        provider: r.get(12)?,
                        parent_run_id: r.get(13)?,
                    })
                },
            )
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn resume_ai_classification(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<AiEnrichmentReport, String> {
    let mut db = open_db(&app)?;
    let (_model, request_json, status): (String, String, String) = db
        .query_row(
            "SELECT model,request_json,status FROM ai_classification_runs WHERE id=?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    if status != "paused" {
        return Err(format!(
            "L'esecuzione in stato {status} non può essere ripresa"
        ));
    }
    let deterministic_only = serde_json::from_str::<serde_json::Value>(&request_json)
        .ok()
        .and_then(|value| {
            value
                .get("deterministic_only")
                .and_then(|flag| flag.as_bool())
        })
        .unwrap_or(false);
    let request: AiClassificationRequest =
        serde_json::from_str(&request_json).map_err(|e| e.to_string())?;
    claim_ai_run(&mut db, &run_id, chrono::Utc::now().timestamp())?;
    drop(db);
    if deterministic_only {
        return crate::ai_commands::process_deterministic_only_run(&app, &run_id);
    }
    crate::ai_commands::spawn_ai_run_for_resume(app, run_id, request);
    Ok(AiEnrichmentReport {
        processed: 0,
        updated: 0,
        skipped: 0,
        errors: Vec::new(),
        misclassified: Vec::new(),
    })
}

pub(crate) fn transition_ai_run(
    db: &Connection,
    run_id: &str,
    allowed: &[&str],
    next: &str,
) -> Result<(), String> {
    let placeholders = (3..3 + allowed.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut values = vec![
        rusqlite::types::Value::Text(next.to_owned()),
        rusqlite::types::Value::Text(run_id.to_owned()),
    ];
    values.extend(
        allowed
            .iter()
            .map(|status| rusqlite::types::Value::Text((*status).to_owned())),
    );
    let changed = db
        .execute(
            &format!(
                "UPDATE ai_classification_runs SET status=?1
                 WHERE id=?2 AND status IN ({placeholders})"
            ),
            rusqlite::params_from_iter(values),
        )
        .map_err(|e| e.to_string())?;
    if changed == 1 {
        return Ok(());
    }
    let status: String = db
        .query_row(
            "SELECT status FROM ai_classification_runs WHERE id=?1",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Err(format!("Transizione IA non valida: {status} → {next}"))
}

#[tauri::command]
pub(crate) async fn pause_ai_classification(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        transition_ai_run(&db, &run_id, &["running"], "pausing")
    })
    .await
}

pub(crate) fn pause_owned_ai_runs_on_shutdown(
    db: &Connection,
    session_id: &str,
) -> Result<usize, String> {
    db.execute(
        "UPDATE ai_classification_runs
         SET status='paused',owner_session=NULL,lease_expires_at=NULL,
             interruption_reason='Pausa automatica alla chiusura dell''app'
         WHERE status IN ('running','pausing') AND owner_session=?1",
        params![session_id],
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn cancel_ai_classification(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        transition_ai_run(&db, &run_id, &["running", "pausing", "paused"], "cancelled")
    })
    .await
}

#[tauri::command]
pub(crate) async fn reset_ai_classification(
    app: tauri::AppHandle,
    run_id: String,
    _request: AiClassificationRequest,
) -> Result<AiRollbackReport, String> {
    crate::database::blocking(move || {
        let mut db = open_db(&app)?;
        let status: String = db
            .query_row(
                "SELECT status FROM ai_classification_runs WHERE id=?1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if matches!(status.as_str(), "running" | "pausing" | "paused") {
            transition_ai_run(&db, &run_id, &["running", "pausing", "paused"], "cancelled")?;
            wait_for_ai_run_release(&db, &run_id)?;
        }
        rollback_ai_run_conn(&mut db, &run_id)
    })
    .await
}

/// Waits until the worker releases a cancelled run. A lease that expired means the
/// worker is gone, so waiting stops there too; the deadline bounds a stuck worker.
pub(crate) fn wait_for_ai_run_release(db: &Connection, run_id: &str) -> Result<(), String> {
    let deadline =
        std::time::Instant::now() + Duration::from_secs(2 * crate::AI_LEASE_SECONDS.unsigned_abs());
    loop {
        let released: bool = db
            .query_row(
                "SELECT owner_session IS NULL OR lease_expires_at IS NULL OR lease_expires_at<?2
                 FROM ai_classification_runs WHERE id=?1",
                params![run_id, chrono::Utc::now().timestamp()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if released {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("La classificazione non si è arrestata in tempo: riprovare".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tauri::command]
pub(crate) async fn rollback_ai_run(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<AiRollbackReport, String> {
    crate::database::blocking(move || {
        let mut db = open_db(&app)?;
        rollback_ai_run_conn(&mut db, &run_id)
    })
    .await
}

#[tauri::command]
pub(crate) async fn rollback_ai_runs(
    app: tauri::AppHandle,
    scope: String,
) -> Result<AiRollbackReport, String> {
    crate::database::blocking(move || {
        let mut db = open_db(&app)?;
        rollback_ai_scope_conn(&mut db, &scope)
    })
    .await
}

pub(crate) fn rollback_ai_run_conn(
    db: &mut Connection,
    run_id: &str,
) -> Result<AiRollbackReport, String> {
    let exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_classification_runs WHERE id=?1)",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("Esecuzione IA non trovata".to_owned());
    }
    let latest: Option<String> = db
        .query_row(
            "SELECT id FROM ai_classification_runs
             WHERE rolled_back_at IS NULL
               AND EXISTS(SELECT 1 FROM ai_field_changes f WHERE f.run_id=ai_classification_runs.id AND f.status='applied' AND f.rolled_back=0)
             ORDER BY coalesce(applied_order,rowid) DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    match latest.as_deref() {
        Some(latest_id) if latest_id != run_id => {
            return Err("Puoi ripristinare solo l'ultima esecuzione applicata".to_owned());
        }
        None => {
            return Ok(AiRollbackReport {
                reverted: 0,
                contacts_reverted: 0,
                runs_rolled_back: 0,
            });
        }
        _ => {}
    }
    rollback_ai_scope_conn(db, "latest")
}

pub(crate) fn rollback_ai_scope_conn(
    db: &mut Connection,
    scope: &str,
) -> Result<AiRollbackReport, String> {
    if !matches!(scope, "latest" | "beginning") {
        return Err("Ambito rollback non valido".to_owned());
    }
    let run_ids: Vec<String> = db
        .prepare(
            "SELECT id FROM ai_classification_runs
             WHERE rolled_back_at IS NULL
               AND status NOT IN ('running','pausing','paused')
               AND EXISTS(SELECT 1 FROM ai_field_changes f WHERE f.run_id=ai_classification_runs.id AND f.status='applied' AND f.rolled_back=0)
             ORDER BY coalesce(applied_order,rowid) DESC",
        )
        .map_err(|error| error.to_string())?
        .query_map([], |row| row.get(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let selected: Vec<String> = if scope == "latest" {
        run_ids.into_iter().take(1).collect()
    } else {
        run_ids
    };
    if selected.is_empty() {
        return Ok(AiRollbackReport {
            reverted: 0,
            contacts_reverted: 0,
            runs_rolled_back: 0,
        });
    }

    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let mut reverted = 0usize;
    let mut touched = std::collections::HashSet::new();
    for run_id in &selected {
        let changes: Vec<(String, String, String, Option<String>)> = tx
            .prepare(
                "SELECT id,contact_id,field_name,old_value FROM ai_field_changes
                 WHERE run_id=?1 AND status='applied' AND rolled_back=0
                 ORDER BY rowid DESC",
            )
            .map_err(|error| error.to_string())?
            .query_map(params![run_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        for (change_id, contact_id, field_name, old_value) in changes {
            let column = match field_name.as_str() {
                "category"
                | "subcategory"
                | "comune"
                | "provincia"
                | "regione"
                | "taxonomy_version_id"
                | "classification_state"
                | "classification_state_reason"
                | "classification_hash" => Some(field_name.as_str()),
                // Generated from regione: restoring regione restores it.
                "macroarea" => None,
                _ => continue,
            };
            if let Some(column) = column {
                tx.execute(
                    &format!("UPDATE contacts SET {column}=?1 WHERE id=?2"),
                    params![old_value, contact_id],
                )
                .map_err(|error| error.to_string())?;
            }
            tx.execute(
                "UPDATE ai_field_changes SET rolled_back=1,resolved_at=?1 WHERE id=?2",
                params![chrono::Utc::now().to_rfc3339(), change_id],
            )
            .map_err(|error| error.to_string())?;
            touched.insert(contact_id);
            reverted += 1;
        }
        tx.execute(
            "UPDATE contacts SET
                category_id=(SELECT id FROM categories WHERE name=contacts.category),
                subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name=contacts.category AND s.name=contacts.subcategory),
                classification_hash=CASE
                    WHEN NOT EXISTS(
                        SELECT 1 FROM ai_field_changes h
                        WHERE h.run_id=?1 AND h.contact_id=contacts.id
                          AND h.field_name='classification_hash'
                    ) THEN NULL
                    ELSE classification_hash
                END
             WHERE id IN (SELECT contact_id FROM ai_field_changes WHERE run_id=?1)",
            params![run_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE classification_reviews SET status='rejected',resolved_at=?1
             WHERE id IN (SELECT id FROM taxonomy_change_candidates WHERE origin_run_id=?2 AND status='pending')
               AND status='pending'",
            params![chrono::Utc::now().to_rfc3339(), run_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE taxonomy_change_candidates SET status='rejected',decided_at=?1
             WHERE origin_run_id=?2 AND status='pending'",
            params![chrono::Utc::now().to_rfc3339(), run_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE ai_classification_runs SET rolled_back_at=?1 WHERE id=?2",
            params![chrono::Utc::now().to_rfc3339(), run_id],
        )
        .map_err(|error| error.to_string())?;
    }
    tx.execute(
        "UPDATE contacts SET needs_review=EXISTS(
            SELECT 1 FROM taxonomy_change_candidates c
            WHERE c.origin_contact_id=contacts.id AND c.status='pending'
         )",
        [],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;

    println!(
        "[rollback][classification] scope=\"{scope}\" runs={} contacts={} mutations={reverted}",
        selected.len(),
        touched.len()
    );
    Ok(AiRollbackReport {
        reverted,
        contacts_reverted: touched.len(),
        runs_rolled_back: selected.len(),
    })
}

pub(crate) fn row_to_ai_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiRunStatus> {
    Ok(AiRunStatus {
        id: row.get(0)?,
        model: row.get(1)?,
        profile: row.get(2)?,
        status: row.get(3)?,
        total: row.get::<_, i64>(4)? as usize,
        processed: row.get::<_, i64>(5)? as usize,
        updated: row.get::<_, i64>(6)? as usize,
        reviews_created: row.get::<_, i64>(7)? as usize,
        skipped: row.get::<_, i64>(8)? as usize,
        errors: row.get::<_, i64>(9)? as usize,
        interruption_reason: row.get(10)?,
        started_at: row.get(11)?,
        provider: row.get(12)?,
        parent_run_id: row.get(13)?,
    })
}

#[tauri::command]
pub(crate) async fn delete_ai_run(app: tauri::AppHandle, run_id: String) -> Result<(), String> {
    crate::database::blocking(move || {
        let mut db = open_db(&app)?;
        delete_ai_run_from_db(&mut db, &run_id)
    })
    .await
}

pub(crate) fn delete_ai_run_from_db(db: &mut Connection, run_id: &str) -> Result<(), String> {
    let delete_start = std::time::Instant::now();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_classification_runs WHERE id=?1)",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("Esecuzione IA non trovata".to_owned());
    }
    let belongs_to_rollback_stack: bool = tx
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM ai_field_changes
                WHERE run_id=?1 AND status='applied' AND rolled_back=0
            )",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if belongs_to_rollback_stack {
        return Err(
            "Ripristina prima questa run: i log necessari al rollback non possono essere eliminati"
                .to_owned(),
        );
    }
    tx.execute("INSERT INTO ai_applied_provenance(id,run_id,contact_id,field_name,value,evidence,model,prompt_version,created_at)
       SELECT id,run_id,contact_id,field_name,new_value,evidence,
              (SELECT model FROM ai_classification_runs WHERE id=ai_field_changes.run_id),
              (SELECT prompt_version FROM ai_classification_runs WHERE id=ai_field_changes.run_id),created_at
       FROM ai_field_changes WHERE run_id=?1 AND status='applied'
         AND NOT EXISTS (SELECT 1 FROM ai_applied_provenance p WHERE p.id=ai_field_changes.id)", params![run_id]).map_err(|e| e.to_string())?;

    tx.execute(
        "UPDATE ai_applied_provenance SET run_id=NULL WHERE run_id=?1",
        params![run_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM ai_classification_runs WHERE id=?1",
        params![run_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    println!(
        "[delete][ai_run] run_id=\"{}\" deleted=true duration_ms={:.2}",
        run_id,
        delete_start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

#[tauri::command]
pub(crate) async fn list_ai_runs(
    app: tauri::AppHandle,
    limit: Option<usize>,
) -> Result<Vec<AiRunStatus>, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let mut stmt = db
            .prepare(
                "SELECT id,model,profile,status,total,processed,updated,reviews_created,skipped,errors,interruption_reason,started_at,provider,parent_run_id
             FROM ai_classification_runs ORDER BY started_at DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit.unwrap_or(20).min(100) as i64], row_to_ai_run)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    })
    .await
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct AiRunFailure {
    contact_id: String,
    email: String,
    error: String,
    failure_kind: String,
    attempt_count: usize,
}

#[tauri::command]
pub(crate) async fn list_ai_run_failures(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<Vec<AiRunFailure>, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let mut statement = db.prepare(
            "SELECT r.contact_id,c.email,coalesce(r.error,'Errore sconosciuto'),coalesce(r.failure_kind,'validation'),r.attempt_count
         FROM ai_run_candidates r JOIN contacts c ON c.id=r.contact_id
         WHERE r.run_id=?1 AND r.status='error' ORDER BY r.ordinal",
        ).map_err(|error| error.to_string())?;
        let failures = statement
            .query_map(params![run_id], |row| {
                Ok(AiRunFailure {
                    contact_id: row.get(0)?,
                    email: row.get(1)?,
                    error: row.get(2)?,
                    failure_kind: row.get(3)?,
                    attempt_count: row.get::<_, i64>(4)? as usize,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(failures)
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_resumable_ai_run(
    app: tauri::AppHandle,
) -> Result<Option<AiRunStatus>, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        db.query_row(
            "SELECT id,model,profile,status,total,processed,updated,reviews_created,skipped,errors,interruption_reason,started_at,provider,parent_run_id
         FROM ai_classification_runs WHERE status='paused'
         ORDER BY started_at DESC LIMIT 1",
            [],
            row_to_ai_run,
        )
        .optional()
        .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_ai_field_changes(
    app: tauri::AppHandle,
    run_id: String,
    limit: usize,
    offset: Option<usize>,
) -> Result<AiFieldChangeList, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let total = db
            .query_row(
                "SELECT
                (SELECT COUNT(*) FROM ai_field_changes WHERE run_id=?1) +
                (SELECT COUNT(*) FROM ai_run_candidates WHERE run_id=?1 AND status='skipped')",
                params![&run_id],
                |row| Ok(row.get::<_, i64>(0)? as usize),
            )
            .map_err(|e| e.to_string())?;
        let mut stmt = db
            .prepare(
                "SELECT id,run_id,contact_id,review_id,email,field_name,old_value,new_value,
                    change_kind,status,confidence,evidence,created_at,resolved_at,rolled_back
             FROM (
                SELECT h.id,h.run_id,h.contact_id,h.review_id,c.email,h.field_name,h.old_value,h.new_value,
                       h.change_kind,h.status,h.confidence,h.evidence,h.created_at,h.resolved_at,h.rolled_back
                FROM ai_field_changes h JOIN contacts c ON c.id=h.contact_id
                WHERE h.run_id=?1
                UNION ALL
                SELECT q.run_id || ':skipped:' || q.contact_id,q.run_id,q.contact_id,NULL,c.email,
                       'classificazione',NULL,NULL,'saltato','skipped',0.0,'[]',q.processed_at,NULL,0
                FROM ai_run_candidates q JOIN contacts c ON c.id=q.contact_id
                WHERE q.run_id=?1 AND q.status='skipped'
             )
             ORDER BY created_at DESC,id DESC LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    &run_id,
                    limit.clamp(1, 200) as i64,
                    offset.unwrap_or(0) as i64
                ],
                |row| {
                    let evidence: String = row.get(11)?;
                    Ok(AiFieldChange {
                        id: row.get(0)?,
                        run_id: row.get(1)?,
                        contact_id: row.get(2)?,
                        review_id: row.get(3)?,
                        email: row.get(4)?,
                        field_name: row.get(5)?,
                        old_value: row.get(6)?,
                        new_value: row.get(7)?,
                        change_kind: row.get(8)?,
                        status: row.get(9)?,
                        confidence: row.get(10)?,
                        evidence: serde_json::from_str(&evidence).unwrap_or_default(),
                        created_at: row.get(12)?,
                        resolved_at: row.get(13)?,
                        rolled_back: row.get::<_, i64>(14)? != 0,
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(AiFieldChangeList {
            changes: rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
            total,
        })
    })
    .await
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct AiBatchLog {
    id: String,
    run_id: String,
    taxonomy_version: Option<String>,
    provider: String,
    model: String,
    status: String,
    contact_count: usize,
    dispatched_at: String,
    completed_at: Option<String>,
    system_prompt: String,
    user_prompt: String,
    raw_response: Option<String>,
    error: Option<String>,
    attempt: usize,
    origin_batch_id: Option<String>,
    usage: serde_json::Value,
    input_bytes: usize,
}

#[tauri::command]
pub(crate) async fn list_ai_run_batches(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<Vec<AiBatchLog>, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let mut statement = db
            .prepare(
                "SELECT b.id,b.run_id,v.semantic_version,b.provider,b.model,b.status,b.contact_count,
                    b.dispatched_at,b.completed_at,b.system_prompt,b.user_prompt,b.raw_response,b.error,
                    b.attempt,b.origin_batch_id,b.usage_json,b.input_bytes
             FROM ai_run_batches b
             LEFT JOIN taxonomy_versions v ON v.id=b.taxonomy_version_id
             WHERE b.run_id=?1 ORDER BY b.dispatched_at,b.rowid",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![run_id], |row| {
                Ok(AiBatchLog {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    taxonomy_version: row.get(2)?,
                    provider: row.get(3)?,
                    model: row.get(4)?,
                    status: row.get(5)?,
                    contact_count: row.get::<_, i64>(6)? as usize,
                    dispatched_at: row.get(7)?,
                    completed_at: row.get(8)?,
                    system_prompt: row.get(9)?,
                    user_prompt: row.get(10)?,
                    raw_response: row.get(11)?,
                    error: row.get(12)?,
                    attempt: row.get::<_, i64>(13)? as usize,
                    origin_batch_id: row.get(14)?,
                    usage: serde_json::from_str(&row.get::<_, String>(15)?)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    input_bytes: row.get::<_, i64>(16)? as usize,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(rows)
    })
    .await
}
