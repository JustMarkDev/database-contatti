use crate::ai_batch::{BatchContactItem, call_provider_batch_unified};
use crate::ai_runs::UnifiedResult;
use crate::benchmark::{
    BenchmarkPrediction, BenchmarkReport, ReviewedCase, evaluate, reviewed_corpus,
};
use crate::taxonomy::load_active_taxonomy;
use crate::{TaxonomyOutcome, new_contact_id, open_db};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BenchmarkRunView {
    id: String,
    provider: String,
    model: String,
    taxonomy_version: String,
    status: String,
    report: Option<BenchmarkReport>,
    started_at: String,
    finished_at: Option<String>,
    total: usize,
    processed: usize,
}

#[derive(Debug)]
pub(crate) struct OwnedPrediction {
    category: Option<String>,
    subcategory: Option<String>,
    outcome: String,
    automatic_fill: bool,
    review_requested: bool,
    escalated: bool,
    latency_ms: f64,
    prompt_context_chars: usize,
}

impl OwnedPrediction {
    pub(crate) fn borrowed(&self) -> BenchmarkPrediction<'_> {
        BenchmarkPrediction {
            category: self.category.as_deref(),
            subcategory: self.subcategory.as_deref(),
            outcome: &self.outcome,
            automatic_fill: self.automatic_fill,
            review_requested: self.review_requested,
            escalated: self.escalated,
            latency_ms: self.latency_ms,
            prompt_context_chars: self.prompt_context_chars,
        }
    }
}

fn outcome_name(outcome: &TaxonomyOutcome) -> &'static str {
    match outcome {
        TaxonomyOutcome::Proposal => "proposal",
        TaxonomyOutcome::Conflict => "conflict",
        TaxonomyOutcome::TaxonomyGap => "taxonomy_gap",
        TaxonomyOutcome::Insufficient => "insufficient",
    }
}

fn item_for_case(case: &ReviewedCase) -> BatchContactItem {
    BatchContactItem {
        id: case.id.to_owned(),
        country: case.country.to_owned(),
        source_context: serde_json::json!([{
            "source_id": "reviewed_benchmark",
            "fields": {
                "Evidence": case.evidence,
                "ContactKind": case.contact_kind
            }
        }])
        .to_string(),
        previous_errors: Vec::new(),
        current_category: None,
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn item_for_case_for_tests(case: &ReviewedCase) -> BatchContactItem {
    item_for_case(case)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn prediction_from_unified_for_tests(
    unified: &UnifiedResult,
    item: &BatchContactItem,
    latency_ms: f64,
    display_names: &HashMap<(String, Option<String>, String), String>,
    escalated: bool,
) -> OwnedPrediction {
    prediction_from_unified(unified, item, latency_ms, display_names, escalated)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn invalid_prediction_for_tests(
    item: &BatchContactItem,
    outcome: &str,
    latency_ms: f64,
) -> OwnedPrediction {
    invalid_prediction(item, outcome, latency_ms)
}

fn prediction_from_unified(
    unified: &UnifiedResult,
    item: &BatchContactItem,
    latency_ms: f64,
    display_names: &HashMap<(String, Option<String>, String), String>,
    escalated: bool,
) -> OwnedPrediction {
    let candidate = unified.taxonomy.candidates.first();
    let category = candidate.and_then(|candidate| {
        display_names
            .get(&("category".to_owned(), None, candidate.category_key.clone()))
            .cloned()
    });
    let subcategory = candidate.and_then(|candidate| {
        candidate.subcategory_key.as_ref().and_then(|key| {
            display_names
                .get(&(
                    "subcategory".to_owned(),
                    Some(candidate.category_key.clone()),
                    key.clone(),
                ))
                .cloned()
        })
    });
    let outcome = outcome_name(&unified.taxonomy.outcome).to_owned();
    OwnedPrediction {
        category,
        subcategory,
        automatic_fill: outcome == "proposal",
        review_requested: outcome == "taxonomy_gap",
        escalated,
        outcome,
        latency_ms,
        prompt_context_chars: item.source_context.len(),
    }
}

fn invalid_prediction(item: &BatchContactItem, outcome: &str, latency_ms: f64) -> OwnedPrediction {
    OwnedPrediction {
        category: None,
        subcategory: None,
        outcome: outcome.to_owned(),
        automatic_fill: false,
        review_requested: false,
        escalated: true,
        latency_ms,
        prompt_context_chars: item.source_context.len(),
    }
}

fn report_for(predictions: &[OwnedPrediction]) -> BenchmarkReport {
    let borrowed = predictions
        .iter()
        .map(OwnedPrediction::borrowed)
        .collect::<Vec<_>>();
    evaluate(&borrowed)
}

fn control_status(db: &Connection, run_id: &str) -> Result<Option<String>, String> {
    db.query_row(
        "SELECT status FROM classification_benchmark_controls WHERE benchmark_run_id=?1",
        params![run_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|error| error.to_string())
}

fn request_control(db: &Connection, run_id: &str, status: &str) -> Result<(), String> {
    let base_status: String = db
        .query_row(
            "SELECT status FROM classification_benchmark_runs WHERE id=?1",
            params![run_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Benchmark non trovato".to_owned())?;
    if base_status != "running" {
        return Err("Il benchmark non è in esecuzione".to_owned());
    }
    db.execute(
        "INSERT INTO classification_benchmark_controls(benchmark_run_id,status,requested_at)
         VALUES(?1,?2,?3)
         ON CONFLICT(benchmark_run_id) DO UPDATE SET status=excluded.status,requested_at=excluded.requested_at",
        params![run_id, status, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn saved_predictions_for_run(
    db: &Connection,
    run_id: &str,
    cases: &[ReviewedCase],
    display_names: &HashMap<(String, Option<String>, String), String>,
) -> Result<(HashSet<String>, Vec<OwnedPrediction>), String> {
    let mut saved = HashMap::new();
    let mut statement = db
        .prepare(
            "SELECT case_id,predicted_json,validation_errors,latency_ms
             FROM classification_benchmark_results WHERE benchmark_run_id=?1",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (case_id, predicted_json, errors, latency_ms) =
            row.map_err(|error| error.to_string())?;
        saved.insert(case_id, (predicted_json, errors, latency_ms as f64));
    }

    let mut processed = HashSet::new();
    let mut predictions = Vec::new();
    for case in cases {
        let Some((predicted_json, errors, latency_ms)) = saved.get(case.id) else {
            break;
        };
        let item = item_for_case(case);
        let prediction = match predicted_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<UnifiedResult>(json).ok())
        {
            Some(unified) => {
                prediction_from_unified(&unified, &item, *latency_ms, display_names, false)
            }
            None => invalid_prediction(
                &item,
                if errors == "[]" { "missing" } else { "invalid" },
                *latency_ms,
            ),
        };
        processed.insert(case.id.to_owned());
        predictions.push(prediction);
    }
    Ok((processed, predictions))
}

fn load_view(db: &Connection, run_id: &str) -> Result<BenchmarkRunView, String> {
    let row = db
        .query_row(
            "SELECT b.id,b.provider,b.model,v.semantic_version,b.status,b.report_json,
                    b.started_at,b.finished_at,b.total_cases,b.processed_cases,c.status
             FROM classification_benchmark_runs b
             LEFT JOIN taxonomy_versions v ON v.id=b.taxonomy_version_id
             LEFT JOIN classification_benchmark_controls c ON c.benchmark_run_id=b.id
             WHERE b.id=?1",
            params![run_id],
            |row| {
                let report_json: Option<String> = row.get(5)?;
                let report = report_json.and_then(|json| serde_json::from_str(&json).ok());
                let stored_total = row.get::<_, i64>(8)?.max(0) as usize;
                let stored_processed = row.get::<_, i64>(9)?.max(0) as usize;
                Ok(BenchmarkRunView {
                    id: row.get(0)?,
                    provider: row.get(1)?,
                    model: row.get(2)?,
                    taxonomy_version: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    status: row.get::<_, Option<String>>(10)?.unwrap_or(row.get(4)?),
                    report: report.clone(),
                    started_at: row.get(6)?,
                    finished_at: row.get(7)?,
                    total: if stored_total == 0 {
                        report.as_ref().map(|value| value.cases).unwrap_or_default()
                    } else {
                        stored_total
                    },
                    processed: if stored_processed == 0 {
                        report.as_ref().map(|value| value.cases).unwrap_or_default()
                    } else {
                        stored_processed
                    },
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Benchmark non trovato".to_owned())?;
    Ok(row)
}

fn update_progress(
    db: &Connection,
    run_id: &str,
    processed: usize,
    report: &BenchmarkReport,
) -> Result<(), String> {
    db.execute(
        "UPDATE classification_benchmark_runs
         SET processed_cases=?1,report_json=?2 WHERE id=?3",
        params![
            processed as i64,
            serde_json::to_string(report).map_err(|e| e.to_string())?,
            run_id
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn finish_cancelled(db: &Connection, run_id: &str) -> Result<(), String> {
    db.execute(
        "UPDATE classification_benchmark_runs
         SET status='failed',finished_at=?1 WHERE id=?2 AND status='running'",
        params![chrono::Utc::now().to_rfc3339(), run_id],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn process_benchmark(
    app: tauri::AppHandle,
    run_id: &str,
) -> Result<BenchmarkRunView, String> {
    let (case_limit, selection) = {
        let db = open_db(&app)?;
        let settings: String = db
            .query_row(
                "SELECT settings_json FROM classification_benchmark_runs WHERE id=?1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let settings: serde_json::Value = serde_json::from_str(&settings).unwrap_or_default();
        let selection: crate::ai_selection::ProviderSelection = settings
            .get("selection")
            .cloned()
            .and_then(|value| {
                serde_json::from_value::<crate::ai_selection::ProviderSelection>(value).ok()
            })
            .unwrap_or_default()
            .normalize()?;
        (
            settings
                .get("case_limit")
                .and_then(|value| value.as_u64())
                .map(|value| value as usize),
            selection,
        )
    };
    let corpus = reviewed_corpus();
    let limit = case_limit.unwrap_or(corpus.len()).clamp(1, corpus.len());
    let cases = &corpus[..limit];
    let (taxonomy, labels, it_gazetteer, ch_gazetteer, display_names) = {
        let db = open_db(&app)?;
        let taxonomy = load_active_taxonomy(&db)?;
        let display_names = crate::ai_registry::load_taxonomy_display_names(&db, taxonomy.id)?;
        let labels = taxonomy
            .categories
            .iter()
            .map(|(category, subcategories)| {
                let category_label = display_names
                    .get(&("category".to_owned(), None, category.clone()))
                    .cloned()
                    .unwrap_or_else(|| category.clone());
                let subs = subcategories
                    .iter()
                    .map(|sub| {
                        display_names
                            .get(&(
                                "subcategory".to_owned(),
                                Some(category.clone()),
                                sub.clone(),
                            ))
                            .cloned()
                            .unwrap_or_else(|| sub.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{category_label}: {subs}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        (
            taxonomy,
            labels,
            crate::gazetteer::catalog(&db, "Italia")?,
            crate::gazetteer::catalog(&db, "Svizzera").unwrap_or_default(),
            display_names,
        )
    };
    let inference_provider = crate::ai_registry::build_generative_provider(&selection)?;
    let typesafe = if selection.provider == crate::ai_selection::PROVIDER_CASCADE {
        let db = open_db(&app)?;
        Some(crate::ai_registry::build_typesafe_client(
            &db, &selection, 600,
        )?)
    } else {
        None
    };
    let (processed_ids, mut predictions) = {
        let db = open_db(&app)?;
        saved_predictions_for_run(&db, run_id, cases, &display_names)?
    };
    let pending_cases = cases
        .iter()
        .filter(|case| !processed_ids.contains(case.id))
        .collect::<Vec<_>>();

    for chunk in pending_cases.chunks(32) {
        let control = {
            let db = open_db(&app)?;
            control_status(&db, run_id)?
        };
        match control.as_deref() {
            Some("cancelled") => {
                let db = open_db(&app)?;
                finish_cancelled(&db, run_id)?;
                return load_view(&db, run_id);
            }
            Some("paused") => {
                let db = open_db(&app)?;
                return load_view(&db, run_id);
            }
            Some("pausing") => {
                let db = open_db(&app)?;
                db.execute(
                    "UPDATE classification_benchmark_controls SET status='paused' WHERE benchmark_run_id=?1",
                    params![run_id],
                )
                .map_err(|error| error.to_string())?;
                return load_view(&db, run_id);
            }
            _ => {}
        }
        let items = chunk
            .iter()
            .map(|case| item_for_case(case))
            .collect::<Vec<_>>();
        let started = Instant::now();
        let response = if let Some(typesafe) = typesafe.as_ref() {
            crate::ai_cascade::call_cascade_batch_unified(
                typesafe,
                inference_provider.as_ref(),
                &items,
                &it_gazetteer,
                &ch_gazetteer,
                selection.spend_policy(),
                crate::ai_cascade::CascadeTaxonomy {
                    taxonomy: &taxonomy.categories,
                    display_names: &display_names,
                    labels: &labels,
                },
            )
            .await
        } else {
            call_provider_batch_unified(
                inference_provider.as_ref(),
                &items,
                &taxonomy.categories,
                &it_gazetteer,
                &ch_gazetteer,
                &labels,
            )
            .await
        };
        let latency_per_case = started.elapsed().as_secs_f64() * 1_000.0 / chunk.len() as f64;
        match response {
            Ok((raw_response, results)) => {
                let escalated_ids = serde_json::from_str::<serde_json::Value>(&raw_response)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/cascade/escalated")
                            .and_then(|ids| ids.as_array().cloned())
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect::<HashSet<_>>();
                let db = open_db(&app)?;
                for (case, item) in chunk.iter().zip(items.iter()) {
                    let result = results.iter().find(|result| result.contact_id == case.id);
                    let case_raw_response = result
                        .and_then(|result| result.raw_item_json.clone())
                        .unwrap_or_else(|| raw_response.clone());
                    let escalated = escalated_ids.contains(case.id);
                    let (prediction, predicted_json, errors) = match result {
                        Some(result) => match &result.result {
                            Ok(unified) => (
                                prediction_from_unified(
                                    unified,
                                    item,
                                    latency_per_case,
                                    &display_names,
                                    escalated,
                                ),
                                serde_json::to_string(unified).ok(),
                                Vec::new(),
                            ),
                            Err(errors) => (
                                invalid_prediction(item, "invalid", latency_per_case),
                                None,
                                errors.clone(),
                            ),
                        },
                        None => (
                            invalid_prediction(item, "missing", latency_per_case),
                            None,
                            vec!["batch.missing_from_output".to_owned()],
                        ),
                    };
                    db.execute(
                        "INSERT OR REPLACE INTO classification_benchmark_results(
                            benchmark_run_id,case_id,expected_json,predicted_json,evidence_json,
                            raw_response,validation_errors,latency_ms
                         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                        params![
                            run_id,
                            case.id,
                            serde_json::to_string(case).map_err(|error| error.to_string())?,
                            predicted_json,
                            serde_json::json!([case.evidence]).to_string(),
                            case_raw_response,
                            serde_json::to_string(&errors).map_err(|error| error.to_string())?,
                            latency_per_case.round() as i64,
                        ],
                    )
                    .map_err(|error| error.to_string())?;
                    predictions.push(prediction);
                }
                let report = report_for(&predictions);
                update_progress(&db, run_id, predictions.len(), &report)?;
            }
            Err(error) => {
                let db = open_db(&app)?;
                db.execute(
                    "UPDATE classification_benchmark_runs
                     SET status='failed',report_json=?1,finished_at=?2 WHERE id=?3",
                    params![
                        serde_json::json!({ "error": error.message }).to_string(),
                        chrono::Utc::now().to_rfc3339(),
                        run_id
                    ],
                )
                .map_err(|database_error| database_error.to_string())?;
                return Err(error.message);
            }
        }
    }

    let control = {
        let db = open_db(&app)?;
        control_status(&db, run_id)?
    };
    if matches!(control.as_deref(), Some("cancelled")) {
        let db = open_db(&app)?;
        finish_cancelled(&db, run_id)?;
        return load_view(&db, run_id);
    }
    if matches!(control.as_deref(), Some("pausing" | "paused")) {
        let db = open_db(&app)?;
        if control.as_deref() == Some("pausing") {
            db.execute(
                "UPDATE classification_benchmark_controls SET status='paused' WHERE benchmark_run_id=?1",
                params![run_id],
            )
            .map_err(|error| error.to_string())?;
        }
        return load_view(&db, run_id);
    }

    let report = report_for(&predictions);
    let db = open_db(&app)?;
    db.execute(
        "UPDATE classification_benchmark_runs
         SET status='completed',processed_cases=?1,report_json=?2,finished_at=?3 WHERE id=?4",
        params![
            predictions.len() as i64,
            serde_json::to_string(&report).map_err(|error| error.to_string())?,
            chrono::Utc::now().to_rfc3339(),
            run_id
        ],
    )
    .map_err(|error| error.to_string())?;
    load_view(&db, run_id)
}

#[tauri::command]
pub(crate) async fn run_classification_benchmark(
    app: tauri::AppHandle,
    case_limit: Option<usize>,
    selection: Option<crate::ai_selection::ProviderSelection>,
) -> Result<BenchmarkRunView, String> {
    let selection =
        crate::ai_commands::prepare_selection(&app, selection.unwrap_or_default()).await?;
    let provider_id = selection.provider.clone();
    let model = selection.run_model_label();
    let corpus_len = reviewed_corpus().len();
    let total = case_limit.unwrap_or(corpus_len).clamp(1, corpus_len);
    let run_id = new_contact_id();
    let started_at = chrono::Utc::now().to_rfc3339();
    let db = open_db(&app)?;
    let running: i64 = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM classification_benchmark_runs WHERE status='running')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if running != 0 {
        return Err("Esiste già un benchmark in esecuzione o in pausa".to_owned());
    }
    let taxonomy = load_active_taxonomy(&db)?;
    db.execute(
        "INSERT INTO classification_benchmark_runs(
            id,provider,model,taxonomy_version_id,settings_json,status,started_at,total_cases,processed_cases
         ) VALUES(?1,?2,?3,?4,?5,'running',?6,?7,0)",
        params![
            run_id,
            provider_id,
            model,
            taxonomy.id,
            serde_json::json!({ "case_limit": case_limit, "selection": selection }).to_string(),
            started_at,
            total as i64,
        ],
    )
    .map_err(|error| error.to_string())?;
    drop(db);
    process_benchmark(app, &run_id).await
}

#[tauri::command]
pub(crate) async fn pause_classification_benchmark(
    app: tauri::AppHandle,
    benchmark_id: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        request_control(&db, &benchmark_id, "pausing")
    })
    .await
}

#[tauri::command]
pub(crate) async fn cancel_classification_benchmark(
    app: tauri::AppHandle,
    benchmark_id: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let was_paused = control_status(&db, &benchmark_id)?.as_deref() == Some("paused");
        request_control(&db, &benchmark_id, "cancelled")?;
        if was_paused {
            finish_cancelled(&db, &benchmark_id)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn resume_classification_benchmark(
    app: tauri::AppHandle,
    benchmark_id: String,
) -> Result<BenchmarkRunView, String> {
    {
        let db = open_db(&app)?;
        if control_status(&db, &benchmark_id)?.as_deref() != Some("paused") {
            return Err("Il benchmark non è in pausa".to_owned());
        }
        db.execute(
            "DELETE FROM classification_benchmark_controls WHERE benchmark_run_id=?1",
            params![benchmark_id],
        )
        .map_err(|error| error.to_string())?;
    }
    process_benchmark(app, &benchmark_id).await
}

#[tauri::command]
pub(crate) async fn list_classification_benchmarks(
    app: tauri::AppHandle,
    limit: Option<usize>,
) -> Result<Vec<BenchmarkRunView>, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let ids = {
            let mut statement = db
                .prepare(
                    "SELECT id FROM classification_benchmark_runs
                 ORDER BY started_at DESC LIMIT ?1",
                )
                .map_err(|error| error.to_string())?;

            statement
                .query_map(params![limit.unwrap_or(10).clamp(1, 50) as i64], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        ids.iter().map(|id| load_view(&db, id)).collect()
    })
    .await
}
