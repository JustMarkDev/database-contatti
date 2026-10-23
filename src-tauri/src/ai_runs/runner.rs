//! AI run processing: batch dispatch, provider calls, and the run loop.

use super::*;

struct BatchDispatch<'a> {
    batch_id: &'a str,
    run_id: &'a str,
    taxonomy_version_id: i64,
    provider: &'a str,
    model: &'a str,
    contact_count: usize,
    attempt: i64,
    origin_batch_id: Option<&'a str>,
}

fn record_batch_dispatch(db: &Connection, dispatch: BatchDispatch<'_>) -> Result<(), String> {
    let BatchDispatch {
        batch_id,
        run_id,
        taxonomy_version_id,
        provider,
        model,
        contact_count,
        attempt,
        origin_batch_id,
    } = dispatch;
    db.execute(
        "INSERT INTO ai_run_batches(
            id,run_id,taxonomy_version_id,provider,model,status,contact_count,dispatched_at,attempt,origin_batch_id
         ) VALUES(?1,?2,?3,?4,?5,'dispatched',?6,?7,?8,?9)",
        params![
            batch_id,
            run_id,
            taxonomy_version_id,
            provider,
            model,
            contact_count as i64,
            chrono::Utc::now().to_rfc3339(),
            attempt,
            origin_batch_id,
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn finish_batch_dispatch(
    db: &Connection,
    batch_id: &str,
    system_prompt: &str,
    user_prompt: &str,
    result: &Result<(String, Vec<crate::ai_batch::BatchItemResult<UnifiedResult>>), ProviderError>,
) -> Result<(), String> {
    let (status, raw_response, error) = match result {
        Ok((raw_response, _)) => ("completed", Some(raw_response.as_str()), None),
        Err(error) => ("failed", None, Some(error.message.as_str())),
    };
    db.execute(
        "UPDATE ai_run_batches SET
            status=?1,completed_at=?2,system_prompt=?3,user_prompt=?4,raw_response=?5,error=?6,input_bytes=?8
         WHERE id=?7",
        params![
            status,
            chrono::Utc::now().to_rfc3339(),
            system_prompt,
            user_prompt,
            raw_response,
            error,
            batch_id,
            system_prompt.len().saturating_add(user_prompt.len()) as i64,
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Prefer per-batch Jev usage from the cascade envelope; attach Muse Spark usage when present.
fn cascade_or_provider_usage(
    call_res: &Result<
        (String, Vec<crate::ai_batch::BatchItemResult<UnifiedResult>>),
        ProviderError,
    >,
    provider: &dyn InferenceProvider,
) -> Option<serde_json::Value> {
    let mut usage = serde_json::Map::new();
    if let Ok((raw, _)) = call_res
        && let Ok(envelope) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(jev) = envelope.pointer("/cascade/jev_usage")
    {
        usage.insert("jev".to_owned(), jev.clone());
    }
    if let Some(generative) = provider.take_last_usage() {
        // Legacy wire key: persisted run batches still read `muse`.
        usage.insert("muse".to_owned(), generative);
    }
    if usage.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(usage))
    }
}

/// What a batched provider run is classifying.
struct BatchedRun<'a> {
    model: &'a str,
    request: &'a AiClassificationRequest,
    run_id: &'a str,
    total: usize,
}

async fn process_ai_run_provider_batched(
    app: &tauri::AppHandle,
    provider: Arc<dyn InferenceProvider>,
    typesafe: Option<crate::ai_typesafe::TypeSafeClient>,
    selection: &crate::ai_selection::ProviderSelection,
    display_names: &std::collections::HashMap<(String, Option<String>, String), String>,
    report: &mut AiEnrichmentReport,
    run: BatchedRun<'_>,
) -> Result<AiEnrichmentReport, String> {
    let BatchedRun {
        model,
        request,
        run_id,
        total,
    } = run;
    // One Jev pass-1 call per batch: keep equal to the System One contact batch.
    const BATCH_SIZE: usize = crate::ai_typesafe::JEV_CONTACT_BATCH;
    const WORKERS: usize = 1;
    const MAX_BATCH_INPUT_BYTES: usize = 512 * 1024;
    /// After this many auto-waits without a successful batch, pause for Riprendi.
    const MAX_AUTO_LIMIT_WAITS: u32 = 24;
    let limit = (WORKERS * BATCH_SIZE) as i64;
    let mut consecutive_limit_waits = 0_u32;

    let (it_gaz, ch_gaz) = {
        let db = open_db(app)?;
        let it = crate::gazetteer::catalog(&db, "Italia")?;
        let ch = crate::gazetteer::catalog(&db, "Svizzera").unwrap_or_default();
        (it, ch)
    };

    // Cascade ladder: Jev → Muse Spark (first pass) → Muse Spark (retry, single-contact xhigh).
    let cascade_high = if selection.provider == crate::ai_selection::PROVIDER_CASCADE
        && selection.generative_provider_id() == crate::ai_selection::PROVIDER_OPENCODE
    {
        Some(crate::ai_registry::build_generative_provider_with_effort(
            selection,
            selection.generative_retry_effort(),
        )?)
    } else {
        None
    };
    let cascade_retry = cascade_high.is_some();

    let load_taxonomy_for_dispatch =
        || -> Result<(crate::taxonomy::TaxonomyVersion, String), String> {
            let db = open_db(app)?;
            let tax = load_active_taxonomy(&db)?;
            let labels = tax
                .categories
                .iter()
                .map(|(c, s)| {
                    let category_label = display_names
                        .get(&("category".to_owned(), None, c.clone()))
                        .cloned()
                        .unwrap_or_else(|| c.clone());
                    let subs: Vec<String> = s
                        .iter()
                        .filter(|sub| sub.as_str() != "contattopersonale")
                        .map(|sub| {
                            display_names
                                .get(&("subcategory".to_owned(), Some(c.clone()), sub.clone()))
                                .cloned()
                                .unwrap_or_else(|| sub.clone())
                        })
                        .collect();
                    format!("{category_label}: {}", subs.join(", "))
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok((tax, labels))
        };

    loop {
        let (taxonomy_ver, labels) = load_taxonomy_for_dispatch()?;
        let candidate_ids: Vec<String> = {
            let db = open_db(app)?;
            let (status, owner): (String, Option<String>) = db
                .query_row(
                    "SELECT status,owner_session FROM ai_classification_runs WHERE id=?1",
                    params![run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            if status == "cancelled" {
                db.execute(
                    "UPDATE ai_classification_runs SET owner_session=NULL,lease_expires_at=NULL
                     WHERE id=?1 AND status='cancelled'",
                    params![run_id],
                )
                .map_err(|e| e.to_string())?;
                return Ok(report.clone());
            }
            if status == "pausing" || status == "paused" {
                if status == "pausing" {
                    db.execute(
                        "UPDATE ai_classification_runs SET status='paused',owner_session=NULL,lease_expires_at=NULL
                         WHERE id=?1 AND status='pausing'",
                        params![run_id],
                    )
                    .map_err(|e| e.to_string())?;
                }
                return Ok(report.clone());
            }
            if matches!(
                status.as_str(),
                "failed" | "completed" | "completed_with_errors"
            ) {
                return Ok(report.clone());
            }
            if owner.as_deref() != Some(APP_SESSION_ID.as_str()) {
                return Err("Controllo dell'esecuzione IA perso; elaborazione arrestata".to_owned());
            }
            if !renew_ai_lease(
                &db,
                run_id,
                APP_SESSION_ID.as_str(),
                chrono::Utc::now().timestamp(),
                (request.timeout_seconds as i64 + 60) * limit,
            )? {
                return Err("Controllo dell'esecuzione IA perso; elaborazione arrestata".to_owned());
            }
            let mut stmt = db
                .prepare(
                    "SELECT contact_id FROM ai_run_candidates
                     WHERE run_id=?1 AND status='pending'
                       AND contact_id NOT IN (SELECT contact_id FROM ai_run_retries WHERE run_id=?1)
                     ORDER BY ordinal LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![run_id, limit], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };

        if candidate_ids.is_empty() {
            break;
        }

        struct ItemData {
            contact: ContactRecord,
            source_context: String,
            catalog_version: String,
            input_hash: String,
        }

        let mut to_infer: Vec<ItemData> = Vec::new();
        {
            let mut db = open_db(app)?;
            for contact_id in candidate_ids {
                let contact = load_ai_candidate(&db, &contact_id)?;
                let source_context = classification_source_context(&db, &contact.id)?;
                if source_context.len().saturating_add(32 * 1024) > MAX_BATCH_INPUT_BYTES {
                    let message = format!(
                        "{} · input troppo grande ({} byte); nessun dato è stato troncato",
                        contact.email,
                        source_context.len()
                    );
                    db.execute(
                        "UPDATE ai_run_candidates SET status='error',failure_kind='oversized_input',error=?1,processed_at=?2,attempt_count=attempt_count+1 WHERE run_id=?3 AND contact_id=?4",
                        params![message, chrono::Utc::now().to_rfc3339(), run_id, contact.id],
                    ).map_err(|error| error.to_string())?;
                    db.execute(
                        "UPDATE ai_classification_runs SET processed=processed+1,errors=errors+1 WHERE id=?1 AND owner_session=?2",
                        params![run_id, APP_SESSION_ID.as_str()],
                    ).map_err(|error| error.to_string())?;
                    report.processed += 1;
                    report.errors.push(message);
                    continue;
                }
                let context_size = adaptive_context_size(request.context_size, &source_context);
                let catalog_version = taxonomy_ver.semantic_version.clone();
                let input_hash = classification_hash(
                    &contact,
                    model,
                    request.thinking,
                    context_size,
                    &request.mode,
                    &catalog_version,
                    &source_context,
                );
                if !request.force {
                    let existing: (Option<String>, Option<i64>, String) = db
                        .query_row(
                            "SELECT classification_hash,taxonomy_version_id,classification_state
                             FROM contacts WHERE id=?1",
                            params![contact.id],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .unwrap_or((None, None, "unprocessed".to_owned()));
                    if existing.0.as_deref() == Some(&input_hash)
                        && existing.1 == Some(taxonomy_ver.id)
                        && existing.2 != "unprocessed"
                    {
                        report.processed += 1;
                        report.skipped += 1;
                        // Reuse the prep connection: a second checkout here used to
                        // hold 2/3 pool slots and starve the AI page refreshes.
                        let tx = db
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .map_err(|e| e.to_string())?;
                        tx.execute(
                            "UPDATE ai_run_candidates SET status='skipped',processed_at=?1 WHERE run_id=?2 AND contact_id=?3",
                            params![chrono::Utc::now().to_rfc3339(), run_id, contact.id],
                        ).map_err(|e| e.to_string())?;
                        let changed = tx
                            .execute(
                                "UPDATE ai_classification_runs SET processed=processed+1,skipped=skipped+1
                             WHERE id=?1 AND owner_session=?2 AND status IN ('running','pausing')",
                                params![run_id, APP_SESSION_ID.as_str()],
                            )
                            .map_err(|e| e.to_string())?;
                        if changed != 1 {
                            return Err(
                                "Salto del contatto rifiutato: controllo dell'esecuzione perso"
                                    .to_owned(),
                            );
                        }
                        tx.commit().map_err(|e| e.to_string())?;
                        let _ = app.emit(
                            "ai-progress",
                            AiProgress {
                                phase: "ai".to_owned(),
                                processed: report.processed,
                                total,
                                updated: report.updated,
                                skipped: report.skipped,
                                error: None,
                                message: Some(format!(
                                    "{} · saltato (già classificato)",
                                    contact.email
                                )),
                            },
                        );
                        continue;
                    }
                }
                to_infer.push(ItemData {
                    contact,
                    source_context,
                    catalog_version,
                    input_hash,
                });
            }
        }

        if to_infer.is_empty() {
            continue;
        }

        let mut chunks: Vec<Vec<ItemData>> = Vec::new();
        let mut current = Vec::new();
        let mut current_bytes: usize = 32 * 1024;
        for item in to_infer {
            let item_bytes = item.source_context.len().saturating_add(1024);
            if !current.is_empty()
                && (current.len() == BATCH_SIZE
                    || current_bytes.saturating_add(item_bytes) > MAX_BATCH_INPUT_BYTES)
            {
                chunks.push(std::mem::take(&mut current));
                current_bytes = 32 * 1024;
            }
            current_bytes = current_bytes.saturating_add(item_bytes);
            current.push(item);
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        let dispatched_batches = chunks
            .into_iter()
            .map(|chunk| {
                let batch_id = new_contact_id();
                let db = open_db(app)?;
                record_batch_dispatch(
                    &db,
                    BatchDispatch {
                        batch_id: &batch_id,
                        run_id,
                        taxonomy_version_id: taxonomy_ver.id,
                        provider: &selection.provider,
                        model,
                        contact_count: chunk.len(),
                        attempt: 1,
                        origin_batch_id: None,
                    },
                )?;
                Ok((batch_id, chunk))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let batch_futures = dispatched_batches.into_iter().map(|(batch_id, chunk)| {
            let provider = provider.clone();
            let typesafe = typesafe.clone();
            let selection = selection.clone();
            let display_names = display_names.clone();
            let taxonomy = taxonomy_ver.categories.clone();
            let it_gaz = it_gaz.clone();
            let ch_gaz = ch_gaz.clone();
            let labels = labels.clone();
            async move {
                let batch_items: Vec<BatchContactItem> = chunk
                    .iter()
                    .map(|item| BatchContactItem {
                        id: item.contact.id.clone(),
                        country: item.contact.paese.clone(),
                        source_context: item.source_context.clone(),
                        previous_errors: vec![],
                        current_category: Some(item.contact.category.clone()),
                    })
                    .collect();
                let call_res = if let Some(typesafe) = typesafe.as_ref() {
                    crate::ai_cascade::call_cascade_batch_unified(
                        typesafe,
                        provider.as_ref(),
                        &batch_items,
                        &it_gaz,
                        &ch_gaz,
                        selection.spend_policy(),
                        crate::ai_cascade::CascadeTaxonomy {
                            taxonomy: &taxonomy,
                            display_names: &display_names,
                            labels: &labels,
                        },
                    )
                    .await
                } else {
                    call_provider_batch_unified(
                        provider.as_ref(),
                        &batch_items,
                        &taxonomy,
                        &it_gaz,
                        &ch_gaz,
                        &labels,
                    )
                    .await
                };
                (batch_id, chunk, call_res)
            }
        });

        let completed_batches = futures::future::join_all(batch_futures).await;

        for (batch_id, chunk, call_res) in completed_batches {
            let prompt_items = chunk
                .iter()
                .map(|item| BatchContactItem {
                    id: item.contact.id.clone(),
                    country: item.contact.paese.clone(),
                    source_context: item.source_context.clone(),
                    previous_errors: Vec::new(),
                    current_category: Some(item.contact.category.clone()),
                })
                .collect::<Vec<_>>();
            let (system_prompt, user_prompt) =
                crate::ai_batch::build_batch_unified_prompt(&prompt_items, &labels);
            {
                let db = open_db(app)?;
                finish_batch_dispatch(&db, &batch_id, &system_prompt, &user_prompt, &call_res)?;
                if let Some(usage) = cascade_or_provider_usage(&call_res, provider.as_ref()) {
                    db.execute(
                        "UPDATE ai_run_batches SET usage_json=?1 WHERE id=?2",
                        params![usage.to_string(), batch_id],
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
            match call_res {
                Ok((raw_envelope, results)) => {
                    consecutive_limit_waits = 0;
                    for item in chunk {
                        let matched = results.iter().find(|r| r.contact_id == item.contact.id);
                        let mut db = open_db(app)?;
                        let tx = db
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .map_err(|e| e.to_string())?;

                        match matched {
                            Some(item_res) if item_res.result.is_ok() => {
                                let unified = item_res.result.as_ref().unwrap();
                                let tax_outcome = apply_taxonomy_result(
                                    &tx,
                                    &item.contact,
                                    &unified.taxonomy,
                                    model,
                                    &item.input_hash,
                                    &item.catalog_version,
                                    run_id,
                                )?;
                                let geo_outcome = apply_geography_result(
                                    &tx,
                                    &item.contact,
                                    &unified.geography,
                                    run_id,
                                )?;
                                let overall_outcome =
                                    if tax_outcome == "updated" || geo_outcome == "updated" {
                                        "updated"
                                    } else if tax_outcome == "review" || geo_outcome == "review" {
                                        "review"
                                    } else {
                                        "skipped"
                                    };

                                let source = item_res.source.as_deref();
                                let source_label = inference_source_label(source);
                                let (up_delta, rev_delta, skip_delta) = match overall_outcome {
                                    "updated" => {
                                        report.updated += 1;
                                        (1, 0, 0)
                                    }
                                    "review" => (0, 1, 0),
                                    _ => {
                                        report.skipped += 1;
                                        (0, 0, 1)
                                    }
                                };
                                let progress_message = (overall_outcome == "review").then(|| {
                                    format!(
                                        "{} · lacuna tassonomica proposta · {}",
                                        item.contact.email, source_label
                                    )
                                });

                                if let Some(source) = source {
                                    tag_contact_field_changes_with_source(
                                        &tx,
                                        run_id,
                                        &item.contact.id,
                                        source,
                                    )?;
                                }

                                report.processed += 1;
                                let runtime_metadata = serde_json::json!({
                                    "provider": source.unwrap_or(provider.provider_id()),
                                    "batch_size": BATCH_SIZE,
                                    "effort": match source {
                                        Some("jev") => "n/a",
                                        Some("muse-minimal") => selection
                                            .generative_effort()
                                            .unwrap_or("minimal"),
                                        Some("muse-high") => {
                                            crate::ai_selection::DEFAULT_OPENCODE_RETRY_EFFORT
                                        }
                                        _ => "low",
                                    }
                                })
                                .to_string();

                                tx.execute(
                                    "INSERT INTO ai_run_artifacts(id,run_id,contact_id,input_hash,system_prompt,user_prompt,raw_response,retry_response,validated_result,validation_errors,runtime_metadata,catalog_version,model_identity,model_digest,settings_json,mode,created_at)
                                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                                    params![
                                        new_contact_id(), run_id, item.contact.id, item.input_hash, "", "",
                                        item_res.raw_item_json.as_deref().unwrap_or(&raw_envelope),
                                        Option::<String>::None,
                                        serde_json::to_string(unified).ok(),
                                        "[]",
                                        runtime_metadata, item.catalog_version,
                                        model, Option::<String>::None,
                                        serde_json::json!({"thinking":request.thinking,"mode":request.mode}).to_string(),
                                        request.mode, chrono::Utc::now().to_rfc3339()
                                    ],
                                ).map_err(|e| e.to_string())?;

                                tx.execute(
                                    "UPDATE ai_run_candidates SET status='processed',error=NULL,processed_at=?1 WHERE run_id=?2 AND contact_id=?3",
                                    params![chrono::Utc::now().to_rfc3339(), run_id, item.contact.id],
                                ).map_err(|e| e.to_string())?;
                                tx.execute(
                                    "DELETE FROM deterministic_recheck_queue WHERE contact_id=?1",
                                    params![item.contact.id],
                                )
                                .map_err(|error| error.to_string())?;

                                tx.execute(
                                    "UPDATE ai_classification_runs SET processed=processed+1,updated=updated+?1,reviews_created=reviews_created+?2,skipped=skipped+?3
                                     WHERE id=?4 AND owner_session=?5",
                                    params![up_delta, rev_delta, skip_delta, run_id, APP_SESSION_ID.as_str()],
                                ).map_err(|e| e.to_string())?;

                                tx.commit().map_err(|e| e.to_string())?;
                                // Counters only for routine apply/skip: detailed field rows
                                // feed the UI. Per-contact messages freeze the live log.
                                let _ = app.emit(
                                    "ai-progress",
                                    AiProgress {
                                        phase: "ai".to_owned(),
                                        processed: report.processed,
                                        total,
                                        updated: report.updated,
                                        skipped: report.skipped,
                                        error: None,
                                        message: progress_message,
                                    },
                                );
                            }
                            _ => {
                                let errors = matched
                                    .and_then(|m| m.result.as_ref().err().cloned())
                                    .unwrap_or_else(|| {
                                        vec!["batch.missing_from_output".to_string()]
                                    });
                                let errors_json =
                                    serde_json::to_string(&errors).unwrap_or_default();
                                let raw_item = matched
                                    .and_then(|m| m.raw_item_json.clone())
                                    .unwrap_or_else(|| raw_envelope.clone());

                                tx.execute(
                                    "INSERT OR REPLACE INTO ai_run_retries(run_id,contact_id,validation_errors,raw_response,created_at,origin_batch_id,provider,model,attempt,failure_kind)
                                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,'structural')",
                                    params![run_id, item.contact.id, errors_json, raw_item, chrono::Utc::now().to_rfc3339(), batch_id, provider.provider_id(), model],
                                ).map_err(|e| e.to_string())?;

                                tx.commit().map_err(|e| e.to_string())?;
                                let _ = app.emit(
                                    "ai-progress",
                                    AiProgress {
                                        phase: "ai".to_owned(),
                                        processed: report.processed,
                                        total,
                                        updated: report.updated,
                                        skipped: report.skipped,
                                        error: None,
                                        message: Some(format!(
                                            "{} · {}",
                                            item.contact.email,
                                            if cascade_retry {
                                                "coda Muse Spark (xhigh) · Muse Spark senza output"
                                            } else {
                                                "pianificato per riesame ad alto sforzo"
                                            }
                                        )),
                                    },
                                );
                            }
                        }
                    }
                }
                Err(err) => {
                    if err.kind == ProviderErrorKind::Quota {
                        match handle_provider_limit(
                            app,
                            run_id,
                            request,
                            &err,
                            &mut consecutive_limit_waits,
                            MAX_AUTO_LIMIT_WAITS,
                            report,
                        )
                        .await?
                        {
                            ProviderLimitAction::Continue => break,
                            ProviderLimitAction::Paused => return Ok(report.clone()),
                        }
                    }
                    // Cascade returns Err only when Jev itself failed. Do not park
                    // those contacts on the Muse Spark retry queue.
                    if typesafe.is_some() {
                        return Err(err.message.clone());
                    }
                    for item in chunk {
                        let mut db = open_db(app)?;
                        let tx = db
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .map_err(|e| e.to_string())?;
                        let errors_json =
                            serde_json::to_string(&[format!("provider_error: {}", err.message)])
                                .unwrap_or_default();
                        tx.execute(
                            "INSERT OR REPLACE INTO ai_run_retries(run_id,contact_id,validation_errors,raw_response,created_at,origin_batch_id,provider,model,attempt,failure_kind)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9)",
                            params![run_id, item.contact.id, errors_json, Option::<String>::None, chrono::Utc::now().to_rfc3339(), batch_id, provider.provider_id(), model, if err.kind == ProviderErrorKind::Timeout { "timeout" } else { "transport" }],
                        ).map_err(|e| e.to_string())?;
                        tx.commit().map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        let queued: i64 = open_db(app)?
            .query_row(
                "SELECT COUNT(*) FROM ai_run_retries WHERE run_id=?1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if queued >= BATCH_SIZE as i64 {
            break;
        }
    }

    loop {
        let (taxonomy_ver, labels) = load_taxonomy_for_dispatch()?;
        struct RetryData {
            contact: ContactRecord,
            source_context: String,
            validation_errors: Vec<String>,
            raw_response: Option<String>,
            catalog_version: String,
            input_hash: String,
            origin_batch_id: Option<String>,
            next_batch_size: usize,
            attempt: i64,
        }

        let retry_rows: Vec<RetryData> = {
            let db = open_db(app)?;
            let (status, owner): (String, Option<String>) = db
                .query_row(
                    "SELECT status,owner_session FROM ai_classification_runs WHERE id=?1",
                    params![run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| e.to_string())?;
            if status == "cancelled" {
                db.execute("UPDATE ai_classification_runs SET owner_session=NULL,lease_expires_at=NULL WHERE id=?1 AND status='cancelled'", params![run_id]).map_err(|error| error.to_string())?;
                return Ok(report.clone());
            }
            if status == "pausing" || status == "paused" {
                if status == "pausing" {
                    db.execute("UPDATE ai_classification_runs SET status='paused',owner_session=NULL,lease_expires_at=NULL WHERE id=?1 AND status='pausing'", params![run_id]).map_err(|error| error.to_string())?;
                }
                return Ok(report.clone());
            }
            if matches!(
                status.as_str(),
                "failed" | "completed" | "completed_with_errors"
            ) {
                return Ok(report.clone());
            }
            if owner.as_deref() != Some(APP_SESSION_ID.as_str()) {
                return Err("Controllo dell'esecuzione IA perso; elaborazione arrestata".to_owned());
            }
            let mut stmt = db
                .prepare(
                    "SELECT contact_id,validation_errors,raw_response,origin_batch_id,next_batch_size,attempt FROM ai_run_retries
                     WHERE run_id=?1 ORDER BY next_batch_size DESC,created_at LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![run_id, limit], |row| {
                    let cid: String = row.get(0)?;
                    let errs_str: String = row.get(1)?;
                    let raw_resp: Option<String> = row.get(2)?;
                    let origin_batch_id: Option<String> = row.get(3)?;
                    let next_batch_size = row.get::<_, i64>(4)? as usize;
                    let attempt: i64 = row.get(5)?;
                    let errs: Vec<String> = serde_json::from_str(&errs_str).unwrap_or_default();
                    Ok((
                        cid,
                        errs,
                        raw_resp,
                        origin_batch_id,
                        next_batch_size,
                        attempt,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let items = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;

            let mut out = Vec::new();
            for (cid, errs, raw, origin_batch_id, next_batch_size, attempt) in items {
                let contact = load_ai_candidate(&db, &cid)?;
                let source_context = classification_source_context(&db, &contact.id)?;
                let context_size = adaptive_context_size(request.context_size, &source_context);
                let catalog_version = taxonomy_ver.semantic_version.clone();
                let input_hash = classification_hash(
                    &contact,
                    model,
                    request.thinking,
                    context_size,
                    &request.mode,
                    &catalog_version,
                    &source_context,
                );
                out.push(RetryData {
                    contact,
                    source_context,
                    validation_errors: errs,
                    raw_response: raw,
                    catalog_version,
                    input_hash,
                    origin_batch_id,
                    next_batch_size,
                    attempt,
                });
            }
            out
        };

        if retry_rows.is_empty() {
            break;
        }

        let mut chunks: Vec<Vec<RetryData>> = Vec::new();
        let mut cursor = 0;
        let retry_batch_cap = if cascade_retry { 4 } else { BATCH_SIZE };
        while cursor < retry_rows.len() {
            let desired = retry_rows[cursor].next_batch_size.clamp(1, retry_batch_cap);
            let end = (cursor + desired).min(retry_rows.len());
            chunks.push(
                retry_rows[cursor..end]
                    .iter()
                    .map(|item| RetryData {
                        contact: item.contact.clone(),
                        source_context: item.source_context.clone(),
                        validation_errors: item.validation_errors.clone(),
                        raw_response: item.raw_response.clone(),
                        catalog_version: item.catalog_version.clone(),
                        input_hash: item.input_hash.clone(),
                        origin_batch_id: item.origin_batch_id.clone(),
                        next_batch_size: item.next_batch_size,
                        attempt: item.attempt,
                    })
                    .collect(),
            );
            cursor = end;
        }
        let dispatched_batches = chunks
            .into_iter()
            .map(|chunk| {
                let batch_id = new_contact_id();
                let origin_batch_id = chunk
                    .first()
                    .and_then(|item| item.origin_batch_id.as_deref());
                let attempt = chunk.first().map(|item| item.attempt + 1).unwrap_or(2);
                let db = open_db(app)?;
                record_batch_dispatch(
                    &db,
                    BatchDispatch {
                        batch_id: &batch_id,
                        run_id,
                        taxonomy_version_id: taxonomy_ver.id,
                        provider: &selection.provider,
                        model,
                        contact_count: chunk.len(),
                        attempt,
                        origin_batch_id,
                    },
                )?;
                Ok((batch_id, chunk))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let retry_futures = dispatched_batches.into_iter().map(|(batch_id, chunk)| {
            let provider = provider.clone();
            let cascade_high = cascade_high.clone();
            let typesafe = typesafe.clone();
            let selection = selection.clone();
            let display_names = display_names.clone();
            let taxonomy = taxonomy_ver.categories.clone();
            let it_gaz = it_gaz.clone();
            let ch_gaz = ch_gaz.clone();
            let labels = labels.clone();
            async move {
                let batch_items: Vec<BatchContactItem> = chunk
                    .iter()
                    .map(|item| BatchContactItem {
                        id: item.contact.id.clone(),
                        country: item.contact.paese.clone(),
                        source_context: item.source_context.clone(),
                        previous_errors: item.validation_errors.clone(),
                        current_category: Some(item.contact.category.clone()),
                    })
                    .collect();
                // Cascade third rung: Muse Spark xhigh only (skip Jev). Other providers keep prior path.
                let call_res = if let Some(high) = cascade_high.as_ref() {
                    call_provider_batch_unified(
                        high.as_ref(),
                        &batch_items,
                        &taxonomy,
                        &it_gaz,
                        &ch_gaz,
                        &labels,
                    )
                    .await
                } else if let Some(typesafe) = typesafe.as_ref() {
                    crate::ai_cascade::call_cascade_batch_unified(
                        typesafe,
                        provider.as_ref(),
                        &batch_items,
                        &it_gaz,
                        &ch_gaz,
                        selection.spend_policy(),
                        crate::ai_cascade::CascadeTaxonomy {
                            taxonomy: &taxonomy,
                            display_names: &display_names,
                            labels: &labels,
                        },
                    )
                    .await
                } else {
                    call_provider_batch_unified(
                        provider.as_ref(),
                        &batch_items,
                        &taxonomy,
                        &it_gaz,
                        &ch_gaz,
                        &labels,
                    )
                    .await
                };
                (batch_id, chunk, call_res)
            }
        });

        let completed_retries = futures::future::join_all(retry_futures).await;

        for (batch_id, chunk, call_res) in completed_retries {
            let prompt_items = chunk
                .iter()
                .map(|item| BatchContactItem {
                    id: item.contact.id.clone(),
                    country: item.contact.paese.clone(),
                    source_context: item.source_context.clone(),
                    previous_errors: item.validation_errors.clone(),
                    current_category: Some(item.contact.category.clone()),
                })
                .collect::<Vec<_>>();
            let (system_prompt, user_prompt) =
                crate::ai_batch::build_batch_unified_prompt(&prompt_items, &labels);
            let usage_provider = cascade_high.as_ref().unwrap_or(&provider);
            {
                let db = open_db(app)?;
                finish_batch_dispatch(&db, &batch_id, &system_prompt, &user_prompt, &call_res)?;
                if let Some(usage) = cascade_or_provider_usage(&call_res, usage_provider.as_ref()) {
                    db.execute(
                        "UPDATE ai_run_batches SET usage_json=?1 WHERE id=?2",
                        params![usage.to_string(), batch_id],
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
            match call_res {
                Ok((raw_envelope, mut results)) => {
                    consecutive_limit_waits = 0;
                    if cascade_retry {
                        for item in &mut results {
                            item.source = Some("muse-high".to_owned());
                        }
                    }
                    if chunk.len() > 1 && results.iter().all(|item| item.result.is_err()) {
                        let current_size = chunk
                            .first()
                            .map(|item| item.next_batch_size)
                            .unwrap_or(BATCH_SIZE);
                        let next_size = match current_size {
                            32.. => 16,
                            16 => 8,
                            _ => 1,
                        };
                        let db = open_db(app)?;
                        for item in &chunk {
                            db.execute("UPDATE ai_run_retries SET next_batch_size=?1,attempt=attempt+1,failure_kind='structural' WHERE run_id=?2 AND contact_id=?3", params![next_size as i64, run_id, item.contact.id]).map_err(|error| error.to_string())?;
                        }
                        continue;
                    }
                    for item in chunk {
                        let matched = results.iter().find(|r| r.contact_id == item.contact.id);
                        let mut db = open_db(app)?;
                        let tx = db
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .map_err(|e| e.to_string())?;

                        tx.execute(
                            "DELETE FROM ai_run_retries WHERE run_id=?1 AND contact_id=?2",
                            params![run_id, item.contact.id],
                        )
                        .map_err(|e| e.to_string())?;

                        match matched {
                            Some(item_res) if item_res.result.is_ok() => {
                                let unified = item_res.result.as_ref().unwrap();
                                let tax_outcome = apply_taxonomy_result(
                                    &tx,
                                    &item.contact,
                                    &unified.taxonomy,
                                    model,
                                    &item.input_hash,
                                    &item.catalog_version,
                                    run_id,
                                )?;
                                let geo_outcome = apply_geography_result(
                                    &tx,
                                    &item.contact,
                                    &unified.geography,
                                    run_id,
                                )?;
                                let overall_outcome =
                                    if tax_outcome == "updated" || geo_outcome == "updated" {
                                        "updated"
                                    } else if tax_outcome == "review" || geo_outcome == "review" {
                                        "review"
                                    } else {
                                        "skipped"
                                    };

                                let source = item_res.source.as_deref().or(if cascade_retry {
                                    Some("muse-high")
                                } else {
                                    None
                                });
                                let source_label = inference_source_label(source);
                                let (up_delta, rev_delta, skip_delta) = match overall_outcome {
                                    "updated" => {
                                        report.updated += 1;
                                        (1, 0, 0)
                                    }
                                    "review" => (0, 1, 0),
                                    _ => {
                                        report.skipped += 1;
                                        (0, 0, 1)
                                    }
                                };
                                let progress_message = (overall_outcome == "review").then(|| {
                                    format!(
                                        "{} · lacuna tassonomica proposta · {}",
                                        item.contact.email, source_label
                                    )
                                });

                                if let Some(source) = source {
                                    tag_contact_field_changes_with_source(
                                        &tx,
                                        run_id,
                                        &item.contact.id,
                                        source,
                                    )?;
                                }

                                report.processed += 1;
                                let runtime_metadata = serde_json::json!({
                                    "provider": source.unwrap_or(usage_provider.provider_id()),
                                    "batch_size": BATCH_SIZE,
                                    "effort": match source {
                                        Some("jev") => "n/a",
                                        Some("muse-minimal") => selection
                                            .generative_effort()
                                            .unwrap_or("minimal"),
                                        Some("muse-high") => {
                                            crate::ai_selection::DEFAULT_OPENCODE_RETRY_EFFORT
                                        }
                                        _ => crate::ai_selection::DEFAULT_OPENCODE_ESCALATION_EFFORT,
                                    }
                                })
                                .to_string();

                                tx.execute(
                                    "INSERT INTO ai_run_artifacts(id,run_id,contact_id,input_hash,system_prompt,user_prompt,raw_response,retry_response,validated_result,validation_errors,runtime_metadata,catalog_version,model_identity,model_digest,settings_json,mode,created_at)
                                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                                    params![
                                        new_contact_id(), run_id, item.contact.id, item.input_hash, "", "",
                                        item.raw_response.as_deref().unwrap_or(""),
                                        item_res.raw_item_json.as_deref().unwrap_or(&raw_envelope),
                                        serde_json::to_string(unified).ok(),
                                        "[]",
                                        runtime_metadata, item.catalog_version,
                                        model, Option::<String>::None,
                                        serde_json::json!({"thinking":request.thinking,"mode":request.mode}).to_string(),
                                        request.mode, chrono::Utc::now().to_rfc3339()
                                    ],
                                ).map_err(|e| e.to_string())?;

                                tx.execute(
                                    "UPDATE ai_run_candidates SET status='processed',error=NULL,processed_at=?1 WHERE run_id=?2 AND contact_id=?3",
                                    params![chrono::Utc::now().to_rfc3339(), run_id, item.contact.id],
                                ).map_err(|e| e.to_string())?;
                                tx.execute(
                                    "DELETE FROM deterministic_recheck_queue WHERE contact_id=?1",
                                    params![item.contact.id],
                                )
                                .map_err(|error| error.to_string())?;

                                tx.execute(
                                    "UPDATE ai_classification_runs SET processed=processed+1,updated=updated+?1,reviews_created=reviews_created+?2,skipped=skipped+?3
                                     WHERE id=?4 AND owner_session=?5",
                                    params![up_delta, rev_delta, skip_delta, run_id, APP_SESSION_ID.as_str()],
                                ).map_err(|e| e.to_string())?;

                                tx.commit().map_err(|e| e.to_string())?;
                                let _ = app.emit(
                                    "ai-progress",
                                    AiProgress {
                                        phase: "ai".to_owned(),
                                        processed: report.processed,
                                        total,
                                        updated: report.updated,
                                        skipped: report.skipped,
                                        error: None,
                                        message: progress_message,
                                    },
                                );
                            }
                            _ => {
                                let errors = matched
                                    .and_then(|m| m.result.as_ref().err().cloned())
                                    .unwrap_or_else(|| {
                                        vec!["batch.missing_from_retry_output".to_string()]
                                    });
                                let err_str = errors.join(", ");
                                let error_msg = format!("{}: {}", item.contact.email, err_str);
                                report.errors.push(error_msg.clone());
                                report.processed += 1;

                                let runtime_metadata = serde_json::json!({
                                    "provider": usage_provider.provider_id(),
                                    "batch_size": BATCH_SIZE,
                                    "effort": if cascade_retry {
                                        crate::ai_selection::DEFAULT_OPENCODE_RETRY_EFFORT
                                    } else {
                                        crate::ai_selection::DEFAULT_OPENCODE_ESCALATION_EFFORT                                    }
                                })
                                .to_string();

                                tx.execute(
                                    "INSERT INTO ai_run_artifacts(id,run_id,contact_id,input_hash,system_prompt,user_prompt,raw_response,retry_response,validated_result,validation_errors,runtime_metadata,catalog_version,model_identity,model_digest,settings_json,mode,created_at)
                                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                                    params![
                                        new_contact_id(), run_id, item.contact.id, item.input_hash, "", "",
                                        item.raw_response.as_deref().unwrap_or(""),
                                        matched.and_then(|r| r.raw_item_json.as_deref()).unwrap_or(&raw_envelope),
                                        Option::<String>::None,
                                        serde_json::to_string(&errors).unwrap_or_default(),
                                        runtime_metadata, item.catalog_version,
                                        model, Option::<String>::None,
                                        serde_json::json!({"thinking":request.thinking,"mode":request.mode}).to_string(),
                                        request.mode, chrono::Utc::now().to_rfc3339()
                                    ],
                                ).map_err(|e| e.to_string())?;

                                tx.execute(
                                    "UPDATE ai_run_candidates SET status='error',failure_kind='validation',attempt_count=attempt_count+1,error=?1,processed_at=?2 WHERE run_id=?3 AND contact_id=?4",
                                    params![error_msg, chrono::Utc::now().to_rfc3339(), run_id, item.contact.id],
                                ).map_err(|e| e.to_string())?;

                                tx.execute(
                                    "UPDATE ai_classification_runs SET processed=processed+1,errors=errors+1
                                     WHERE id=?1 AND owner_session=?2",
                                    params![run_id, APP_SESSION_ID.as_str()],
                                ).map_err(|e| e.to_string())?;

                                tx.commit().map_err(|e| e.to_string())?;
                                let _ = app.emit(
                                    "ai-progress",
                                    AiProgress {
                                        phase: "ai".to_owned(),
                                        processed: report.processed,
                                        total,
                                        updated: report.updated,
                                        skipped: report.skipped,
                                        error: Some(error_msg.clone()),
                                        message: Some(format!(
                                            "{} · errore definitivo: {}",
                                            item.contact.email, err_str
                                        )),
                                    },
                                );
                            }
                        }
                    }
                }
                Err(err) => {
                    if err.kind == ProviderErrorKind::Quota {
                        match handle_provider_limit(
                            app,
                            run_id,
                            request,
                            &err,
                            &mut consecutive_limit_waits,
                            MAX_AUTO_LIMIT_WAITS,
                            report,
                        )
                        .await?
                        {
                            ProviderLimitAction::Continue => break,
                            ProviderLimitAction::Paused => return Ok(report.clone()),
                        }
                    }
                    if chunk.len() > 1 {
                        let current_size = chunk
                            .first()
                            .map(|item| item.next_batch_size)
                            .unwrap_or(BATCH_SIZE);
                        let next_size = match current_size {
                            32.. => 16,
                            16 => 8,
                            _ => 1,
                        };
                        let db = open_db(app)?;
                        for item in &chunk {
                            db.execute("UPDATE ai_run_retries SET next_batch_size=?1,attempt=attempt+1,failure_kind=?2 WHERE run_id=?3 AND contact_id=?4", params![next_size as i64, if err.kind == ProviderErrorKind::Timeout { "timeout" } else { "transport" }, run_id, item.contact.id]).map_err(|error| error.to_string())?;
                        }
                        continue;
                    }
                    for item in chunk {
                        let mut db = open_db(app)?;
                        let tx = db
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .map_err(|e| e.to_string())?;
                        tx.execute(
                            "DELETE FROM ai_run_retries WHERE run_id=?1 AND contact_id=?2",
                            params![run_id, item.contact.id],
                        )
                        .map_err(|e| e.to_string())?;
                        let error_msg = format!(
                            "{}: errore del fornitore dopo i tentativi: {}",
                            item.contact.email, err.message
                        );
                        report.errors.push(error_msg.clone());
                        report.processed += 1;

                        tx.execute(
                            "UPDATE ai_run_candidates SET status='error',failure_kind=?1,attempt_count=attempt_count+1,error=?2,processed_at=?3 WHERE run_id=?4 AND contact_id=?5",
                            params![if err.kind == ProviderErrorKind::Timeout { "timeout" } else { "transport" }, error_msg, chrono::Utc::now().to_rfc3339(), run_id, item.contact.id],
                        ).map_err(|e| e.to_string())?;

                        tx.execute(
                            "UPDATE ai_classification_runs SET processed=processed+1,errors=errors+1 WHERE id=?1 AND owner_session=?2",
                            params![run_id, APP_SESSION_ID.as_str()],
                        ).map_err(|e| e.to_string())?;

                        tx.commit().map_err(|e| e.to_string())?;
                    }
                }
            }
        }
    }

    let pending_initial: i64 = open_db(app)?
        .query_row(
            "SELECT COUNT(*) FROM ai_run_candidates WHERE run_id=?1 AND status='pending'",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if pending_initial > 0 {
        return Box::pin(process_ai_run_provider_batched(
            app,
            provider,
            typesafe,
            selection,
            display_names,
            report,
            BatchedRun {
                model,
                request,
                run_id,
                total,
            },
        ))
        .await;
    }

    let persisted_errors: i64 = open_db(app)?
        .query_row(
            "SELECT errors FROM ai_classification_runs WHERE id=?1",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let completion_status = if persisted_errors == 0 {
        "completed"
    } else {
        "completed_with_errors"
    };
    open_db(app)?
        .execute(
            "UPDATE ai_classification_runs SET status=?1,finished_at=?2,
             owner_session=NULL,lease_expires_at=NULL
         WHERE id=?3 AND status='running' AND owner_session=?4",
            params![
                completion_status,
                chrono::Utc::now().to_rfc3339(),
                run_id,
                APP_SESSION_ID.as_str()
            ],
        )
        .map_err(|e| e.to_string())?;
    {
        let db = open_db(app)?;
        let _ = trim_completed_ai_blobs(&db, run_id);
    }

    Ok(report.clone())
}

pub(crate) fn trim_completed_ai_blobs(db: &Connection, run_id: &str) -> Result<(), String> {
    db.execute(
        "UPDATE ai_run_artifacts
         SET system_prompt='', user_prompt='', raw_response=NULL, retry_response=NULL, operational_log=''
         WHERE run_id=?1",
        params![run_id],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE ai_run_batches
         SET system_prompt='', user_prompt='', raw_response=NULL
         WHERE run_id=?1",
        params![run_id],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) async fn process_ai_run(
    app: tauri::AppHandle,
    run_id: String,
    request: AiClassificationRequest,
    model: String,
) -> Result<AiEnrichmentReport, String> {
    let selection = request.selection.clone().normalize()?;
    let provider = crate::ai_registry::build_generative_provider(&selection)?;
    let typesafe = if selection.provider == crate::ai_selection::PROVIDER_CASCADE {
        let db = open_db(&app)?;
        Some(crate::ai_registry::build_typesafe_client(
            &db,
            &selection,
            request.timeout_seconds,
        )?)
    } else {
        None
    };
    let display_names = {
        let db = open_db(&app)?;
        let taxonomy = load_active_taxonomy(&db)?;
        crate::ai_registry::load_taxonomy_display_names(&db, taxonomy.id)?
    };

    let (total, mut report) = {
        let db = open_db(&app)?;
        db.query_row(
            "SELECT total,processed,updated,skipped FROM ai_classification_runs WHERE id=?1",
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
                ))
            },
        )
        .map_err(|e| e.to_string())?
    };

    let db = open_db(&app)?;
    let run_mode: String = db
        .query_row(
            "SELECT mode FROM ai_classification_runs WHERE id=?1",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    // Geography-only runs skip the catalog. Cascade still runs deterministic first
    // (school codes, forze dell'ordine, comuni, …) then Jev on the rest. Resumed
    // runs repeat it over pending contacts so newer rules spare Jev calls.
    let skip_deterministic = run_mode == "geography";
    if skip_deterministic {
        db.execute(
            "UPDATE ai_classification_runs SET deterministic_finished_at=COALESCE(deterministic_finished_at,?1) WHERE id=?2",
            params![chrono::Utc::now().to_rfc3339(), run_id],
        )
        .map_err(|error| error.to_string())?;
    } else {
        run_deterministic_catalog_pass(&app, &run_id, &mut report, total)?;
    }

    process_ai_run_provider_batched(
        &app,
        provider,
        typesafe,
        &selection,
        &display_names,
        &mut report,
        BatchedRun {
            model: &model,
            request: &request,
            run_id: &run_id,
            total,
        },
    )
    .await?;
    Ok(report)
}

fn pause_run_for_provider_limit(
    app: &tauri::AppHandle,
    run_id: &str,
    reason: &str,
) -> Result<(), String> {
    open_db(app)?.execute(
        "UPDATE ai_classification_runs SET status='paused',interruption_reason=?1,owner_session=NULL,lease_expires_at=NULL WHERE id=?2 AND owner_session=?3",
        params![reason, run_id, APP_SESSION_ID.as_str()],
    ).map_err(|error| error.to_string())?;
    let _ = app.emit(
        "ai-progress",
        AiProgress {
            phase: "paused".to_owned(),
            processed: 0,
            total: 0,
            updated: 0,
            skipped: 0,
            error: Some(reason.to_owned()),
            message: Some(
                "Limite del fornitore raggiunto: esecuzione conservata e messa in pausa".to_owned(),
            ),
        },
    );
    Ok(())
}

enum ProviderLimitAction {
    Continue,
    Paused,
}

async fn handle_provider_limit(
    app: &tauri::AppHandle,
    run_id: &str,
    _request: &AiClassificationRequest,
    err: &ProviderError,
    consecutive_limit_waits: &mut u32,
    max_auto_waits: u32,
    report: &AiEnrichmentReport,
) -> Result<ProviderLimitAction, String> {
    if !err.is_auto_retryable_limit() {
        pause_run_for_provider_limit(app, run_id, &err.message)?;
        return Ok(ProviderLimitAction::Paused);
    }
    *consecutive_limit_waits += 1;
    if *consecutive_limit_waits > max_auto_waits {
        let reason = format!(
            "{} (riprova automatico esaurito dopo {} attese)",
            err.message, max_auto_waits
        );
        pause_run_for_provider_limit(app, run_id, &reason)?;
        return Ok(ProviderLimitAction::Paused);
    }
    let wait = provider_limit_backoff(&err.message, *consecutive_limit_waits);
    let wait_secs = wait.as_secs().max(1);
    let _ = app.emit(
        "ai-progress",
        AiProgress {
            phase: "ai".to_owned(),
            processed: report.processed,
            total: 0,
            updated: report.updated,
            skipped: report.skipped,
            error: Some(err.message.clone()),
            message: Some(format!(
                "Limite o sovraccarico provider: nuovo tentativo tra {wait_secs}s (attesa {}/{max_auto_waits})",
                *consecutive_limit_waits
            )),
        },
    );
    if !wait_while_holding_ai_lease(app, run_id, wait).await? {
        return Ok(ProviderLimitAction::Paused);
    }
    Ok(ProviderLimitAction::Continue)
}

/// Sleeps up to `wait`, renewing the AI lease and honouring Ferma / Annulla.
/// Returns false when the run was paused or cancelled during the wait.
async fn wait_while_holding_ai_lease(
    app: &tauri::AppHandle,
    run_id: &str,
    wait: Duration,
) -> Result<bool, String> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(true);
        }
        let slice = remaining.min(Duration::from_secs(10));
        tokio::time::sleep(slice).await;

        let db = open_db(app)?;
        let (status, owner): (String, Option<String>) = db
            .query_row(
                "SELECT status,owner_session FROM ai_classification_runs WHERE id=?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        // Stopped states first: shutdown/Ferma clear ownership before the
        // worker wakes, and that must not become "Arricchimento fallito".
        if status == "cancelled" {
            let _ = db.execute(
                "UPDATE ai_classification_runs SET owner_session=NULL,lease_expires_at=NULL
                 WHERE id=?1 AND status='cancelled'",
                params![run_id],
            );
            return Ok(false);
        }
        if status == "pausing" || status == "paused" {
            if status == "pausing" {
                let _ = db.execute(
                    "UPDATE ai_classification_runs SET status='paused',owner_session=NULL,lease_expires_at=NULL
                     WHERE id=?1 AND status='pausing'",
                    params![run_id],
                );
            }
            return Ok(false);
        }
        if matches!(
            status.as_str(),
            "failed" | "completed" | "completed_with_errors"
        ) {
            return Ok(false);
        }
        if owner.as_deref() != Some(APP_SESSION_ID.as_str()) {
            return Err("Controllo dell'esecuzione IA perso; elaborazione arrestata".to_owned());
        }
        if !renew_ai_lease(
            &db,
            run_id,
            APP_SESSION_ID.as_str(),
            chrono::Utc::now().timestamp(),
            // Keep waits on the short session lease so a quit mid-wait does not
            // leave pausing stuck for hours behind a future expiry.
            crate::AI_LEASE_SECONDS,
        )? {
            return Err("Controllo dell'esecuzione IA perso; elaborazione arrestata".to_owned());
        }
    }
}
