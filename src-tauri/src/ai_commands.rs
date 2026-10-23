use crate::ai_runs::{count_deterministic_catalog_matches, run_deterministic_catalog_pass};
use crate::{
    AI_LEASE_SECONDS, APP_SESSION_ID, AiClassificationRequest, AiEnrichmentReport, AiProgress,
    load_ai_candidate_ids, new_contact_id, normalize_orphaned_ai_runs, open_db, process_ai_run,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use tauri::Emitter;

fn empty_run_report() -> AiEnrichmentReport {
    AiEnrichmentReport {
        processed: 0,
        updated: 0,
        skipped: 0,
        errors: Vec::new(),
        misclassified: Vec::new(),
    }
}

fn mark_ai_run_failed(app: &tauri::AppHandle, run_id: &str, error: &str) {
    if let Ok(db) = open_db(app) {
        let _ = db.execute(
            "UPDATE ai_classification_runs
             SET status='failed', interruption_reason=?1,
                 owner_session=NULL, lease_expires_at=NULL
             WHERE id=?2 AND status IN ('running','pausing')",
            params![error, run_id],
        );
    }
    let _ = app.emit(
        "ai-progress",
        AiProgress {
            phase: "failed".to_owned(),
            processed: 0,
            total: 0,
            updated: 0,
            skipped: 0,
            error: Some(error.to_owned()),
            message: Some("Arricchimento fallito".to_owned()),
        },
    );
}

const MAX_BUSY_RETRIES: u32 = 20;
const BUSY_RETRY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

fn is_transient_database_busy(error: &str) -> bool {
    error.starts_with("Database occupato")
        || error.contains("database is locked")
        || error.contains("database is busy")
}

fn spawn_ai_run(app: tauri::AppHandle, run_id: String, mut request: AiClassificationRequest) {
    tauri::async_runtime::spawn(async move {
        match prepare_selection(&app, request.selection.clone()).await {
            Ok(selection) => {
                request.selection = selection;
                let model = request.selection.run_model_label();
                if let Ok(db) = open_db(&app) {
                    let _ = db.execute(
                        "UPDATE ai_classification_runs SET model=?1 WHERE id=?2",
                        params![model, run_id],
                    );
                }
                // A busy pool or a locked database is transient: resume from the
                // persisted queue instead of failing a run that may be hours old.
                let mut busy_retries = 0;
                loop {
                    match process_ai_run(
                        app.clone(),
                        run_id.clone(),
                        request.clone(),
                        model.clone(),
                    )
                    .await
                    {
                        Err(error)
                            if is_transient_database_busy(&error)
                                && busy_retries < MAX_BUSY_RETRIES =>
                        {
                            busy_retries += 1;
                            tokio::time::sleep(BUSY_RETRY_WAIT).await;
                        }
                        Err(error) => {
                            mark_ai_run_failed(&app, &run_id, &error);
                            break;
                        }
                        Ok(_) => break,
                    }
                }
            }
            Err(error) => mark_ai_run_failed(&app, &run_id, &error),
        }
    });
}

pub(crate) fn spawn_ai_run_for_resume(
    app: tauri::AppHandle,
    run_id: String,
    request: AiClassificationRequest,
) {
    spawn_ai_run(app, run_id, request);
}

pub(crate) async fn prepare_selection(
    app: &tauri::AppHandle,
    mut selection: crate::ai_selection::ProviderSelection,
) -> Result<crate::ai_selection::ProviderSelection, String> {
    selection = selection.normalize()?;
    match selection.provider.as_str() {
        crate::ai_selection::PROVIDER_OPENCODE => {
            let status = crate::ai_opencode::detect_opencode().await?;
            if !status.available {
                return Err(format!(
                    "OpenCode non disponibile: {}",
                    status.error.unwrap_or_default()
                ));
            }
            let model = crate::ai_selection::strip_provider_prefix(
                crate::ai_selection::PROVIDER_OPENCODE,
                &selection.model,
            );
            if model.is_empty() {
                selection.model = status.selected_model;
            } else {
                selection.model = model;
            }
            if selection.effort.is_none() {
                selection.effort = status.selected_effort;
            }
        }
        crate::ai_selection::PROVIDER_CASCADE => {
            let (app_check, selection_check) = (app.clone(), selection.clone());
            crate::database::blocking(move || {
                let db = open_db(&app_check)?;
                crate::ai_registry::build_typesafe_client(&db, &selection_check, 60).map(|_| ())
            })
            .await?;
            let mut escalation = crate::ai_selection::ProviderSelection {
                provider: selection
                    .escalation_provider
                    .clone()
                    .unwrap_or_else(|| crate::ai_selection::PROVIDER_OPENCODE.to_owned()),
                model: selection.escalation_model.clone().unwrap_or_default(),
                effort: selection.escalation_effort.clone(),
                ..crate::ai_selection::ProviderSelection::default()
            };
            escalation = Box::pin(prepare_selection(app, escalation)).await?;
            selection.escalation_provider = Some(escalation.provider);
            selection.escalation_model = Some(escalation.model);
            selection.escalation_effort = escalation.effort;
        }
        _ => {}
    }
    selection.normalize()
}

#[tauri::command]
pub(crate) async fn start_geographic_enrichment(
    app: tauri::AppHandle,
    mut request: AiClassificationRequest,
) -> Result<AiEnrichmentReport, String> {
    request.mode = "geography".to_owned();
    start_ai_classification(app, request).await
}

/// Creates the run row and its persisted queue. Blocking: call through
/// `database::blocking`.
fn create_ai_run(
    app: &tauri::AppHandle,
    run_id: &str,
    request: &AiClassificationRequest,
    provider: &str,
    model: &str,
) -> Result<usize, String> {
    let candidates = {
        let db = open_db(app)?;
        load_ai_candidate_ids(&db, &request.mode)?
    };
    if matches!(request.mode.as_str(), "follow_up" | "deterministic_audit") && candidates.is_empty()
    {
        return Err(if request.mode == "follow_up" {
            "Nessun contatto deterministico irrisolto da processare".to_owned()
        } else {
            "Nessun risultato deterministico da verificare con IA".to_owned()
        });
    }
    {
        let mut db = open_db(app)?;
        let now = chrono::Utc::now().timestamp();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        normalize_orphaned_ai_runs(&tx, now)?;
        let active: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM ai_classification_runs
                 WHERE status IN ('running','pausing') AND lease_expires_at>=?1",
                params![now],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if active != 0 {
            return Err("È già attiva una classificazione IA".to_owned());
        }
        let paused: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM ai_classification_runs WHERE status='paused'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if paused != 0 {
            return Err(
                "Esiste una classificazione in pausa: riprendila o annullarla prima di avviarne una nuova"
                    .to_owned(),
            );
        }
        let prompt_version = if request.mode == "geography" {
            "geography-v2"
        } else if provider == crate::ai_selection::PROVIDER_CASCADE {
            "cascade-jev-v1"
        } else {
            "taxonomy-v8-batch"
        };
        let deterministic_catalog_id: Option<String> = tx
            .query_row(
                "SELECT id FROM deterministic_rule_catalogs WHERE status='active'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at,request_json,owner_session,lease_expires_at,provider,deterministic_catalog_id
             ) VALUES (?1,?2,?3,?4,'running',?5,?6,?7,?8,?9,?10,?11)",
            params![run_id, model, crate::ai_runs::AI_PROFILE_LABEL, prompt_version, candidates.len() as i64, chrono::Utc::now().to_rfc3339(),
                serde_json::to_string(&request).map_err(|e| e.to_string())?,
                APP_SESSION_ID.as_str(), now + AI_LEASE_SECONDS, provider, deterministic_catalog_id],
        ).map_err(|e| e.to_string())?;
        for (ordinal, contact_id) in candidates.iter().enumerate() {
            tx.execute(
                "INSERT INTO ai_run_candidates(run_id,contact_id,ordinal) VALUES (?1,?2,?3)",
                params![run_id, contact_id, ordinal as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(candidates.len())
}

#[tauri::command]
pub(crate) async fn start_ai_classification(
    app: tauri::AppHandle,
    mut request: AiClassificationRequest,
) -> Result<AiEnrichmentReport, String> {
    request.thinking = true;
    request.timeout_seconds = request.timeout_seconds.max(600);
    // Normalize only: OpenCode/Pi probes run inside the spawned worker so this invoke returns
    // as soon as the run row exists and the UI can show progress.
    request.selection = request.selection.normalize()?;
    let provider = request.selection.provider.clone();
    let model = request.selection.run_model_label();
    if provider == crate::ai_selection::PROVIDER_CASCADE {
        let app = app.clone();
        let selection = request.selection.clone();
        crate::database::blocking(move || {
            let db = open_db(&app)?;
            crate::ai_registry::build_typesafe_client(&db, &selection, 60).map(|_| ())
        })
        .await?;
    } else {
        request.selection = prepare_selection(&app, request.selection).await?;
    }
    let run_id = new_contact_id();
    let queued = {
        let app = app.clone();
        let run_id = run_id.clone();
        let request = request.clone();
        crate::database::blocking(move || create_ai_run(&app, &run_id, &request, &provider, &model))
            .await?
    };
    let _ = app.emit(
        "ai-progress",
        AiProgress {
            phase: "ai".to_owned(),
            processed: 0,
            total: queued,
            updated: 0,
            skipped: 0,
            error: None,
            message: Some(format!("Arricchimento avviato · {queued} contatti in coda")),
        },
    );
    spawn_ai_run(app, run_id, request);
    Ok(empty_run_report())
}

pub(crate) fn process_deterministic_only_run(
    app: &tauri::AppHandle,
    run_id: &str,
) -> Result<AiEnrichmentReport, String> {
    let (total, mut report, model) = {
        let db = open_db(app)?;
        db.query_row(
            "SELECT total,processed,updated,skipped,model
             FROM ai_classification_runs WHERE id=?1",
            params![run_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as usize,
                    AiEnrichmentReport {
                        processed: row.get::<_, i64>(1)? as usize,
                        updated: row.get::<_, i64>(2)? as usize,
                        skipped: row.get::<_, i64>(3)? as usize,
                        errors: Vec::new(),
                        misclassified: Vec::new(),
                    },
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?
    };
    run_deterministic_catalog_pass(app, run_id, &mut report, total)?;

    let mut db = open_db(app)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let pending: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM ai_run_candidates WHERE run_id=?1 AND status='pending'",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if pending > 0 {
        tx.execute(
            "UPDATE ai_run_candidates SET status='skipped',processed_at=?1
             WHERE run_id=?2 AND status='pending'",
            params![chrono::Utc::now().to_rfc3339(), run_id],
        )
        .map_err(|error| error.to_string())?;
    }
    tx.execute(
        "UPDATE ai_classification_runs
         SET status='completed',finished_at=?1,
             owner_session=NULL,lease_expires_at=NULL
         WHERE id=?2",
        params![chrono::Utc::now().to_rfc3339(), run_id],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    let _ = app.emit(
        "ai-progress",
        crate::AiProgress {
            phase: "completed".to_owned(),
            processed: report.processed,
            total,
            updated: report.updated,
            skipped: report.skipped,
            error: None,
            message: Some(format!(
                "Passo 1 completato · {} aggiornati · {} saltati · catalogo {}",
                report.updated, report.skipped, model
            )),
        },
    );
    Ok(report)
}

#[tauri::command]
pub(crate) async fn start_deterministic_classification(
    app: tauri::AppHandle,
    expected_match_count: Option<usize>,
) -> Result<AiEnrichmentReport, String> {
    crate::database::blocking(move || run_deterministic_classification(&app, expected_match_count))
        .await
}

fn run_deterministic_classification(
    app: &tauri::AppHandle,
    expected_match_count: Option<usize>,
) -> Result<AiEnrichmentReport, String> {
    let run_id = new_contact_id();
    {
        let candidates = {
            let db = open_db(app)?;
            load_ai_candidate_ids(&db, "full")?
        };
        let mut db = open_db(app)?;
        let now = chrono::Utc::now().timestamp();
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        normalize_orphaned_ai_runs(&tx, now)?;
        let active: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM ai_classification_runs
                 WHERE status IN ('running','pausing','paused')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if active != 0 {
            return Err(
                "Concludi o annulla l'esecuzione attiva o in pausa prima del passo deterministico"
                    .to_owned(),
            );
        }
        let (catalog_id, catalog_version): (String, String) = tx
            .query_row(
                "SELECT id,semantic_version FROM deterministic_rule_catalogs WHERE status='active'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| "Nessun catalogo deterministico attivo".to_owned())?;
        let deterministic_total = match expected_match_count {
            Some(count) => count,
            None => count_deterministic_catalog_matches(&tx, &catalog_id)?,
        };
        let model = format!("deterministic/{catalog_version}");
        let request = serde_json::json!({
            "mode": "deterministic_only",
            "deterministic_only": true,
            "catalog_id": catalog_id,
        });
        tx.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at,request_json,
                owner_session,lease_expires_at,provider,deterministic_catalog_id,mode
             ) VALUES(?1,?2,?3,?4,'running',?5,?6,?7,?8,?9,'deterministic',?10,?11)",
            params![
                run_id,
                model,
                crate::ai_runs::AI_PROFILE_LABEL,
                format!("deterministic-{catalog_version}"),
                deterministic_total as i64,
                chrono::Utc::now().to_rfc3339(),
                request.to_string(),
                APP_SESSION_ID.as_str(),
                now + AI_LEASE_SECONDS,
                catalog_id,
                "deterministic_only",
            ],
        )
        .map_err(|error| error.to_string())?;
        for (ordinal, contact_id) in candidates.iter().enumerate() {
            tx.execute(
                "INSERT INTO ai_run_candidates(run_id,contact_id,ordinal) VALUES(?1,?2,?3)",
                params![run_id, contact_id, ordinal as i64],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
    }

    process_deterministic_only_run(app, &run_id)
}

#[tauri::command]
pub(crate) async fn retry_failed_contacts(
    app: tauri::AppHandle,
    origin_run_id: String,
    mut request: AiClassificationRequest,
) -> Result<AiEnrichmentReport, String> {
    request.force = true;
    request.thinking = true;
    request.timeout_seconds = request.timeout_seconds.max(600);
    request.selection = prepare_selection(&app, request.selection).await?;
    let provider = request.selection.provider.clone();
    let model = request.selection.run_model_label();
    let run_id = new_contact_id();
    let request = {
        let (app, run_id) = (app.clone(), run_id.clone());
        let mut request = request;
        crate::database::blocking(move || {
            let mut db = open_db(&app)?;
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| error.to_string())?;
            let origin_mode: String = tx.query_row(
                "SELECT mode FROM ai_classification_runs WHERE id=?1 AND status='completed_with_errors'",
                params![origin_run_id],
                |row| row.get(0),
            ).map_err(|_| "L'esecuzione di origine non è terminata con errori".to_owned())?;
            request.mode = origin_mode;
            let active: i64 = tx.query_row(
                "SELECT COUNT(*) FROM ai_classification_runs WHERE status IN ('running','pausing','paused')",
                [], |row| row.get(0),
            ).map_err(|error| error.to_string())?;
            if active != 0 {
                return Err("Concludi o annulla l'esecuzione attiva o in pausa prima di riprovare".to_owned());
            }
            let failed = {
                let mut statement = tx.prepare(
                    "SELECT contact_id FROM ai_run_candidates WHERE run_id=?1 AND status='error' ORDER BY ordinal",
                ).map_err(|error| error.to_string())?;

                statement
                    .query_map(params![origin_run_id], |row| row.get::<_, String>(0))
                    .map_err(|error| error.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?
            };
            if failed.is_empty() {
                return Err("L'esecuzione non contiene contatti non riusciti".to_owned());
            }
            let catalog_id: Option<String> = tx
                .query_row(
                    "SELECT deterministic_catalog_id FROM ai_classification_runs WHERE id=?1",
                    params![origin_run_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| error.to_string())?
                .flatten();
            tx.execute(
                "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at,request_json,owner_session,lease_expires_at,provider,parent_run_id,deterministic_catalog_id)
                 VALUES(?1,?2,?3,'taxonomy-v9-batch','running',?4,?5,?6,?7,?8,?9,?10,?11)",
                params![run_id, model, crate::ai_runs::AI_PROFILE_LABEL, failed.len() as i64, chrono::Utc::now().to_rfc3339(), serde_json::to_string(&request).map_err(|error| error.to_string())?, APP_SESSION_ID.as_str(), chrono::Utc::now().timestamp() + AI_LEASE_SECONDS, provider, origin_run_id, catalog_id],
            )
            .map_err(|error| error.to_string())?;
            for (ordinal, contact_id) in failed.iter().enumerate() {
                tx.execute(
                    "INSERT INTO ai_run_candidates(run_id,contact_id,ordinal) VALUES(?1,?2,?3)",
                    params![run_id, contact_id, ordinal as i64],
                )
                .map_err(|error| error.to_string())?;
            }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(request)
        })
        .await?
    };
    spawn_ai_run(app, run_id, request);
    Ok(empty_run_report())
}

#[tauri::command]
pub(crate) async fn start_deterministic_recheck(
    app: tauri::AppHandle,
    mut request: AiClassificationRequest,
) -> Result<AiEnrichmentReport, String> {
    request.force = true;
    request.mode = "full".to_owned();
    request.thinking = true;
    request.timeout_seconds = request.timeout_seconds.max(600);
    request.selection = prepare_selection(&app, request.selection).await?;
    let provider = request.selection.provider.clone();
    let model = request.selection.run_model_label();
    let run_id = new_contact_id();
    let request = {
        let (app, run_id) = (app.clone(), run_id.clone());
        crate::database::blocking(move || {
            let mut db = open_db(&app)?;
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| error.to_string())?;
            let active: i64 = tx.query_row("SELECT COUNT(*) FROM ai_classification_runs WHERE status IN ('running','pausing','paused')", [], |row| row.get(0)).map_err(|error| error.to_string())?;
            if active != 0 {
                return Err("Concludi o annulla l'esecuzione attiva o in pausa prima del riesame".to_owned());
            }
            let catalog_id: String = tx
                .query_row(
                    "SELECT id FROM deterministic_rule_catalogs WHERE status='active'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| "Nessun catalogo deterministico attivo".to_owned())?;
            let contact_ids = {
                let mut statement = tx.prepare("SELECT contact_id FROM deterministic_recheck_queue WHERE catalog_id=?1 ORDER BY created_at,contact_id").map_err(|error| error.to_string())?;

                statement
                    .query_map(params![catalog_id], |row| row.get::<_, String>(0))
                    .map_err(|error| error.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?
            };
            if contact_ids.is_empty() {
                return Err("Nessun contatto richiede il riesame deterministico".to_owned());
            }
            let now = chrono::Utc::now().timestamp();
            tx.execute(
                "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at,request_json,owner_session,lease_expires_at,provider,deterministic_catalog_id)
                 VALUES(?1,?2,?3,'taxonomy-v9-batch','running',?4,?5,?6,?7,?8,?9,?10)",
                params![run_id, model, crate::ai_runs::AI_PROFILE_LABEL, contact_ids.len() as i64, chrono::Utc::now().to_rfc3339(), serde_json::to_string(&request).map_err(|error| error.to_string())?, APP_SESSION_ID.as_str(), now + AI_LEASE_SECONDS, provider, catalog_id],
            ).map_err(|error| error.to_string())?;
            for (ordinal, contact_id) in contact_ids.into_iter().enumerate() {
                tx.execute(
                    "INSERT INTO ai_run_candidates(run_id,contact_id,ordinal) VALUES(?1,?2,?3)",
                    params![run_id, contact_id, ordinal as i64],
                )
                .map_err(|error| error.to_string())?;
            }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(request)
        })
        .await?
    };
    spawn_ai_run(app, run_id, request);
    Ok(empty_run_report())
}

#[cfg(test)]
mod busy_retry_tests {
    #[test]
    fn only_pool_and_lock_contention_is_retried() {
        assert!(super::is_transient_database_busy(
            "Database occupato da altre operazioni: riprovare tra poco"
        ));
        assert!(super::is_transient_database_busy("database is locked"));
        assert!(!super::is_transient_database_busy(
            "no such table: taxonomy_change_candidate_contacts"
        ));
        assert!(!super::is_transient_database_busy(
            "Controllo dell'esecuzione IA perso; elaborazione arrestata"
        ));
    }
}
