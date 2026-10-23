//! Deterministic catalog pass that runs before any model call.

use super::*;

#[cfg(test)]
pub(crate) fn evaluate_deterministic(
    email: &str,
    denomination: Option<&str>,
) -> Option<DeterministicResult> {
    let catalog: crate::deterministic_rules::RuleCatalog =
        serde_json::from_str(crate::deterministic_rules::BUILTIN_CATALOG_JSON).ok()?;
    crate::deterministic_rules::evaluate(
        &catalog,
        crate::deterministic_rules::DeterministicEvidence {
            email,
            denomination,
            source_url: None,
        },
    )
    .ok()
    .flatten()
    .map(|matched| matched.result)
}

pub(crate) fn count_deterministic_catalog_matches(
    db: &Connection,
    catalog_id: &str,
) -> Result<usize, String> {
    let catalog = crate::deterministic_rules::catalog_by_id(db, catalog_id)?;
    let compiled = catalog.compile();
    let mut statement = db
        .prepare(
            "SELECT email,denomination,source_url FROM contacts WHERE contact_kind <> 'personale'",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut matches = 0;
    for row in rows {
        let (email, denomination, source_url) = row.map_err(|error| error.to_string())?;
        if matches!(
            crate::deterministic_rules::evaluate_compiled(
                &compiled,
                crate::deterministic_rules::DeterministicEvidence {
                    email: &email,
                    denomination: denomination.as_deref(),
                    source_url: source_url.as_deref(),
                },
            ),
            Ok(Some(_))
        ) {
            matches += 1;
        }
    }
    Ok(matches)
}

fn enqueue_deterministic_follow_up(
    tx: &rusqlite::Transaction<'_>,
    contact_id: &str,
    catalog_id: &str,
    reason: &str,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO classification_follow_up_queue(contact_id,catalog_id,reason,created_at)
         VALUES(?1,?2,?3,?4)
         ON CONFLICT(contact_id) DO UPDATE SET catalog_id=excluded.catalog_id,reason=excluded.reason,created_at=excluded.created_at",
        params![contact_id, catalog_id, reason, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn emit_deterministic_progress(
    app: &tauri::AppHandle,
    processed: usize,
    total: usize,
    updated: usize,
    skipped: usize,
    scanned: usize,
    scan_total: usize,
) {
    let _ = app.emit(
        "ai-progress",
        AiProgress {
            phase: "deterministic".to_owned(),
            processed,
            total,
            updated,
            skipped,
            error: None,
            message: Some(format!(
                "Passo 1 · analizzati {scanned}/{scan_total} contatti · {processed}/{total} match completati"
            )),
        },
    );
}

pub(crate) fn run_deterministic_catalog_pass(
    app: &tauri::AppHandle,
    run_id: &str,
    report: &mut AiEnrichmentReport,
    total: usize,
) -> Result<(), String> {
    let mut db = open_db(app)?;
    let active_taxonomy = load_active_taxonomy(&db)?;
    let (catalog_id, run_mode): (Option<String>, String) = db
        .query_row(
            "SELECT deterministic_catalog_id,mode FROM ai_classification_runs WHERE id=?1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| error.to_string())?;
    let catalog = catalog_id
        .as_deref()
        .map(|id| crate::deterministic_rules::catalog_by_id(&db, id))
        .transpose()?;
    let compiled = catalog.as_ref().map(|catalog| catalog.compile());
    let deterministic_hash = catalog
        .as_ref()
        .map(|catalog| format!("deterministic:{}", catalog.id));
    // Full/fill runs leave unmatched contacts pending for AI; the follow-up
    // queue is only needed when a later follow_up/audit/deterministic_only pass
    // will read it. Writing no_match for every miss was dominating wall time.
    let track_follow_up = matches!(
        run_mode.as_str(),
        "follow_up" | "deterministic_audit" | "deterministic_only"
    );
    type CandidateRow = Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    )>;
    let candidates: CandidateRow = {
        let mut stmt = db
            .prepare(
                "SELECT c.id,c.email,c.denomination,c.source_url,c.comune,c.provincia,c.regione,c.taxonomy_version_id,c.classification_hash
                 FROM ai_run_candidates r
                 JOIN contacts c ON c.id=r.contact_id
                 WHERE r.run_id=?1 AND r.status='pending'",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([run_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    let candidate_total = candidates.len();
    let deterministic_only = run_mode == "deterministic_only";
    emit_deterministic_progress(
        app,
        report.processed,
        total,
        report.updated,
        report.skipped,
        0,
        candidate_total,
    );

    let mut resolved_count = 0;
    let mut skipped_count = 0;
    let mut scanned_count = 0;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    {
        let mut update_candidate = tx
            .prepare(
                "UPDATE ai_run_candidates
                  SET status='processed', processed_at=?1
                  WHERE run_id=?2 AND contact_id=?3",
            )
            .map_err(|e| e.to_string())?;
        let mut skip_candidate = tx
            .prepare(
                "UPDATE ai_run_candidates
                  SET status='skipped', processed_at=?1
                  WHERE run_id=?2 AND contact_id=?3",
            )
            .map_err(|e| e.to_string())?;

        let now = chrono::Utc::now().to_rfc3339();

        for (
            contact_id,
            email,
            denomination,
            source_url,
            old_comune,
            old_provincia,
            old_regione,
            taxonomy_version_id,
            classification_hash,
        ) in candidates
        {
            scanned_count += 1;
            let should_emit = scanned_count % 512 == 0 || scanned_count == candidate_total;
            let Some(compiled) = compiled.as_ref() else {
                if should_emit {
                    emit_deterministic_progress(
                        app,
                        report.processed + resolved_count + skipped_count,
                        total,
                        report.updated + resolved_count,
                        report.skipped + skipped_count,
                        scanned_count,
                        candidate_total,
                    );
                }
                continue;
            };
            let matched = match crate::deterministic_rules::evaluate_compiled(
                compiled,
                crate::deterministic_rules::DeterministicEvidence {
                    email: &email,
                    denomination: denomination.as_deref(),
                    source_url: source_url.as_deref(),
                },
            ) {
                Ok(Some(value)) => value,
                Ok(None) => {
                    fill_unique_gazetteer_geo(
                        &tx,
                        run_id,
                        &contact_id,
                        &email,
                        denomination.as_deref(),
                        source_url.as_deref(),
                        OldGeo {
                            comune: &old_comune,
                            provincia: &old_provincia,
                            regione: &old_regione,
                        },
                    )?;
                    if track_follow_up {
                        enqueue_deterministic_follow_up(
                            &tx,
                            &contact_id,
                            catalog_id.as_deref().expect("catalog pinned"),
                            "no_match",
                        )?;
                    }
                    if deterministic_only {
                        skip_candidate
                            .execute(params![now, run_id, contact_id])
                            .map_err(|error| error.to_string())?;
                    }
                    if should_emit {
                        emit_deterministic_progress(
                            app,
                            report.processed + resolved_count + skipped_count,
                            total,
                            report.updated + resolved_count,
                            report.skipped + skipped_count,
                            scanned_count,
                            candidate_total,
                        );
                    }
                    continue;
                }
                Err(_) => {
                    if track_follow_up {
                        enqueue_deterministic_follow_up(
                            &tx,
                            &contact_id,
                            catalog_id.as_deref().expect("catalog pinned"),
                            "conflict",
                        )?;
                    }
                    if deterministic_only {
                        skip_candidate
                            .execute(params![now, run_id, contact_id])
                            .map_err(|error| error.to_string())?;
                    }
                    if should_emit {
                        emit_deterministic_progress(
                            app,
                            report.processed + resolved_count + skipped_count,
                            total,
                            report.updated + resolved_count,
                            report.skipped + skipped_count,
                            scanned_count,
                            candidate_total,
                        );
                    }
                    continue;
                }
            };
            let mut res = matched.result;
            let school_code = catalog.as_ref().is_some_and(|catalog| {
                catalog
                    .rules
                    .iter()
                    .any(|rule| rule.id == matched.rule_id && rule.kind == "school_mechanographic")
            });
            if school_code {
                keep_existing_geography(&mut res, &old_comune, &old_provincia, &old_regione);
            }
            if res
                .comune
                .as_ref()
                .is_none_or(|value| value.trim().is_empty())
                || res
                    .provincia
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
                || res
                    .regione
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
            {
                let places = crate::ai_typesafe::extract_place_candidates(
                    &crate::ai_typesafe::place_evidence_context(
                        &email,
                        denomination.as_deref(),
                        source_url.as_deref(),
                    ),
                    "Italia",
                );
                let (comune, provincia, regione) = crate::ai_typesafe::unique_geo_values(&places);
                if res
                    .comune
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    res.comune = comune;
                }
                if res
                    .provincia
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    res.provincia = provincia;
                }
                if res
                    .regione
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    res.regione = regione;
                }
            }
            let index = &*crate::gazetteer::GAZETTEER_INDEX;
            let geography_conflict = [
                (&index.municipalities, &old_comune, &res.comune),
                (&index.provinces, &old_provincia, &res.provincia),
                (&index.regions, &old_regione, &res.regione),
            ]
            .into_iter()
            .any(|(names, old, new)| geography_conflicts(names, old, new));
            if geography_conflict {
                if track_follow_up {
                    enqueue_deterministic_follow_up(
                        &tx,
                        &contact_id,
                        catalog_id.as_deref().expect("catalog pinned"),
                        "geography_conflict",
                    )?;
                }
                if deterministic_only {
                    skip_candidate
                        .execute(params![now, run_id, contact_id])
                        .map_err(|error| error.to_string())?;
                    skipped_count += 1;
                }
                if should_emit {
                    emit_deterministic_progress(
                        app,
                        report.processed + resolved_count + skipped_count,
                        total,
                        report.updated + resolved_count,
                        report.skipped + skipped_count,
                        scanned_count,
                        candidate_total,
                    );
                }
                continue;
            }
            if !matches!(run_mode.as_str(), "follow_up" | "deterministic_audit")
                && taxonomy_version_id == Some(active_taxonomy.id)
                && classification_hash.as_deref() == deterministic_hash.as_deref()
            {
                tx.execute(
                    "UPDATE ai_run_candidates SET status='skipped',processed_at=?1
                     WHERE run_id=?2 AND contact_id=?3",
                    params![now, run_id, contact_id],
                )
                .map_err(|error| error.to_string())?;
                skipped_count += 1;
                if should_emit {
                    emit_deterministic_progress(
                        app,
                        report.processed + resolved_count + skipped_count,
                        total,
                        report.updated + resolved_count,
                        report.skipped + skipped_count,
                        scanned_count,
                        candidate_total,
                    );
                }
                continue;
            }
            apply_deterministic_to_contact_with_rule(
                &tx,
                run_id,
                &contact_id,
                &res,
                true,
                DeterministicRule {
                    catalog_id: catalog_id.as_deref().expect("catalog pinned"),
                    rule_id: &matched.rule_id,
                    rule_version: matched.rule_version,
                },
            )?;
            let retain_for_ai = run_mode == "follow_up"
                || run_mode == "deterministic_audit"
                || res.subcategory.is_none();
            if retain_for_ai {
                if res.subcategory.is_none() {
                    if track_follow_up {
                        enqueue_deterministic_follow_up(
                            &tx,
                            &contact_id,
                            catalog_id.as_deref().expect("catalog pinned"),
                            "missing_subcategory",
                        )?;
                    }
                    if deterministic_only {
                        skip_candidate
                            .execute(params![now, run_id, contact_id])
                            .map_err(|error| error.to_string())?;
                        skipped_count += 1;
                    }
                }
            } else {
                update_candidate
                    .execute(params![now, run_id, contact_id])
                    .map_err(|e| e.to_string())?;
                tx.execute(
                    "DELETE FROM classification_follow_up_queue WHERE contact_id=?1",
                    params![contact_id],
                )
                .map_err(|error| error.to_string())?;
                resolved_count += 1;
            }
            if should_emit {
                emit_deterministic_progress(
                    app,
                    report.processed + resolved_count + skipped_count,
                    total,
                    report.updated + resolved_count,
                    report.skipped + skipped_count,
                    scanned_count,
                    candidate_total,
                );
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    open_db(app)?
        .execute(
            "UPDATE ai_classification_runs SET deterministic_finished_at=?1 WHERE id=?2",
            params![chrono::Utc::now().to_rfc3339(), run_id],
        )
        .map_err(|error| error.to_string())?;

    if resolved_count > 0 || skipped_count > 0 {
        report.processed += resolved_count + skipped_count;
        report.updated += resolved_count;
        report.skipped += skipped_count;
        let db2 = open_db(app)?;
        db2.execute(
            "UPDATE ai_classification_runs
             SET processed=processed+?1,updated=updated+?2,skipped=skipped+?3
             WHERE id=?4",
            params![
                (resolved_count + skipped_count) as i64,
                resolved_count as i64,
                skipped_count as i64,
                run_id
            ],
        )
        .map_err(|e| e.to_string())?;

        let _ = app.emit(
            "ai-progress",
            AiProgress {
                phase: "deterministic".to_owned(),
                processed: report.processed,
                total,
                updated: report.updated,
                skipped: report.skipped,
                error: None,
                message: Some(if deterministic_only {
                    format!(
                        "Passo 1 completato · {resolved_count} classificati · {skipped_count} già aggiornati"
                    )
                } else {
                    format!(
                        "Passo 1 completato · {resolved_count} classificati · {skipped_count} già aggiornati. Passo 2 · Classificazione IA in avvio…"
                    )
                }),
            },
        );
    }
    Ok(())
}

/// Empty, or a pipe-joined conflict left by merging source rows: both still
/// need one resolved place.
/// A school code's province prefix predates newer provinces (BT, MB, SU,
/// Roma Capitale), so a contact's existing geography wins over it.
fn keep_existing_geography(
    res: &mut DeterministicResult,
    comune: &Option<String>,
    provincia: &Option<String>,
    regione: &Option<String>,
) {
    if comune.is_some() || provincia.is_some() || regione.is_some() {
        res.comune = comune.clone();
        res.provincia = provincia.clone();
        res.regione = regione.clone();
    }
}

/// A stored place conflicts with a rule's place unless they name the same
/// gazetteer entry ("Florence" = "Firenze", "Roma Capitale" = "Roma") or the
/// rule picks one of the options of a pipe-joined value, which apply replaces.
fn geography_conflicts(
    names: &std::collections::HashMap<String, &'static str>,
    old: &Option<String>,
    new: &Option<String>,
) -> bool {
    let (Some(old), Some(new)) = (old, new) else {
        return false;
    };
    let canonical = |value: &str| {
        let key = crate::gazetteer::norm(value);
        names
            .get(&key)
            .map(|name| crate::gazetteer::norm(name))
            .unwrap_or(key)
    };
    let new = canonical(new);
    let options = old
        .split('|')
        .map(str::trim)
        .filter(|option| !option.is_empty())
        .collect::<Vec<_>>();
    !options.is_empty() && !options.iter().any(|option| canonical(option) == new)
}

pub(super) fn empty_geo(value: &Option<String>) -> bool {
    value
        .as_ref()
        .is_none_or(|text| text.trim().is_empty() || text.contains('|'))
}

/// Fill empty comune/provincia/regione from unique gazetteer hits. No AI, no taxonomy write.
/// Pipe-joined conflicts stay: this path records no history, so it must not overwrite.
struct OldGeo<'a> {
    comune: &'a Option<String>,
    provincia: &'a Option<String>,
    regione: &'a Option<String>,
}

fn fill_unique_gazetteer_geo(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    contact_id: &str,
    email: &str,
    denomination: Option<&str>,
    source_url: Option<&str>,
    old_geo: OldGeo<'_>,
) -> Result<(), String> {
    let OldGeo {
        comune: old_comune,
        provincia: old_provincia,
        regione: old_regione,
    } = old_geo;
    if !empty_geo(old_comune) && !empty_geo(old_provincia) && !empty_geo(old_regione) {
        return Ok(());
    }
    let places = crate::ai_typesafe::extract_place_candidates(
        &crate::ai_typesafe::place_evidence_context(email, denomination, source_url),
        "Italia",
    );
    let (comune, provincia, regione) = crate::ai_typesafe::unique_geo_values(&places);
    let now = chrono::Utc::now().to_rfc3339();
    for (column, old, new) in [
        ("comune", old_comune, comune),
        ("provincia", old_provincia, provincia),
        ("regione", old_regione, regione),
    ] {
        let Some(new) = new else {
            continue;
        };
        // Only blank fields: pipe-separated conflicts belong to the rule/Jev paths.
        if old.as_deref().is_some_and(|value| !value.trim().is_empty()) {
            continue;
        }
        tx.execute(
            &format!("UPDATE contacts SET {column}=?1 WHERE id=?2"),
            params![new, contact_id],
        )
        .map_err(|error| error.to_string())?;
        // Logged like every other run write, so rollback can restore it.
        tx.execute(
            "INSERT INTO ai_field_changes(
                id,run_id,contact_id,field_name,old_value,new_value,change_kind,status,
                confidence,evidence,created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,'aggiunto','applied',1.0,?7,?8)",
            params![
                crate::new_contact_id(),
                run_id,
                contact_id,
                column,
                old.as_deref(),
                new,
                r#"["Passo deterministico · Luogo univoco nel gazetteer"]"#,
                now,
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The catalog rule that produced a deterministic result.
pub(crate) struct DeterministicRule<'a> {
    pub(crate) catalog_id: &'a str,
    pub(crate) rule_id: &'a str,
    pub(crate) rule_version: i64,
}

pub(crate) fn apply_deterministic_to_contact_with_rule(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    contact_id: &str,
    res: &DeterministicResult,
    overwrite_classification: bool,
    rule: DeterministicRule<'_>,
) -> Result<(), String> {
    let DeterministicRule {
        catalog_id,
        rule_id,
        rule_version,
    } = rule;
    let taxonomy_version_id: i64 = tx
        .query_row(
            "SELECT id FROM taxonomy_versions WHERE status='active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let deterministic_hash = format!("deterministic:{catalog_id}");
    type BeforeRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        f64,
        String,
        i64,
    );
    let before: BeforeRow = tx
        .query_row(
            "SELECT category,subcategory,comune,provincia,regione,taxonomy_version_id,
                    classification_state,classification_state_reason,classification_hash,
                    macroarea,classification_method,classification_confidence,
                    classification_evidence,needs_review
             FROM contacts WHERE id=?1",
            params![contact_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?;

    let effective_subcategory = res
        .subcategory
        .or_else(|| crate::taxonomy::fallback_subcategory(res.category));
    let category_placeholder = normalized_history_value(before.0.as_deref()).is_none()
        || is_placeholder(before.0.as_deref().unwrap_or_default());
    let ai_owns_classification = before.10.eq_ignore_ascii_case("ai");
    // A newer catalog supersedes an older deterministic result.
    let owns_classification = !ai_owns_classification
        && (overwrite_classification
            || category_placeholder
            || before.10.eq_ignore_ascii_case("deterministic")
            || before
                .0
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(res.category)));
    let classification_state = if ai_owns_classification {
        before.6.as_str()
    } else if owns_classification {
        "classified"
    } else {
        "conflicting"
    };
    let classification_reason = if ai_owns_classification {
        before.7.clone()
    } else if owns_classification {
        None
    } else {
        Some(format!(
            "Il passo deterministico suggerisce {} ma conserva la classificazione esistente",
            res.category
        ))
    };

    tx.execute(
        "UPDATE contacts
          SET category=CASE WHEN ?8=1 THEN ?1 ELSE category END,
              subcategory=CASE WHEN ?8=1 THEN ?2 ELSE subcategory END,
              comune=CASE WHEN comune IS NULL OR trim(comune)='' OR (?3 IS NOT NULL AND instr(comune,'|')>0) THEN ?3 ELSE comune END,
              provincia=CASE WHEN provincia IS NULL OR trim(provincia)='' OR (?4 IS NOT NULL AND instr(provincia,'|')>0) THEN ?4 ELSE provincia END,
              regione=CASE WHEN regione IS NULL OR trim(regione)='' OR (?5 IS NOT NULL AND instr(regione,'|')>0) THEN ?5 ELSE regione END,
              classification_method=CASE WHEN ?8=1 THEN 'deterministic' ELSE classification_method END,
              classification_confidence=CASE WHEN ?8=1 THEN 1 ELSE classification_confidence END,
              classification_evidence=CASE WHEN ?8=1 THEN ?12 ELSE classification_evidence END,
              needs_review=CASE WHEN ?8=1 THEN 0 ELSE needs_review END,
              classification_hash=CASE WHEN ?8=1 THEN ?6 ELSE classification_hash END,
              taxonomy_version_id=CASE WHEN ?8=1 THEN ?9 ELSE taxonomy_version_id END,
              classification_state=CASE WHEN ?8=1 THEN ?10 ELSE classification_state END,
              classification_state_reason=CASE WHEN ?8=1 THEN ?11 ELSE classification_state_reason END,
              category_id=(SELECT id FROM categories WHERE name=CASE WHEN ?8=1 THEN ?1 ELSE category END),
              subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id
                              WHERE c.name=CASE WHEN ?8=1 THEN ?1 ELSE category END
                                AND s.name=CASE WHEN ?8=1 THEN ?2 ELSE subcategory END)
          WHERE id=?7",
        params![
            res.category,
            effective_subcategory,
            res.comune,
            res.provincia,
            res.regione,
            deterministic_hash,
            contact_id,
            owns_classification as i64,
            taxonomy_version_id,
            classification_state,
            classification_reason,
            serde_json::to_string(&[format!("Passo deterministico · {}", res.reason)])
                .map_err(|error| error.to_string())?,
        ],
    )
    .map_err(|e| e.to_string())?;

    let mut changes: Vec<(&str, Option<&str>, Option<&str>)> = Vec::new();
    if owns_classification
        && !before
            .0
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(res.category))
    {
        changes.push(("category", before.0.as_deref(), Some(res.category)));
    }
    if let Some(sub) = effective_subcategory
        && owns_classification
        && before.1.as_deref() != Some(sub)
    {
        changes.push(("subcategory", before.1.as_deref(), Some(sub)));
    }
    for (field, old, new) in [
        ("comune", before.2.as_deref(), res.comune.as_deref()),
        ("provincia", before.3.as_deref(), res.provincia.as_deref()),
        ("regione", before.4.as_deref(), res.regione.as_deref()),
    ] {
        let replaceable =
            normalized_history_value(old).is_none() || old.is_some_and(|value| value.contains('|'));
        if replaceable && new.is_some() {
            changes.push((field, old, new));
        }
    }
    let metadata_changes = [
        (
            "taxonomy_version_id",
            before.5.map(|value| value.to_string()),
            if owns_classification {
                Some(taxonomy_version_id.to_string())
            } else {
                before.5.map(|value| value.to_string())
            },
        ),
        (
            "classification_state",
            Some(before.6.clone()),
            Some(classification_state.to_owned()),
        ),
        (
            "classification_state_reason",
            before.7.clone(),
            classification_reason.clone(),
        ),
        (
            "classification_hash",
            before.8.clone(),
            if owns_classification {
                Some(deterministic_hash.clone())
            } else {
                before.8.clone()
            },
        ),
        (
            "classification_method",
            Some(before.10.clone()),
            Some(if owns_classification {
                "deterministic".to_owned()
            } else {
                before.10.clone()
            }),
        ),
        (
            "classification_confidence",
            Some(before.11.to_string()),
            Some(if owns_classification {
                "1".to_owned()
            } else {
                before.11.to_string()
            }),
        ),
        (
            "classification_evidence",
            Some(before.12.clone()),
            if owns_classification {
                Some(
                    serde_json::to_string(&[format!("Passo deterministico · {}", res.reason)])
                        .map_err(|error| error.to_string())?,
                )
            } else {
                Some(before.12.clone())
            },
        ),
        (
            "needs_review",
            Some(before.13.to_string()),
            Some(if owns_classification {
                "0".to_owned()
            } else {
                before.13.to_string()
            }),
        ),
    ];
    for (field_name, old_value, new_value) in changes {
        tx.execute(
            "INSERT INTO ai_field_changes(
                id,run_id,contact_id,review_id,field_name,old_value,new_value,change_kind,status,
                confidence,evidence,created_at
             ) VALUES (?1,?2,?3,NULL,?4,?5,?6,?7,'applied',1,?8,?9)",
            params![
                new_contact_id(),
                run_id,
                contact_id,
                field_name,
                old_value,
                new_value,
                if normalized_history_value(old_value).is_none() {
                    "aggiunto"
                } else {
                    "modificato"
                },
                serde_json::to_string(&[format!("Passo deterministico · {}", res.reason)])
                    .map_err(|e| e.to_string())?,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    for (field_name, old_value, new_value) in metadata_changes {
        if normalized_history_value(old_value.as_deref())
            == normalized_history_value(new_value.as_deref())
        {
            continue;
        }
        tx.execute(
            "INSERT INTO ai_field_changes(
                id,run_id,contact_id,review_id,field_name,old_value,new_value,
                change_kind,status,confidence,evidence,created_at
             ) VALUES(?1,?2,?3,NULL,?4,?5,?6,
                CASE WHEN ?5 IS NULL OR trim(?5)='' THEN 'aggiunto' ELSE 'modificato' END,
                'applied',1,?7,?8)",
            params![
                new_contact_id(),
                run_id,
                contact_id,
                field_name,
                old_value,
                new_value,
                serde_json::to_string(&[format!("Passo deterministico · {}", res.reason)])
                    .map_err(|error| error.to_string())?,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO deterministic_rule_attributions(contact_id,rule_id,rule_version,catalog_id,run_id,taxonomy_version_id,applied_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![contact_id, rule_id, rule_version, catalog_id, run_id, taxonomy_version_id, chrono::Utc::now().to_rfc3339()],
    ).map_err(|error| error.to_string())?;
    tx.execute(
        "DELETE FROM deterministic_recheck_queue WHERE contact_id=?1",
        params![contact_id],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn apply_deterministic_to_contact(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    contact_id: &str,
    result: &DeterministicResult,
) -> Result<(), String> {
    apply_deterministic_to_contact_with_rule(
        tx,
        run_id,
        contact_id,
        result,
        false,
        DeterministicRule {
            catalog_id: "exact-it-v1",
            rule_id: "test.rule",
            rule_version: 1,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_gazetteer_fill_is_logged_and_rolls_back() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::setup_schema(&db).unwrap();
        db.execute_batch(
            "INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,
                validation_notes,source_file,category,contact_kind,paese)
             VALUES('c1','anagrafe.aprigliano@libero.it','anagrafe.aprigliano','libero','.it',
                'valid','[]','test.txt','Varie','non_determinato','Italia'),
                   ('c2','utente@example.com','utente','example','.com',
                'valid','[]','test.txt','Varie','non_determinato','Italia');
             INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
             VALUES('run-geo','deterministic','default','deterministic','completed',2,'now');",
        )
        .unwrap();
        let tx = db.transaction().unwrap();
        for (id, email) in [
            ("c1", "anagrafe.aprigliano@libero.it"),
            ("c2", "utente@example.com"),
        ] {
            fill_unique_gazetteer_geo(
                &tx,
                "run-geo",
                id,
                email,
                None,
                None,
                OldGeo {
                    comune: &None,
                    provincia: &None,
                    regione: &None,
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let comune = |db: &rusqlite::Connection, id: &str| -> Option<String> {
            db.query_row("SELECT comune FROM contacts WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert_eq!(comune(&db, "c1").as_deref(), Some("Aprigliano"));
        // `.com` is not Como.
        assert_eq!(comune(&db, "c2"), None);
        let logged: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM ai_field_changes WHERE run_id='run-geo' AND contact_id='c1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(logged >= 1);
        crate::ai_runs::rollback_ai_scope_conn(&mut db, "latest").unwrap();
        assert_eq!(comune(&db, "c1"), None);
    }

    #[test]
    fn school_code_province_does_not_conflict_with_newer_provinces() {
        let mut res = evaluate_deterministic("bamm067007@istruzione.it", None).unwrap();
        assert_eq!(res.provincia.as_deref(), Some("Bari"));
        keep_existing_geography(
            &mut res,
            &Some("Andria".to_owned()),
            &Some("Barletta-Andria-Trani".to_owned()),
            &None,
        );
        assert_eq!(res.comune.as_deref(), Some("Andria"));
        assert_eq!(res.provincia.as_deref(), Some("Barletta-Andria-Trani"));
        assert_eq!(res.regione, None);

        let mut unknown = evaluate_deterministic("rmic000000@istruzione.it", None).unwrap();
        keep_existing_geography(&mut unknown, &None, &None, &None);
        assert_eq!(unknown.provincia.as_deref(), Some("Roma"));
    }

    #[test]
    fn geography_conflicts_ignore_aliases_blanks_and_resolvable_pipe_values() {
        let index = &*crate::gazetteer::GAZETTEER_INDEX;
        let place = |value: &str| Some(value.to_owned());
        let provinces = &index.provinces;
        assert!(!geography_conflicts(
            provinces,
            &place("Florence"),
            &place("Firenze")
        ));
        assert!(!geography_conflicts(
            provinces,
            &place("Roma Capitale"),
            &place("Roma")
        ));
        assert!(!geography_conflicts(provinces, &place(""), &place("Pavia")));
        assert!(!geography_conflicts(provinces, &None, &place("Pavia")));
        assert!(!geography_conflicts(
            provinces,
            &place("Cremona | Pavia | Lodi"),
            &place("Pavia")
        ));
        assert!(geography_conflicts(
            provinces,
            &place("Cremona | Lodi | Mantova"),
            &place("Pavia")
        ));
        assert!(geography_conflicts(
            provinces,
            &place("Bari"),
            &place("Lecce")
        ));
    }
}
