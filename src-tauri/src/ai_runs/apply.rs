//! Applying validated taxonomy and geography results to contacts.

use super::*;

pub(crate) fn taxonomy_boundary(
    raw: &str,
    taxonomy: &[(String, Vec<String>)],
    sources: &str,
) -> Result<TaxonomyResult, Vec<String>> {
    parse_taxonomy_result(raw)
        .map_err(|_| vec!["schema.taxonomy.invalid".to_owned()])
        .and_then(|mut value| {
            if matches!(value.outcome, crate::TaxonomyOutcome::Insufficient) {
                value.candidates.clear();
            }
            for cand in &mut value.candidates {
                if cand.category_key.contains(':') && cand.subcategory_key.is_none() {
                    let parts = cand
                        .category_key
                        .split_once(':')
                        .map(|(c, s)| (c.trim().to_string(), s.trim().to_string()));
                    if let Some((cat, sub)) = parts {
                        cand.category_key = cat;
                        cand.subcategory_key = Some(sub);
                    }
                }
                if cand.subcategory_key.is_some() {
                    if cand.subcategory_evidence.is_empty() && !cand.category_evidence.is_empty() {
                        cand.subcategory_evidence = cand.category_evidence.clone();
                    }
                    if cand.subcategory_grade.is_none() {
                        cand.subcategory_grade = cand.category_grade.clone().into();
                    }
                } else {
                    cand.subcategory_grade = None;
                    cand.subcategory_evidence.clear();
                }
                if let Some((canonical_cat, canonical_subs)) = taxonomy
                    .iter()
                    .find(|(k, _)| taxonomy_slug(k) == taxonomy_slug(&cand.category_key))
                {
                    cand.category_key = canonical_cat.clone();
                    if let Some(sub) = &cand.subcategory_key
                        && let Some(canonical_sub) = canonical_subs
                            .iter()
                            .find(|s| taxonomy_slug(s) == taxonomy_slug(sub))
                    {
                        cand.subcategory_key = Some(canonical_sub.clone());
                    }
                }
            }
            if !matches!(value.outcome, crate::TaxonomyOutcome::TaxonomyGap) {
                value.gap = None;
            }
            validate_taxonomy_result(&value, taxonomy, sources)
                .map(|_| value)
                .map_err(|e| e.split(',').map(str::to_owned).collect())
        })
}

pub(crate) fn geography_boundary(
    raw: &str,
    gazetteer: &[crate::gazetteer::Place],
    fixed_country: &str,
    sources: &str,
) -> Result<GeographicResult, Vec<String>> {
    parse_geography_result(raw)
        .map_err(|_| vec!["schema.geography.invalid".to_owned()])
        .and_then(|value| {
            let mut errors = validate_geography_result(&value, sources)
                .err()
                .map(|e| e.split(',').map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_default();
            let allowed_hints: &[&str] = match fixed_country.trim().to_uppercase().as_str() {
                "ITALIA" | "IT" => &["municipality", "province", "region"],
                "SVIZZERA" | "SWITZERLAND" | "CH" => &["municipality", "canton"],
                _ => &[],
            };
            let mut places = Vec::new();
            for claim in &value.claims {
                if claim
                    .level_hint
                    .as_deref()
                    .is_some_and(|hint| !allowed_hints.contains(&hint))
                {
                    errors.push("geography.level_hint.invalid".to_owned());
                    continue;
                }
                let matches = gazetteer
                    .iter()
                    .filter(|place| {
                        let observed = normalize_grounding(&claim.observed_place);
                        (normalize_grounding(&place.name) == observed
                            || place
                                .aliases
                                .iter()
                                .any(|alias| normalize_grounding(alias) == observed))
                            && claim
                                .level_hint
                                .as_deref()
                                .is_none_or(|hint| hint == place.level)
                    })
                    .collect::<Vec<_>>();
                if matches.len() == 1 {
                    places.push(matches[0]);
                } else if let Some(muni) = matches.iter().find(|p| p.level == "municipality") {
                    places.push(*muni);
                } else if let Some(prov) = matches.iter().find(|p| p.level == "province") {
                    places.push(*prov);
                } else if let Some(parent_match) = gazetteer.iter().find(|place| {
                    let obs = normalize_grounding(&claim.observed_place);
                    (normalize_grounding(&place.name) == obs
                        || place
                            .aliases
                            .iter()
                            .any(|alias| normalize_grounding(alias) == obs))
                        && claim
                            .level_hint
                            .as_deref()
                            .is_none_or(|hint| hint == place.level)
                }) {
                    places.push(parent_match);
                } else {
                    errors.push("geography.place.unresolvable".to_owned());
                }
            }
            if places.len() == value.claims.len() && places.len() > 1 {
                let compatible = places.iter().enumerate().all(|(index, left)| {
                    places.iter().skip(index + 1).all(|right| {
                        left.id == right.id
                            || left.parent_ids.contains(&right.id)
                            || right.parent_ids.contains(&left.id)
                            || (left.level == "municipality"
                                && right.level == "municipality"
                                && left
                                    .parent_ids
                                    .iter()
                                    .any(|pid| right.parent_ids.contains(pid)))
                    })
                });
                if value.outcome == "proposal" && !compatible {
                    errors.push("geography.proposal.claims.conflict".to_owned());
                }
                if value.outcome == "conflict" && compatible {
                    errors.push("geography.conflict.claims.compatible".to_owned());
                }
            }
            errors.sort();
            errors.dedup();
            if errors.is_empty() {
                Ok(value)
            } else {
                Err(errors)
            }
        })
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct UnifiedResult {
    pub taxonomy: TaxonomyResult,
    pub geography: GeographicResult,
}

pub(crate) fn unified_boundary(
    raw: &str,
    taxonomy: &[(String, Vec<String>)],
    gazetteer: &[crate::gazetteer::Place],
    fixed_country: &str,
    sources: &str,
) -> Result<UnifiedResult, Vec<String>> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| vec!["schema.unified.invalid_json".to_owned()])?;

    let taxonomy_str = if let Some(t) = value.get("taxonomy") {
        serde_json::to_string(t).unwrap_or_default()
    } else {
        raw.to_owned()
    };

    let geography_str = if let Some(g) = value.get("geography") {
        serde_json::to_string(g).unwrap_or_default()
    } else {
        raw.to_owned()
    };

    let tax_res = taxonomy_boundary(&taxonomy_str, taxonomy, sources);
    let geo_res = geography_boundary(&geography_str, gazetteer, fixed_country, sources);

    match (tax_res, geo_res) {
        (Ok(tax), Ok(geo)) => Ok(UnifiedResult {
            taxonomy: tax,
            geography: geo,
        }),
        (Err(mut errs1), Err(errs2)) => {
            errs1.extend(errs2);
            Err(errs1)
        }
        (Err(errs), Ok(_)) => Err(errs),
        (Ok(_), Err(errs)) => Err(errs),
    }
}

#[cfg(test)]
pub(crate) fn taxonomy_valid(category: &str, subcategory: Option<&str>) -> bool {
    TAXONOMY
        .iter()
        .any(|(c, values)| *c == category && subcategory.is_none_or(|s| values.contains(&s)))
}
pub(crate) fn normalized_history_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("unknown"))
        .map(|value| value.to_lowercase())
}

/// Human-facing label for the provenance tags persisted in `via:<tag>` evidence.
///
/// The wire tags stay `muse-minimal` / `muse-high` for backward compatibility with
/// existing databases; only the display label changed to the current provider.
pub(super) fn inference_source_label(source: Option<&str>) -> &'static str {
    match source {
        Some("jev") => "Jev",
        Some("muse-minimal") => "Muse Spark",
        Some("muse-high") => "Muse Spark (retry)",
        _ => "IA",
    }
}

pub(super) fn tag_contact_field_changes_with_source(
    db: &Connection,
    run_id: &str,
    contact_id: &str,
    source: &str,
) -> Result<(), String> {
    let rows: Vec<(String, String)> = db
        .prepare(
            "SELECT id, evidence FROM ai_field_changes
             WHERE run_id=?1 AND contact_id=?2
               AND status IN ('applied','pending_review') AND rolled_back=0",
        )
        .map_err(|e| e.to_string())?
        .query_map(params![run_id, contact_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let marker = format!("via:{source}");
    for (change_id, evidence_json) in rows {
        let mut evidence: Vec<String> = serde_json::from_str(&evidence_json).unwrap_or_default();
        if evidence.iter().any(|entry| entry.starts_with("via:")) {
            continue;
        }
        evidence.insert(0, marker.clone());
        db.execute(
            "UPDATE ai_field_changes SET evidence=?1 WHERE id=?2",
            params![
                serde_json::to_string(&evidence).map_err(|e| e.to_string())?,
                change_id
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// One proposed or applied change to a contact field, as recorded in the field-change history.
pub(crate) struct FieldChange<'a> {
    pub(crate) review_id: Option<&'a str>,
    pub(crate) field_name: &'a str,
    pub(crate) old_value: Option<&'a str>,
    pub(crate) new_value: Option<&'a str>,
    pub(crate) confidence: f64,
    pub(crate) evidence: &'a [String],
    pub(crate) pending_review: bool,
}

pub(crate) fn record_ai_field_change(
    db: &Connection,
    run_id: &str,
    contact: &ContactRecord,
    change: FieldChange<'_>,
) -> Result<(), String> {
    let FieldChange {
        review_id,
        field_name,
        old_value,
        new_value,
        confidence,
        evidence,
        pending_review,
    } = change;
    if normalized_history_value(old_value) == normalized_history_value(new_value) {
        return Ok(());
    }
    if normalized_history_value(new_value).is_none() {
        return Ok(());
    }
    let change_kind = if pending_review {
        "in_revisione"
    } else if normalized_history_value(old_value).is_none() {
        "aggiunto"
    } else {
        "modificato"
    };
    db.execute(
        "INSERT INTO ai_field_changes(
            id,run_id,contact_id,review_id,field_name,old_value,new_value,change_kind,status,
            confidence,evidence,created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            new_contact_id(),
            run_id,
            contact.id,
            review_id,
            field_name,
            old_value,
            new_value,
            change_kind,
            if pending_review {
                "pending_review"
            } else {
                "applied"
            },
            confidence,
            serde_json::to_string(evidence).map_err(|e| e.to_string())?,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn is_placeholder(value: &str) -> bool {
    let v = value.trim().to_ascii_lowercase();
    v.is_empty()
        || matches!(
            v.as_str(),
            "varie" | "unknown" | "sconosciuto" | "dato misto o da revisionare"
        )
}

fn taxonomy_candidate_values_at(
    db: &Connection,
    candidate: &TaxonomyCandidate,
    catalog_version: &str,
) -> Result<(String, Option<String>), String> {
    let category: String = db
        .query_row(
            "SELECT e.display_name FROM taxonomy_entries e
             JOIN taxonomy_versions v ON v.id=e.taxonomy_version_id
             WHERE v.semantic_version=?1 AND e.level='category' AND e.semantic_key=?2",
            params![catalog_version, candidate.category_key],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let subcategory = candidate.subcategory_key.as_deref().map(|key| db.query_row("SELECT s.display_name FROM taxonomy_entries s JOIN taxonomy_versions v ON v.id=s.taxonomy_version_id WHERE v.semantic_version=?1 AND s.level='subcategory' AND s.parent_semantic_key=?2 AND s.semantic_key=?3", params![catalog_version, candidate.category_key, key], |r| r.get(0))).transpose().map_err(|e| e.to_string())?;

    Ok((category, subcategory))
}

pub(crate) fn apply_geography_result(
    db: &Connection,
    contact: &ContactRecord,
    result: &GeographicResult,
    run_id: &str,
) -> Result<&'static str, String> {
    if let Some(country) = result
        .country
        .as_deref()
        .filter(|c| matches!(*c, "Italia" | "Svizzera") && *c != contact.paese)
    {
        db.execute(
            "UPDATE contacts SET paese=?1 WHERE id=?2",
            params![country, contact.id],
        )
        .map_err(|e| e.to_string())?;
        let evidence = result
            .summary
            .iter()
            .map(|s| format!("Motivo: {s}"))
            .collect::<Vec<_>>();
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "paese",
                old_value: Some(&contact.paese),
                new_value: Some(country),
                confidence: 0.0,
                evidence: &evidence,
                pending_review: false,
            },
        )?;
        // shortcut: place claims were picked from the old country's gazetteer, skip them until the next run.
        return Ok("updated");
    }
    if result.outcome == "insufficient" {
        return Ok("skipped");
    }
    let mut resolved = Vec::new();
    for claim in &result.claims {
        match crate::gazetteer::resolve(
            db,
            &contact.paese,
            &format!(
                "{}:{}",
                claim.level_hint.as_deref().unwrap_or("municipality"),
                claim.observed_place
            ),
        ) {
            Ok(place) => resolved.push(place),
            Err(_) => return Ok("skipped"),
        }
    }
    if result.outcome == "conflict"
        || result.claims.is_empty()
        || resolved.len() != result.claims.len()
    {
        return Ok("skipped");
    }
    let compatible = resolved.iter().enumerate().all(|(index, left)| {
        resolved.iter().skip(index + 1).all(|right| {
            left.id == right.id
                || left.parent_ids.contains(&right.id)
                || right.parent_ids.contains(&left.id)
        })
    });
    if !compatible {
        return Ok("skipped");
    }

    let place = resolved
        .iter()
        .min_by_key(|place| match place.level.as_str() {
            "municipality" => 0,
            "province" | "canton" => 1,
            "region" => 2,
            _ => 3,
        })
        .ok_or_else(|| "geography.place.missing".to_owned())?;
    let comune = (place.level == "municipality").then(|| place.name.clone());
    let mut provincia = None;
    let mut regione = None;
    for (level, name) in &place.parents {
        if level == "province" || level == "canton" {
            provincia = Some(name.clone());
        } else if level == "region" {
            regione = Some(name.clone());
        }
    }
    if place.level == "province" || place.level == "canton" {
        provincia = Some(place.name.clone());
    }
    if place.level == "region" {
        regione = Some(place.name.clone());
    }
    let before: (Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT comune,provincia,regione FROM contacts WHERE id=?1",
            params![contact.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    let comune_assignment = empty_geo(&before.0).then_some(comune).flatten();
    let provincia_assignment = empty_geo(&before.1).then_some(provincia).flatten();
    let regione_assignment = empty_geo(&before.2).then_some(regione).flatten();
    if comune_assignment.is_none() && provincia_assignment.is_none() && regione_assignment.is_none()
    {
        return Ok("skipped");
    }
    db.execute(
        "UPDATE contacts SET comune=COALESCE(?1,comune),provincia=COALESCE(?2,provincia),regione=COALESCE(?3,regione) WHERE id=?4",
        params![comune_assignment, provincia_assignment, regione_assignment, contact.id],
    )
    .map_err(|e| e.to_string())?;
    let after: (Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT comune,provincia,regione FROM contacts WHERE id=?1",
            params![contact.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    let mut geo_evidence =
        grounded_evidence_strings(result.claims.iter().flat_map(|claim| claim.evidence.iter()));
    if let Some(summary) = result.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        geo_evidence.push(format!("Motivo: {summary}"));
    }
    let mut applied = false;
    for (field, old_value, new_value) in [
        ("comune", before.0.as_deref(), after.0.as_deref()),
        ("provincia", before.1.as_deref(), after.1.as_deref()),
        ("regione", before.2.as_deref(), after.2.as_deref()),
    ] {
        if normalized_history_value(old_value) != normalized_history_value(new_value) {
            record_ai_field_change(
                db,
                run_id,
                contact,
                FieldChange {
                    review_id: None,
                    field_name: field,
                    old_value,
                    new_value,
                    confidence: 0.0,
                    evidence: &geo_evidence,
                    pending_review: false,
                },
            )?;
            applied = true;
        }
    }
    Ok(if applied { "updated" } else { "skipped" })
}

fn ensure_ai_classification_fallback(
    db: &Connection,
    contact: &ContactRecord,
    run_id: &str,
    force_undetermined: bool,
    evidence: &[String],
) -> Result<(String, String), String> {
    if !force_undetermined
        && contact
            .subcategory
            .as_deref()
            .is_some_and(|value| !is_placeholder(value))
    {
        return Ok((
            contact.category.clone(),
            contact.subcategory.clone().unwrap(),
        ));
    }
    let category = if force_undetermined || is_placeholder(&contact.category) {
        "Varie".to_owned()
    } else {
        let known: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM categories WHERE name=?1 AND active=1)",
                params![contact.category],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if known {
            contact.category.clone()
        } else {
            "Varie".to_owned()
        }
    };
    let subcategory = crate::taxonomy::fallback_subcategory(&category)
        .ok_or_else(|| format!("no taxonomy fallback for category {category}"))?
        .to_owned();
    if contact.category != category {
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "category",
                old_value: Some(&contact.category),
                new_value: Some(&category),
                confidence: 0.6,
                evidence,
                pending_review: false,
            },
        )?;
    }
    if contact.subcategory.as_deref() != Some(subcategory.as_str()) {
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "subcategory",
                old_value: contact.subcategory.as_deref(),
                new_value: Some(&subcategory),
                confidence: 0.6,
                evidence,
                pending_review: false,
            },
        )?;
    }
    db.execute(
        "UPDATE contacts SET category=?1,subcategory=?2,
            category_id=(SELECT id FROM categories WHERE name=?1 AND active=1),
            subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id
                            WHERE c.name=?1 AND s.name=?2 AND s.active=1),
            classification_method='ai',classification_confidence=?3,
            classification_evidence=?4
         WHERE id=?5",
        params![
            category,
            subcategory,
            0.6_f64,
            serde_json::to_string(evidence).map_err(|error| error.to_string())?,
            contact.id
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok((category, subcategory))
}

pub(crate) fn apply_taxonomy_result(
    db: &Connection,
    contact: &ContactRecord,
    result: &TaxonomyResult,
    model: &str,
    input_hash: &str,
    catalog_version: &str,
    run_id: &str,
) -> Result<&'static str, String> {
    // Policy: closed-set proposals apply automatically (fill or replace). Only
    // TaxonomyGap creates a pending review for the user to accept or reject.
    db.execute(
        "DELETE FROM classification_follow_up_queue WHERE contact_id=?1",
        params![contact.id],
    )
    .map_err(|error| error.to_string())?;
    let taxonomy_version_id: i64 = db
        .query_row(
            "SELECT id FROM taxonomy_versions WHERE semantic_version=?1",
            params![catalog_version],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let resolved_candidate = if result.outcome == crate::TaxonomyOutcome::Proposal {
        Some(taxonomy_candidate_values_at(
            db,
            result
                .candidates
                .first()
                .ok_or_else(|| "proposta di tassonomia senza candidati".to_owned())?,
            catalog_version,
        )?)
    } else {
        None
    };
    let plan = crate::classification_core::plan_taxonomy_commit(
        &contact.category,
        contact.subcategory.as_deref(),
        result,
        resolved_candidate,
    )?;
    if let crate::classification_core::TaxonomyCommitPlan::Retain {
        state,
        reason,
        evidence,
    } = &plan
    {
        if *state == "insufficient_evidence" || *state == "conflicting" {
            ensure_ai_classification_fallback(db, contact, run_id, false, evidence)?;
        }
        apply_classification_metadata(
            db,
            contact,
            run_id,
            taxonomy_version_id,
            catalog_version,
            state,
            reason.as_deref(),
            input_hash,
            evidence,
        )?;
        return Ok("updated");
    }
    if matches!(
        &plan,
        crate::classification_core::TaxonomyCommitPlan::ProposeTaxonomyGap
    ) {
        ensure_ai_classification_fallback(
            db,
            contact,
            run_id,
            true,
            &result
                .gap
                .as_ref()
                .map(|gap| grounded_evidence_strings(gap.evidence.iter()))
                .unwrap_or_default(),
        )?;
        let gap = result
            .gap
            .as_ref()
            .ok_or_else(|| "proposta di lacuna tassonomica senza dettagli".to_owned())?;
        let proposed_category = gap
            .nearest_category_key
            .as_deref()
            .or(gap.parent_category_key.as_deref())
            .and_then(|key| {
                db.query_row(
                    "SELECT e.display_name FROM taxonomy_entries e
                     JOIN taxonomy_versions v ON v.id=e.taxonomy_version_id
                     WHERE v.status='active' AND e.level='category' AND e.semantic_key=?1",
                    params![key],
                    |r| r.get::<_, String>(0),
                )
                .ok()
            })
            .unwrap_or_else(|| contact.category.clone());
        let proposed_subcategory = Some(gap.proposed_display_name.clone());
        let evidence_quotes: Vec<String> = gap.evidence.iter().map(|e| e.quote.clone()).collect();
        let review_id = new_contact_id();
        let taxonomy_change_json = serde_json::to_string(&result.gap).map_err(|e| e.to_string())?;
        let group_key = crate::taxonomy::taxonomy_gap_group_key(gap);
        let candidate_id: Option<String> = db
            .query_row(
                "SELECT id FROM taxonomy_change_candidates
                 WHERE status='pending' AND based_on_version_id=?1 AND group_key=?2
                 ORDER BY created_at LIMIT 1",
                params![taxonomy_version_id, group_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let candidate_id = candidate_id.unwrap_or_else(|| review_id.clone());

        db.execute(
            "INSERT OR IGNORE INTO taxonomy_change_candidates(
                id,origin_run_id,origin_contact_id,based_on_version_id,change_json,
                group_key,evidence_json,impact_count,status,created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,0,'pending',?8)",
            params![
                candidate_id,
                run_id,
                contact.id,
                taxonomy_version_id,
                taxonomy_change_json,
                group_key,
                serde_json::to_string(&evidence_quotes).map_err(|error| error.to_string())?,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| e.to_string())?;
        db.execute(
            "INSERT OR IGNORE INTO taxonomy_change_candidate_contacts(candidate_id,contact_id)
             VALUES(?1,?2)",
            params![candidate_id, contact.id],
        )
        .map_err(|e| e.to_string())?;
        db.execute(
            "UPDATE taxonomy_change_candidates SET impact_count=(
                SELECT COUNT(*) FROM taxonomy_change_candidate_contacts WHERE candidate_id=?1
             ) WHERE id=?1",
            params![candidate_id],
        )
        .map_err(|e| e.to_string())?;

        db.execute(
            "INSERT INTO classification_reviews(
                id, contact_id, current_category, current_subcategory,
                proposed_category, proposed_subcategory, confidence,
                evidence, model, profile, prompt_version, status,
                outcome, candidates, summary, taxonomy_change,
                taxonomy_change_candidate_id, created_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending',
                'taxonomy_gap', '[]', ?12, ?13, ?14, ?15
            )",
            params![
                review_id,
                contact.id,
                "Varie",
                Some("Ente non determinato"),
                proposed_category,
                proposed_subcategory,
                0.5_f64,
                serde_json::to_string(&evidence_quotes).map_err(|e| e.to_string())?,
                model,
                AI_PROFILE_LABEL,
                catalog_version,
                result.summary,
                taxonomy_change_json,
                candidate_id,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| e.to_string())?;

        apply_classification_metadata(
            db,
            contact,
            run_id,
            taxonomy_version_id,
            catalog_version,
            "taxonomy_gap",
            result.summary.as_deref(),
            input_hash,
            &evidence_quotes,
        )?;
        db.execute(
            "UPDATE contacts SET needs_review=1 WHERE id=?1",
            params![contact.id],
        )
        .map_err(|error| error.to_string())?;

        return Ok("review");
    }

    let crate::classification_core::TaxonomyCommitPlan::Apply {
        category,
        subcategory,
        agreement,
        confidence,
        evidence,
    } = plan
    else {
        return Err("classification plan did not reach a terminal branch".to_owned());
    };
    let subcategory =
        subcategory.or_else(|| crate::taxonomy::fallback_subcategory(&category).map(str::to_owned));
    let category_same = category.eq_ignore_ascii_case(&contact.category);
    let sub_same = subcategory
        .as_deref()
        .zip(contact.subcategory.as_deref())
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b));
    if agreement {
        ensure_ai_classification_fallback(db, contact, run_id, false, &evidence)?;
        let mut provenance: Vec<serde_json::Value> = contact
            .classification_evidence
            .iter()
            .filter_map(|value| serde_json::from_str(value).ok())
            .collect();
        provenance.push(serde_json::json!({"source":"ai_confirmation","model":model,"candidates":result.candidates,"summary":result.summary}));
        db.execute(
            "UPDATE contacts SET classification_evidence=?1 WHERE id=?2",
            params![
                serde_json::to_string(&provenance).map_err(|e| e.to_string())?,
                contact.id
            ],
        )
        .map_err(|e| e.to_string())?;
        apply_classification_metadata(
            db,
            contact,
            run_id,
            taxonomy_version_id,
            catalog_version,
            "classified",
            None,
            input_hash,
            &[],
        )?;
        return Ok("updated");
    }

    if !category_same {
        let new_subcategory = subcategory.clone();
        db.execute(
            "UPDATE contacts SET \
             category=?1, \
             subcategory=?2, \
             category_id=(SELECT id FROM categories WHERE name=?3 AND active=1), \
             subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name=?4 AND s.name=?5 AND s.active=1), \
             classification_method='ai', \
             classification_hash=?6 \
             WHERE id=?7",
            params![
                category,
                new_subcategory,
                category,
                category,
                new_subcategory,
                input_hash,
                contact.id,
            ],
        )
        .map_err(|e| e.to_string())?;
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "category",
                old_value: Some(&contact.category),
                new_value: Some(&category),
                confidence,
                evidence: &evidence,
                pending_review: false,
            },
        )?;
        apply_classification_metadata(
            db,
            contact,
            run_id,
            taxonomy_version_id,
            catalog_version,
            "classified",
            None,
            input_hash,
            &evidence,
        )?;
        if new_subcategory != contact.subcategory {
            record_ai_field_change(
                db,
                run_id,
                contact,
                FieldChange {
                    review_id: None,
                    field_name: "subcategory",
                    old_value: contact.subcategory.as_deref(),
                    new_value: new_subcategory.as_deref(),
                    confidence,
                    evidence: &evidence,
                    pending_review: false,
                },
            )?;
        }
        return Ok("updated");
    }
    if subcategory.is_some() && !sub_same {
        db.execute(
            "UPDATE contacts SET \
             subcategory=?1, \
             subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name=?2 AND s.name=?3 AND s.active=1), \
             classification_method='ai', \
             classification_hash=?4 \
             WHERE id=?5",
            params![
                subcategory,
                contact.category,
                subcategory,
                input_hash,
                contact.id,
            ],
        )
        .map_err(|e| e.to_string())?;
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "subcategory",
                old_value: contact.subcategory.as_deref(),
                new_value: subcategory.as_deref(),
                confidence,
                evidence: &evidence,
                pending_review: false,
            },
        )?;
    }
    apply_classification_metadata(
        db,
        contact,
        run_id,
        taxonomy_version_id,
        catalog_version,
        "classified",
        None,
        input_hash,
        &evidence,
    )?;
    Ok("updated")
}

#[allow(clippy::too_many_arguments)]
fn apply_classification_metadata(
    db: &Connection,
    contact: &ContactRecord,
    run_id: &str,
    taxonomy_version_id: i64,
    catalog_version: &str,
    state: &str,
    reason: Option<&str>,
    input_hash: &str,
    evidence: &[String],
) -> Result<(), String> {
    let old_version = contact.taxonomy_version_id.map(|value| value.to_string());
    let new_version = taxonomy_version_id.to_string();
    if old_version.as_deref() != Some(new_version.as_str()) {
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "taxonomy_version_id",
                old_value: old_version.as_deref(),
                new_value: Some(&new_version),
                confidence: 1.0,
                evidence,
                pending_review: false,
            },
        )?;
    }
    if contact.classification_state != state {
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "classification_state",
                old_value: Some(&contact.classification_state),
                new_value: Some(state),
                confidence: 1.0,
                evidence,
                pending_review: false,
            },
        )?;
    }
    if contact.classification_state_reason.as_deref() != reason {
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "classification_state_reason",
                old_value: contact.classification_state_reason.as_deref(),
                new_value: reason,
                confidence: 1.0,
                evidence,
                pending_review: false,
            },
        )?;
    }
    if contact.classification_hash.as_deref() != Some(input_hash) {
        record_ai_field_change(
            db,
            run_id,
            contact,
            FieldChange {
                review_id: None,
                field_name: "classification_hash",
                old_value: contact.classification_hash.as_deref(),
                new_value: Some(input_hash),
                confidence: 1.0,
                evidence,
                pending_review: false,
            },
        )?;
    }
    db.execute(
        "UPDATE contacts SET taxonomy_version_id=?1,classification_state=?2,
         classification_state_reason=?3,classification_hash=?4,needs_review=CASE WHEN ?2='taxonomy_gap' THEN needs_review ELSE 0 END
         WHERE id=?5",
        params![taxonomy_version_id, state, reason, input_hash, contact.id],
    )
    .map_err(|error| error.to_string())?;
    println!(
        "[classification][metadata] contact_id=\"{}\" taxonomy=\"{}\" state=\"{}\"",
        contact.id, catalog_version, state
    );
    Ok(())
}

pub(crate) fn grounded_evidence_strings<'a>(
    references: impl Iterator<Item = &'a crate::GroundedSourceReference>,
) -> Vec<String> {
    references
        .map(|e| format!("[{} · {}] {}", e.source_id, e.field, e.quote.trim()))
        .collect()
}

#[cfg(test)]
fn canonical_region(value: &str) -> Option<&'static str> {
    REGIONS
        .iter()
        .copied()
        .find(|region| region.eq_ignore_ascii_case(value.trim()))
}

#[cfg(test)]
fn canonical_province(value: &str) -> Option<(&'static str, &'static str)> {
    let normalized = normalize_ai_field("provincia", value.to_owned());
    PROVINCE_REGIONS.iter().copied().find(|(province, _)| {
        province.eq_ignore_ascii_case(&normalized)
            || province
                .replace(['-', '\''], " ")
                .eq_ignore_ascii_case(&normalized)
    })
}

#[cfg(test)]
fn geography_from_known_contacts(
    db: &Connection,
    comune: &str,
) -> Result<Option<(String, String)>, String> {
    let mut stmt = db
        .prepare(
            "SELECT provincia,regione,COUNT(*) AS occurrences
             FROM contacts
             WHERE lower(trim(comune))=lower(trim(?1)) AND provincia IS NOT NULL
             GROUP BY lower(trim(provincia)),lower(trim(regione))
             ORDER BY occurrences DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![comune], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut valid = Vec::new();
    for row in rows {
        let (province, region) = row.map_err(|e| e.to_string())?;
        if let Some((canonical_province, canonical_region)) = canonical_province(&province)
            && region
                .as_deref()
                .and_then(self::canonical_region)
                .is_none_or(|value| value == canonical_region)
            && !valid
                .iter()
                .any(|pair: &(String, String)| pair.0 == canonical_province)
        {
            valid.push((canonical_province.to_owned(), canonical_region.to_owned()));
        }
    }
    Ok((valid.len() == 1).then(|| valid.remove(0)))
}

/// Reconciled `(comune, provincia, regione)`.
#[cfg(test)]
type ReconciledGeography = (Option<String>, Option<String>, Option<String>);

#[cfg(test)]
pub(crate) fn reconcile_ai_geography(
    db: &Connection,
    contact: &ContactRecord,
    suggestion: &AiContactSuggestion,
) -> Result<ReconciledGeography, String> {
    let proposed_comune =
        clean_ai_option(suggestion.comune.clone()).map(|value| normalize_ai_field("comune", value));
    let source_urls = contact
        .source_url
        .as_deref()
        .map(|value| {
            value
                .split('|')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if source_urls.len() > 1
        && proposed_comune.as_deref().is_some_and(|comune| {
            !source_urls
                .iter()
                .all(|url| url.to_lowercase().contains(&comune.to_lowercase()))
        })
    {
        return Ok((None, None, None));
    }

    let inferred = if let Some(comune) = proposed_comune.as_deref() {
        if let Some((province, region)) = canonical_province(comune) {
            Some((province.to_owned(), region.to_owned()))
        } else {
            geography_from_known_contacts(db, comune)?
        }
    } else {
        None
    };
    let proposed_province = clean_ai_option(suggestion.provincia.clone())
        .and_then(|value| canonical_province(&value))
        .map(|(province, region)| (province.to_owned(), region.to_owned()));

    let (province, region) = inferred.or(proposed_province).map_or_else(
        || {
            (
                None,
                clean_ai_option(suggestion.regione.clone())
                    .and_then(|value| canonical_region(&value).map(str::to_owned)),
            )
        },
        |(province, region)| (Some(province), Some(region)),
    );

    let comune = province.as_ref().and(proposed_comune);
    Ok((comune, province, region))
}
