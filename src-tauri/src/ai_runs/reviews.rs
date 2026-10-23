//! Taxonomy-gap reviews awaiting user approval.

use super::*;

#[tauri::command]
pub(crate) async fn list_classification_reviews(
    app: tauri::AppHandle,
    status: Option<String>,
) -> Result<ClassificationReviewList, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let wanted = status.unwrap_or_else(|| "pending".to_owned());
        Ok(ClassificationReviewList {
            reviews: load_classification_reviews(&db, &wanted)?,
            total: count_classification_reviews(&db, &wanted)?,
        })
    })
    .await
}

const REVIEW_FILTER: &str = "r.status=?1 AND r.outcome='taxonomy_gap'";

pub(crate) fn count_classification_reviews(db: &Connection, wanted: &str) -> Result<usize, String> {
    db.query_row(
        &format!(
            "SELECT COUNT(*) FROM classification_reviews r JOIN contacts c ON c.id=r.contact_id
             WHERE {REVIEW_FILTER}"
        ),
        params![wanted],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count as usize)
    .map_err(|e| e.to_string())
}

/// Oldest reviews first, bounded to one page like every other list.
pub(crate) fn load_classification_reviews(
    db: &Connection,
    wanted: &str,
) -> Result<Vec<ClassificationReview>, String> {
    let mut stmt = db.prepare(&format!(
        "SELECT r.id,r.contact_id,c.email,r.current_category,r.current_subcategory,r.proposed_category,
         r.proposed_subcategory,r.confidence,r.evidence,r.model,r.profile,r.status,
         r.outcome,r.candidates,r.summary,r.taxonomy_change
         FROM classification_reviews r JOIN contacts c ON c.id=r.contact_id
         WHERE {REVIEW_FILTER} ORDER BY r.created_at LIMIT ?2"
    )).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![wanted, crate::MAX_PAGE_SIZE as i64], |row| {
            let evidence: String = row.get(8)?;
            Ok(ClassificationReview {
                id: row.get(0)?,
                contact_id: row.get(1)?,
                email: row.get(2)?,
                current_category: row.get(3)?,
                current_subcategory: row.get(4)?,
                proposed_category: row.get(5)?,
                proposed_subcategory: row.get(6)?,
                confidence: row.get(7)?,
                evidence: serde_json::from_str(&evidence).unwrap_or_default(),
                model: row.get(9)?,
                profile: row.get(10)?,
                status: row.get(11)?,
                outcome: row.get(12)?,
                candidates: serde_json::from_str(&row.get::<_, String>(13)?).unwrap_or_default(),
                summary: row.get(14)?,
                taxonomy_change: row
                    .get::<_, Option<String>>(15)?
                    .and_then(|v| serde_json::from_str(&v).ok()),
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// The taxonomy assignment a reviewer approved.
struct ApprovedTaxonomy<'a> {
    category: &'a str,
    subcategory: &'a str,
    evidence: &'a str,
}

fn apply_approved_taxonomy_to_contact(
    tx: &rusqlite::Transaction<'_>,
    contact_id: &str,
    run_id: &str,
    review_id: &str,
    version_id: i64,
    now: &str,
    approved: ApprovedTaxonomy<'_>,
) -> Result<(), String> {
    let ApprovedTaxonomy {
        category,
        subcategory,
        evidence,
    } = approved;
    let before: (String, Option<String>, Option<i64>, String, Option<String>) = tx
        .query_row(
            "SELECT category,subcategory,taxonomy_version_id,classification_state,
                    classification_state_reason
             FROM contacts WHERE id=?1",
            params![contact_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE contacts SET
            category=?1,subcategory=?2,taxonomy_version_id=?3,
            classification_state='classified',classification_state_reason=NULL,
            classification_method='ai',classification_confidence=1,
            classification_evidence=?4,needs_review=0,
            category_id=(SELECT id FROM categories WHERE name=?1 AND active=1),
            subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id
                            WHERE c.name=?1 AND s.name=?2 AND s.active=1)
         WHERE id=?5",
        params![category, subcategory, version_id, evidence, contact_id],
    )
    .map_err(|error| error.to_string())?;
    if has_table(tx, "email_candidates")? {
        tx.execute(
            "UPDATE email_candidates SET category=?1,subcategory=?2 WHERE contact_id=?3",
            params![category, subcategory, contact_id],
        )
        .map_err(|error| error.to_string())?;
    }
    let changes = [
        ("category", Some(before.0), Some(category.to_owned())),
        ("subcategory", before.1, Some(subcategory.to_owned())),
        (
            "taxonomy_version_id",
            before.2.map(|value| value.to_string()),
            Some(version_id.to_string()),
        ),
        (
            "classification_state",
            Some(before.3),
            Some("classified".to_owned()),
        ),
        ("classification_state_reason", before.4, None),
    ];
    for (field, old_value, new_value) in changes {
        if normalized_history_value(old_value.as_deref())
            == normalized_history_value(new_value.as_deref())
        {
            continue;
        }
        tx.execute(
            "INSERT INTO ai_field_changes(
                id,run_id,contact_id,review_id,field_name,old_value,new_value,
                change_kind,status,confidence,evidence,created_at,resolved_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,
                CASE WHEN ?6 IS NULL OR trim(?6)='' THEN 'aggiunto' ELSE 'modificato' END,
                'applied',1,?8,?9,?9)",
            params![
                new_contact_id(),
                run_id,
                contact_id,
                review_id,
                field,
                old_value,
                new_value,
                evidence,
                now
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn approve_classification_review_conn(
    db: &mut Connection,
    review_id: &str,
    _category: Option<String>,
    _subcategory: Option<String>,
) -> Result<(), String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| error.to_string())?;
    let (candidate_id, origin_run_id, outcome): (String, String, String) = tx
        .query_row(
            "SELECT coalesce(r.taxonomy_change_candidate_id,r.id),c.origin_run_id,r.outcome
             FROM classification_reviews r
             JOIN taxonomy_change_candidates c ON c.id=coalesce(r.taxonomy_change_candidate_id,r.id)
             WHERE r.id=?1 AND r.status='pending' AND c.status='pending'",
            params![review_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| error.to_string())?;
    if outcome != "taxonomy_gap" {
        return Err("Solo le proposte di tassonomia richiedono approvazione".to_owned());
    }
    let approval = crate::taxonomy::approve_change_candidate(&tx, &candidate_id)?;
    let now = chrono::Utc::now().to_rfc3339();
    match approval {
        crate::taxonomy::CandidateApproval::Stale => {
            tx.execute(
                "UPDATE contacts SET needs_review=0
                 WHERE id IN (SELECT contact_id FROM taxonomy_change_candidate_contacts WHERE candidate_id=?1)",
                params![candidate_id],
            )
            .map_err(|error| error.to_string())?;
        }
        crate::taxonomy::CandidateApproval::Applied {
            version_id,
            semantic_version,
            category,
            subcategory,
        } => {
            tx.execute(
                "UPDATE taxonomy_change_candidates
                 SET status='stale',decided_at=?1
                 WHERE status='pending' AND based_on_version_id<>?2",
                params![now, version_id],
            )
            .map_err(|error| error.to_string())?;
            tx.execute(
                "UPDATE classification_reviews SET status='rejected',resolved_at=?1
                 WHERE status='pending' AND coalesce(taxonomy_change_candidate_id,id) IN(
                    SELECT id FROM taxonomy_change_candidates WHERE status='stale'
                 )",
                params![now],
            )
            .map_err(|error| error.to_string())?;
            let linked_contacts: Vec<String> = tx
                .prepare(
                    "SELECT contact_id FROM taxonomy_change_candidate_contacts
                     WHERE candidate_id=?1 ORDER BY contact_id",
                )
                .map_err(|error| error.to_string())?
                .query_map(params![candidate_id], |row| row.get(0))
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            let assigned_subcategory = subcategory
                .or_else(|| crate::taxonomy::fallback_subcategory(&category).map(str::to_owned))
                .unwrap_or_else(|| "Altro ente".to_owned());
            let evidence = serde_json::json!([format!(
                "Proposta di tassonomia approvata: {semantic_version}"
            )])
            .to_string();
            for linked_contact_id in linked_contacts {
                let linked_review_id: String = tx
                    .query_row(
                        "SELECT id FROM classification_reviews
                         WHERE contact_id=?1 AND coalesce(taxonomy_change_candidate_id,id)=?2
                         ORDER BY created_at DESC LIMIT 1",
                        params![linked_contact_id, candidate_id],
                        |row| row.get(0),
                    )
                    .unwrap_or_else(|_| review_id.to_owned());
                apply_approved_taxonomy_to_contact(
                    &tx,
                    &linked_contact_id,
                    &origin_run_id,
                    &linked_review_id,
                    version_id,
                    &now,
                    ApprovedTaxonomy {
                        category: &category,
                        subcategory: &assigned_subcategory,
                        evidence: &evidence,
                    },
                )?;
            }
            tx.execute(
                "UPDATE classification_reviews
                 SET status='approved',resolved_at=?1
                 WHERE coalesce(taxonomy_change_candidate_id,id)=?2 AND status='pending'",
                params![now, candidate_id],
            )
            .map_err(|error| error.to_string())?;
            tx.execute(
                "UPDATE contacts SET needs_review=EXISTS(
                    SELECT 1 FROM taxonomy_change_candidates c
                    WHERE c.origin_contact_id=contacts.id AND c.status='pending'
                 )",
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }
    tx.commit().map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn approve_classification_review(
    app: tauri::AppHandle,
    review_id: String,
    category: Option<String>,
    subcategory: Option<String>,
) -> Result<(), String> {
    crate::database::blocking(move || {
        let mut db = open_db(&app)?;
        approve_classification_review_conn(&mut db, &review_id, category, subcategory)
    })
    .await
}

#[cfg(test)]
pub(crate) fn discard_pending_classification_reviews_conn(
    db: &mut Connection,
) -> Result<usize, String> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let discarded = tx
        .execute(
            "DELETE FROM classification_reviews WHERE status='pending'",
            [],
        )
        .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE ai_field_changes SET status='rejected',resolved_at=?1 WHERE status='pending_review'",
        params![chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE contacts SET needs_review=0
         WHERE needs_review=1
           AND NOT EXISTS(SELECT 1 FROM classification_reviews WHERE contact_id=contacts.id AND status='pending')",
        [],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(discarded)
}

pub(crate) fn reject_classification_review_conn(
    db: &mut Connection,
    review_id: &str,
) -> Result<(), String> {
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let candidate_id: String = tx
        .query_row(
            "SELECT coalesce(taxonomy_change_candidate_id,id)
             FROM classification_reviews WHERE id=?1",
            params![review_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "UPDATE classification_reviews SET status='rejected',resolved_at=?1
         WHERE coalesce(taxonomy_change_candidate_id,id)=?2 AND status='pending'",
        params![now, candidate_id],
    )
    .map_err(|e| e.to_string())?;
    let active_version_id: i64 = tx
        .query_row(
            "SELECT id FROM taxonomy_versions WHERE status='active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE contacts SET
            needs_review=0,
            taxonomy_version_id=?1,
            classification_state='classified',
            classification_state_reason='Gap tassonomico rifiutato: mantenuta la classificazione generica'
         WHERE id IN (SELECT contact_id FROM taxonomy_change_candidate_contacts WHERE candidate_id=?2)",
        params![active_version_id, candidate_id],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE taxonomy_change_candidates SET status='rejected',decided_at=?1
         WHERE id=?2 AND status='pending'",
        params![now, candidate_id],
    )
    .map_err(|error| error.to_string())?;

    tx.commit().map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn reject_classification_review(
    app: tauri::AppHandle,
    review_id: String,
) -> Result<(), String> {
    crate::database::blocking(move || {
        let mut db = open_db(&app)?;
        reject_classification_review_conn(&mut db, &review_id)
    })
    .await
}
