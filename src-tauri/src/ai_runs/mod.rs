#[cfg(test)]
use crate::AiContactSuggestion;
#[cfg(test)]
use crate::TAXONOMY;
use crate::ai_batch::{BatchContactItem, call_provider_batch_unified};
#[cfg(test)]
use crate::ai_enrichment::{clean_ai_option, normalize_ai_field};
use crate::ai_provider::{
    InferenceProvider, ProviderError, ProviderErrorKind, provider_limit_backoff,
};
#[cfg(test)]
use crate::parsing::{PROVINCE_REGIONS, REGIONS};
use crate::taxonomy::load_active_taxonomy;
use crate::{
    APP_SESSION_ID, AiClassificationRequest, AiEnrichmentReport, AiFieldChange, AiFieldChangeList,
    AiProgress, AiRollbackReport, AiRunStatus, ClassificationReview, ClassificationReviewList,
    ContactRecord, TaxonomyCandidate, TaxonomyResult, claim_ai_run, has_table, load_ai_candidate,
    new_contact_id, open_db, renew_ai_lease, simple_hash,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{sync::Arc, time::Duration};
use tauri::Emitter;

mod apply;
mod context;
mod deterministic;
mod lifecycle;
mod reviews;
mod runner;

pub(crate) use apply::*;
pub(crate) use context::*;
pub(crate) use deterministic::*;
pub(crate) use lifecycle::*;
pub(crate) use reviews::*;
pub(crate) use runner::*;

pub(crate) const AI_PROFILE_LABEL: &str = "default";

#[cfg(test)]
mod taxonomy_result_tests {
    use super::{
        AI_PROFILE_LABEL, classification_hash, classification_source_context, clean_source_path,
        evaluate_deterministic, parse_geography_result, parse_taxonomy_result, taxonomy_boundary,
        validate_geography_result, validate_taxonomy_result,
    };
    use rusqlite::{Connection, params};

    #[test]
    fn taxonomy_boundary_parses_insufficient_outcome() {
        let raw = " {\n  \"outcome\":\"insufficient\",\"candidates\":[],\"gap\":null,\"summary\":null\n} ";
        let result = taxonomy_boundary(raw, &[], "[]").unwrap();
        assert_eq!(result.candidates.len(), 0);
        assert_eq!(result.outcome, crate::TaxonomyOutcome::Insufficient);
    }

    #[test]
    fn taxonomy_boundary_validates_proposal_requires_candidates() {
        let invalid = r#"{"outcome":"proposal","candidates":[],"gap":null,"summary":null}"#;
        let errors = taxonomy_boundary(invalid, &[], "[]").unwrap_err();
        assert_eq!(errors, vec!["proposal.candidates.exactly_one"]);
    }

    #[test]
    fn parses_semantic_taxonomy_proposal_with_independent_grades() {
        let result = parse_taxonomy_result(r#"{"outcome":"proposal","candidates":[{"category_key":"education","subcategory_key":"primary","category_grade":"direct","subcategory_grade":"corroborated","category_evidence":[{"source_id":"S1","field":"Name","quote":"Scuola"}],"subcategory_evidence":[]}],"gap":null,"summary":"display only"}"#).unwrap();
        assert_eq!(result.candidates[0].category_key, "education");
        assert!(matches!(
            result.candidates[0].subcategory_grade,
            Some(crate::EvidenceGrade::Corroborated)
        ));
    }

    #[test]
    fn rejects_invalid_hierarchy_and_ungrounded_evidence() {
        let result = parse_taxonomy_result(r#"{"outcome":"proposal","candidates":[{"category_key":"education","subcategory_key":"wrong","category_grade":"weak","subcategory_grade":"direct","category_evidence":[{"source_id":"S1","field":"Name","quote":" missing "}],"subcategory_evidence":[]}],"gap":null,"summary":null}"#).unwrap();
        let errors = validate_taxonomy_result(
            &result,
            &[("education".to_owned(), vec!["primary".to_owned()])],
            r#"[{"source_id":"S1","fields":{"Name":"Scuola"}}]"#,
        )
        .unwrap_err();
        assert!(errors.contains("taxonomy.subcategory.invalid"));
        assert!(errors.contains("evidence.quote.not_found"));
    }

    #[test]
    fn rejects_missing_level_support_and_empty_quotes() {
        let result = parse_taxonomy_result(r#"{"outcome":"proposal","candidates":[{"category_key":"education","subcategory_key":"primary","category_grade":"direct","subcategory_grade":null,"category_evidence":[{"source_id":"S1","field":"Name","quote":"  "}],"subcategory_evidence":[]}],"gap":null,"summary":null}"#).unwrap();
        let errors = validate_taxonomy_result(
            &result,
            &[("education".to_owned(), vec!["primary".to_owned()])],
            r#"[{"source_id":"S1","fields":{"Name":"Scuola primaria"}}]"#,
        )
        .unwrap_err();
        assert!(errors.contains("evidence.quote.empty"));
        assert!(errors.contains("grade.subcategory.required"));
        assert!(errors.contains("evidence.subcategory.required"));
    }

    #[test]
    fn rejects_gap_payload_on_non_gap_outcome() {
        let result = parse_taxonomy_result(r#"{"outcome":"insufficient","candidates":[],"gap":{"level":"category","nearest_category_key":null,"nearest_subcategory_key":null,"proposed_display_name":"Nuovo","proposed_definition":"Definizione","parent_category_key":null,"evidence":[]},"summary":null}"#).unwrap();
        let errors = validate_taxonomy_result(&result, &[], "[]").unwrap_err();
        assert!(errors.contains("gap.unexpected"));
    }

    #[test]
    fn rejects_inactive_gap_keys_and_cross_category_hierarchy() {
        let result = parse_taxonomy_result(r#"{"outcome":"taxonomy_gap","candidates":[],"gap":{"level":"subcategory","nearest_category_key":"education","nearest_subcategory_key":"alpine","proposed_display_name":"Nuovo","proposed_definition":"Definizione","parent_category_key":"missing","evidence":[{"source_id":"S1","field":"Name","quote":"Nuovo"}]},"summary":null}"#).unwrap();
        let errors = validate_taxonomy_result(
            &result,
            &[("education".to_owned(), vec!["primary".to_owned()])],
            r#"[{"source_id":"S1","fields":{"Name":"Nuovo"}}]"#,
        )
        .unwrap_err();
        assert!(errors.contains("taxonomy_gap.nearest_subcategory.invalid"));
        assert!(errors.contains("taxonomy_gap.parent.invalid"));
    }

    #[test]
    fn accepts_case_and_whitespace_normalized_grounding() {
        let result = parse_taxonomy_result(r#"{"outcome":"proposal","candidates":[{"category_key":"education","subcategory_key":null,"category_grade":"direct","subcategory_grade":null,"category_evidence":[{"source_id":"S1","field":"Name","quote":"  SCUOLA  "}],"subcategory_evidence":[]}],"gap":null,"summary":null}"#).unwrap();
        assert!(
            validate_taxonomy_result(
                &result,
                &[("education".to_owned(), vec![])],
                r#"[{"source_id":"S1","fields":{"Name":"Scuola"}}]"#
            )
            .is_ok()
        );
    }

    fn geography_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::setup_schema(&db).unwrap();
        crate::gazetteer::seed(&db).unwrap();
        db
    }

    fn geography_call(
        db: &Connection,
        response: &str,
    ) -> Result<super::GeographicResult, Vec<String>> {
        let gaz = crate::gazetteer::catalog(db, "Italia").unwrap();
        super::geography_boundary(
            response,
            &gaz,
            "Italia",
            r#"[{"source_id":"S1","fields":{"Name":"Comune di Torino, Provincia di Torino; Milano"}}]"#,
        )
    }

    #[test]
    fn deterministic_evaluation_uses_only_reviewed_exact_rules() {
        let res_school = evaluate_deterministic("MIIC8D0002@istruzione.it", None).unwrap();
        assert_eq!(res_school.category, "Istruzione");
        assert_eq!(res_school.subcategory, Some("Istituto comprensivo"));
        assert_eq!(res_school.provincia.as_deref(), Some("Milano"));
        assert_eq!(res_school.regione.as_deref(), Some("Lombardia"));

        let res_prim = evaluate_deterministic("NOEE04500G@pec.istruzione.it", None).unwrap();
        assert_eq!(res_prim.category, "Istruzione");
        assert_eq!(res_prim.subcategory, Some("Primaria"));
        assert_eq!(res_prim.provincia.as_deref(), Some("Novara"));
        assert_eq!(res_prim.regione.as_deref(), Some("Piemonte"));

        let res_comune = evaluate_deterministic("protocollo@comune.cavriago.re.it", None).unwrap();
        assert_eq!(res_comune.category, "Pubblica amministrazione");
        assert_eq!(res_comune.subcategory, Some("Comune"));
        assert_eq!(res_comune.comune.as_deref(), Some("Cavriago"));
        assert_eq!(res_comune.provincia.as_deref(), Some("Reggio Emilia"));
        assert_eq!(res_comune.regione.as_deref(), Some("Emilia Romagna"));

        let res_carab = evaluate_deterministic("stazione@carabinieri.it", None).unwrap();
        assert_eq!(res_carab.category, "Pubblica amministrazione");
        assert_eq!(res_carab.subcategory, Some("Forze dell'ordine"));

        let res_alpini = evaluate_deterministic("segreteria@ana.it", None).unwrap();
        assert_eq!(res_alpini.category, "Associazioni");
        assert_eq!(res_alpini.subcategory, Some("Alpini"));

        let res_proloco = evaluate_deterministic("info@prolocomelle.it", None).unwrap();
        assert_eq!(res_proloco.category, "Turismo e promozione territoriale");
        assert_eq!(res_proloco.subcategory, Some("Pro Loco"));

        assert!(evaluate_deterministic("mario.rossi@hotmail.com", None).is_none());
        assert!(evaluate_deterministic("giovanni@gmail.com", None).is_none());
        assert!(evaluate_deterministic("xprolocoy@example.test", None).is_none());
    }

    #[test]
    fn geography_boundary_accepts_valid_proposal() {
        let db = geography_db();
        let valid = r#"{"outcome":"proposal","claims":[{"observed_place":"Torino","level_hint":"municipality","evidence":[{"source_id":"S1","field":"Name","quote":"Comune di Torino"}]}],"summary":null}"#;
        let attempt = geography_call(&db, valid).unwrap();
        assert_eq!(attempt.outcome, "proposal");
        assert_eq!(attempt.claims.len(), 1);
    }

    #[test]
    fn geography_boundary_validates_unresolvable_place() {
        let db = geography_db();
        let invalid = r#"{"outcome":"proposal","claims":[{"observed_place":"Atlantide","level_hint":"municipality","evidence":[{"source_id":"S1","field":"Name","quote":"Torino"}]}],"summary":null}"#;
        let error = geography_call(&db, invalid).unwrap_err();
        assert!(error.contains(&"geography.place.unresolvable".to_owned()));
    }

    #[test]
    fn geography_pipeline_accepts_resolvable_conflict() {
        let db = geography_db();
        let conflict = r#"{"outcome":"conflict","claims":[{"observed_place":"Torino","level_hint":"municipality","evidence":[{"source_id":"S1","field":"Name","quote":"Torino"}]},{"observed_place":"Milano","level_hint":"municipality","evidence":[{"source_id":"S1","field":"Name","quote":"Milano"}]}],"summary":null}"#;
        assert_eq!(geography_call(&db, conflict).unwrap().outcome, "conflict");
    }

    #[test]
    fn geography_pipeline_accepts_insufficient_without_claims() {
        let db = geography_db();
        let insufficient = r#"{"outcome":"insufficient","claims":[],"summary":null}"#;
        assert_eq!(
            geography_call(&db, insufficient).unwrap().outcome,
            "insufficient"
        );
    }

    #[test]
    fn geography_pipeline_accepts_compatible_multi_claim_proposal() {
        let db = geography_db();
        let proposal = r#"{"outcome":"proposal","claims":[{"observed_place":"Milano","level_hint":"municipality","evidence":[{"source_id":"S1","field":"Name","quote":"Milano"}]},{"observed_place":"Milano","level_hint":"province","evidence":[{"source_id":"S1","field":"Name","quote":"Milano"}]}],"summary":null}"#;
        assert_eq!(geography_call(&db, proposal).unwrap().claims.len(), 2);
    }

    #[test]
    fn geography_contract_requires_grounded_observed_place_and_has_no_country_or_parents() {
        let result = parse_geography_result(r#"{"outcome":"proposal","claims":[{"observed_place":"Torino","level_hint":"comune","evidence":[{"source_id":"S1","field":"Name","quote":"Comune di Torino"}]}],"summary":"display"}"#).unwrap();
        assert!(
            validate_geography_result(
                &result,
                r#"[{"source_id":"S1","fields":{"Name":"Comune di Torino"}}]"#
            )
            .is_ok()
        );
    }

    #[test]
    fn geography_contract_rejects_unquoted_and_no_place_proposals() {
        let result = parse_geography_result(r#"{"outcome":"proposal","claims":[{"observed_place":"Roma","level_hint":null,"evidence":[{"source_id":"S1","field":"Name","quote":"not present"}]}],"summary":null}"#).unwrap();
        let errors = validate_geography_result(
            &result,
            r#"[{"source_id":"S1","fields":{"Name":"Azienda"}}]"#,
        )
        .unwrap_err();
        assert!(errors.contains("evidence.quote.not_found"));
        let insufficient =
            parse_geography_result(r#"{"outcome":"insufficient","claims":[],"summary":"none"}"#)
                .unwrap();
        assert!(validate_geography_result(&insufficient, "[]").is_ok());
    }

    fn sample_contact(id: &str, email: &str, category: &str) -> crate::ContactRecord {
        crate::ContactRecord {
            id: id.to_owned(),
            email: email.to_owned(),
            email_local_part: "test".to_owned(),
            email_domain: "example.com".to_owned(),
            email_tld: ".com".to_owned(),
            status: crate::EmailStatus::Valid,
            validation_notes: Vec::new(),
            source_file: "test.csv".to_owned(),
            category: category.to_owned(),
            subcategory: None,
            contact_kind: "non_determinato".to_owned(),
            paese: "Italia".to_owned(),
            is_pec: false,
            comune: None,
            provincia: None,
            regione: None,
            macroarea: None,
            website: None,
            source_url: None,
            category_id: None,
            subcategory_id: None,
            classification_method: "legacy".to_owned(),
            classification_confidence: 0.0,
            classification_evidence: Vec::new(),
            needs_review: false,
            classification_hash: None,
            taxonomy_version_id: None,
            taxonomy_version: None,
            classification_state: "unprocessed".to_owned(),
            classification_state_reason: None,
        }
    }

    #[test]
    fn taxonomy_apply_persists_review_for_taxonomy_gap() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::schema::setup_schema(&db).unwrap();
        let contact = sample_contact("contact-gap-1", "test@example.com", "Istruzione");
        crate::contacts::insert_contact_record(&db, &contact).unwrap();
        db.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at
             ) VALUES('run-gap','test-model','default','taxonomy-v1','running',1,'now')",
            [],
        )
        .unwrap();

        let result = crate::TaxonomyResult {
            outcome: crate::TaxonomyOutcome::TaxonomyGap,
            candidates: vec![],
            gap: Some(crate::TaxonomyGap {
                level: "subcategory".into(),
                nearest_category_key: Some("istruzione".into()),
                nearest_subcategory_key: None,
                proposed_display_name: "Scuola di Robotica".into(),
                proposed_definition: "Corsi di robotica e automazione".into(),
                parent_category_key: Some("istruzione".into()),
                evidence: vec![crate::GroundedSourceReference {
                    source_id: "S1".into(),
                    field: "Note".into(),
                    quote: "laboratorio robotica".into(),
                }],
            }),
            summary: Some("Rilevata lacuna tassonomica per robotica".into()),
        };

        let status = super::apply_taxonomy_result(
            &db,
            &contact,
            &result,
            "test-model",
            "hash123",
            "taxonomy-v1",
            "run-gap",
        )
        .unwrap();

        assert_eq!(status, "review");

        type ReviewRow = (
            String,
            String,
            String,
            String,
            f64,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        );
        let review: ReviewRow = db.query_row(
            "SELECT contact_id, current_category, proposed_category, proposed_subcategory, confidence, evidence, model, profile, prompt_version, outcome, summary, taxonomy_change FROM classification_reviews WHERE contact_id=?1",
            rusqlite::params![contact.id],
            |r| Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?,
                r.get(10)?, r.get(11)?
            )),
        ).unwrap();

        assert_eq!(review.0, "contact-gap-1");
        assert_eq!(review.1, "Varie");
        assert_eq!(review.2, "Istruzione");
        assert_eq!(review.3, "Scuola di Robotica");
        assert_eq!(review.4, 0.5);
        assert!(review.5.contains("laboratorio robotica"));
        assert_eq!(review.6, "test-model");
        assert_eq!(review.7, AI_PROFILE_LABEL);
        assert_eq!(review.8, "taxonomy-v1");
        assert_eq!(review.9, "taxonomy_gap");
        assert_eq!(
            review.10.as_deref(),
            Some("Rilevata lacuna tassonomica per robotica")
        );
        assert!(review.11.unwrap().contains("Scuola di Robotica"));

        let needs_review: i64 = db
            .query_row(
                "SELECT needs_review FROM contacts WHERE id=?1",
                rusqlite::params![contact.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(needs_review, 1);

        let review_id: String = db
            .query_row(
                "SELECT id FROM classification_reviews WHERE contact_id=?1",
                rusqlite::params![contact.id],
                |row| row.get(0),
            )
            .unwrap();
        super::approve_classification_review_conn(&mut db, &review_id, None, None).unwrap();
        let active: String = db
            .query_row(
                "SELECT semantic_version FROM taxonomy_versions WHERE status='active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, "taxonomy-v2");
        let assigned: (String, Option<String>, String, String, i64) = db
            .query_row(
                "SELECT category,subcategory,classification_state,
                        (SELECT semantic_version FROM taxonomy_versions WHERE id=contacts.taxonomy_version_id),
                        needs_review
                 FROM contacts WHERE id=?1",
                rusqlite::params![contact.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            assigned,
            (
                "Istruzione".to_owned(),
                Some("Scuola di Robotica".to_owned()),
                "classified".to_owned(),
                "taxonomy-v2".to_owned(),
                0,
            )
        );

        let mut rejected_contact = contact.clone();
        rejected_contact.id = "contact-gap-2".to_owned();
        rejected_contact.email = "second@example.com".to_owned();
        crate::contacts::insert_contact_record(&db, &rejected_contact).unwrap();
        db.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at
             ) VALUES('run-gap-2','test-model','default','taxonomy-v2','running',1,'now')",
            [],
        )
        .unwrap();
        super::apply_taxonomy_result(
            &db,
            &rejected_contact,
            &result,
            "test-model",
            "hash456",
            "taxonomy-v2",
            "run-gap-2",
        )
        .unwrap();
        let rejected_review_id: String = db
            .query_row(
                "SELECT id FROM classification_reviews WHERE contact_id=?1 AND status='pending'",
                rusqlite::params![rejected_contact.id],
                |row| row.get(0),
            )
            .unwrap();
        super::reject_classification_review_conn(&mut db, &rejected_review_id).unwrap();
        let rejected: (String, String, String, i64) = db
            .query_row(
                "SELECT r.status,c.status,
                        (SELECT classification_state FROM contacts WHERE id=r.contact_id),
                        (SELECT needs_review FROM contacts WHERE id=r.contact_id)
                 FROM classification_reviews r
                 JOIN taxonomy_change_candidates c ON c.id=r.id WHERE r.id=?1",
                rusqlite::params![rejected_review_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            rejected,
            (
                "rejected".to_owned(),
                "rejected".to_owned(),
                "classified".to_owned(),
                0
            )
        );

        db.execute(
            "UPDATE ai_classification_runs SET status='completed' WHERE id IN('run-gap','run-gap-2')",
            [],
        )
        .unwrap();
        let rollback = super::rollback_ai_scope_conn(&mut db, "beginning").unwrap();
        assert_eq!(rollback.runs_rolled_back, 2);
        let surviving_taxonomy: (String, i64) = db
            .query_row(
                "SELECT v.semantic_version,
                        EXISTS(SELECT 1 FROM taxonomy_entries e
                               WHERE e.taxonomy_version_id=v.id
                                 AND e.display_name='Scuola di Robotica')
                 FROM taxonomy_versions v WHERE v.status='active'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(surviving_taxonomy, ("taxonomy-v2".to_owned(), 1));
    }

    #[test]
    fn taxonomy_proposal_applies_automatically_without_review() {
        let db = Connection::open_in_memory().unwrap();
        crate::schema::setup_schema(&db).unwrap();
        let contact = sample_contact("contact-auto-1", "auto@example.test", "Varie");
        crate::contacts::insert_contact_record(&db, &contact).unwrap();
        db.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at
             ) VALUES('run-auto','test-model','default','taxonomy-v1','running',1,'now')",
            [],
        )
        .unwrap();
        let result = crate::TaxonomyResult {
            outcome: crate::TaxonomyOutcome::Proposal,
            candidates: vec![crate::TaxonomyCandidate {
                category_key: "istruzione".into(),
                subcategory_key: Some("primaria".into()),
                category_grade: crate::EvidenceGrade::Direct,
                subcategory_grade: Some(crate::EvidenceGrade::Direct),
                category_evidence: vec![crate::GroundedSourceReference {
                    source_id: "S1".into(),
                    field: "Evidence".into(),
                    quote: "Scuola Primaria".into(),
                }],
                subcategory_evidence: vec![crate::GroundedSourceReference {
                    source_id: "S1".into(),
                    field: "Evidence".into(),
                    quote: "Scuola Primaria".into(),
                }],
            }],
            gap: None,
            summary: Some("Closed-set apply".into()),
        };
        let status = super::apply_taxonomy_result(
            &db,
            &contact,
            &result,
            "test-model",
            "hash-auto",
            "taxonomy-v1",
            "run-auto",
        )
        .unwrap();
        assert_eq!(status, "updated");
        let reviews: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM classification_reviews WHERE contact_id=?1",
                rusqlite::params![contact.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reviews, 0);
        let assigned: (String, Option<String>, i64) = db
            .query_row(
                "SELECT category,subcategory,needs_review FROM contacts WHERE id=?1",
                rusqlite::params![contact.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            assigned,
            ("Istruzione".to_owned(), Some("Primaria".to_owned()), 0)
        );
        let kinds: Vec<String> = db
            .prepare(
                "SELECT change_kind FROM ai_field_changes
                 WHERE contact_id=?1 ORDER BY field_name",
            )
            .unwrap()
            .query_map(rusqlite::params![contact.id], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            kinds
                .iter()
                .all(|kind| kind == "aggiunto" || kind == "modificato")
        );
        assert!(!kinds.iter().any(|kind| kind == "in_revisione"));
    }

    #[test]
    fn classification_hash_is_shared_across_fill_and_audit_runs() {
        let contact = sample_contact("c1", "test@example.com", "Associazioni");
        let input = r#"[{"source_id":"S1","source_path":"Associazioni/test.xlsx","fields":{"Nome":"Pro Loco"}}]"#;
        let fill_hash =
            classification_hash(&contact, "qwen2.5:7b", false, 4096, "fill", "1.0", input);
        let audit_hash =
            classification_hash(&contact, "qwen2.5:7b", false, 4096, "audit", "1.0", input);
        assert_eq!(
            fill_hash, audit_hash,
            "Fill and Audit must produce identical classification hashes on the same input"
        );
        assert!(fill_hash.starts_with("ai:"));

        let geo_hash = classification_hash(
            &contact,
            "qwen2.5:7b",
            false,
            4096,
            "geography",
            "1.0",
            input,
        );
        assert_ne!(
            fill_hash, geo_hash,
            "Geography must have its own policy hash"
        );
        assert!(geo_hash.starts_with("ai:"));
    }

    #[test]
    fn classification_source_context_cleans_paths_deduplicates_and_filters_internals() {
        let mut db = Connection::open_in_memory().unwrap();
        crate::schema::migrate_database(&mut db, None, false).unwrap();

        db.execute(
            "INSERT INTO imports(id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at)
             VALUES ('imp1','Proloco.xlsx','Associazioni/Proloco.xlsx','hash1','completed',1,1,'2026-01-01')",
            [],
        ).unwrap();

        let contact_id = "c1";
        db.execute(
            "INSERT INTO contacts(id, email, email_local_part, email_domain, email_tld, status, validation_notes,
             source_file, category, subcategory, contact_kind, paese, is_pec)
             VALUES (?1, 'info@example.test', 'info', 'example.test', '.test', 'valid', '[]',
             'Associazioni/Proloco.xlsx', 'Associazioni', 'Pro Loco', 'istituzionale', 'Italia', 0)",
            params![contact_id],
        ).unwrap();

        let raw1 = serde_json::json!({
            "Denominazione": "  Pro Loco Esempio  ",
            "Telefono": "123456",
            "__role_referente_1": "Mario Rossi",
            "row_number": 42,
            "id": "row-42",
            "category": "Associazioni",
            "comune": "Torino"
        })
        .to_string();
        db.execute(
            "INSERT INTO contact_sources(id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,source_url)
             VALUES ('cs1',?1,'imp1','/Users/user/data/email/Associazioni/Proloco.xlsx','Sheet1',1,?2,'https://example.test')",
            params![contact_id, raw1],
        ).unwrap();

        let raw2 = serde_json::json!({
            "Denominazione": "Pro Loco Esempio",
            "Telefono": "123456",
            "comune": "Torino",
            "__role_presidente_2": "Luigi Verdi"
        })
        .to_string();
        db.execute(
            "INSERT INTO contact_sources(id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,source_url)
             VALUES ('cs2',?1,'imp1','/Users/user/data/email/Associazioni/Proloco.xlsx','Sheet1',2,?2,'https://example.test')",
            params![contact_id, raw2],
        ).unwrap();

        let raw3 = serde_json::json!({
            "__role_segretario": "Giovanni",
            "status": "valid",
            "validation_notes": ""
        })
        .to_string();
        db.execute(
            "INSERT INTO contact_sources(id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,source_url)
             VALUES ('cs3',?1,'imp1','/archive/data/email/Associazioni/Altro.xlsx','Sheet1',3,?2,null)",
            params![contact_id, raw3],
        ).unwrap();

        db.execute(
            "INSERT INTO classification_claims(id,contact_id,source_row_id,category,subcategory,confidence,method,evidence_json,created_at)
             VALUES ('claim1',?1,null,'Associazioni','Pro Loco',0.9,'heuristic','[]','2026-01-01')",
            params![contact_id],
        ).unwrap();

        let context_json = classification_source_context(&db, contact_id).unwrap();
        let records: Vec<serde_json::Value> = serde_json::from_str(&context_json).unwrap();

        assert_eq!(records.len(), 1);
        let r0 = &records[0];
        assert_eq!(r0["source_id"].as_str().unwrap(), "cs1");
        assert_eq!(
            r0["source_path"].as_str().unwrap(),
            "Associazioni/Proloco.xlsx"
        );
        assert!(
            r0.get("source_classifications").is_none(),
            "source_classifications must be stripped"
        );

        let fields = r0["fields"].as_object().unwrap();
        assert_eq!(
            fields.get("Denominazione").unwrap().as_str().unwrap(),
            "Pro Loco Esempio"
        );
        assert_eq!(fields.get("Telefono").unwrap().as_str().unwrap(), "123456");
        assert!(!fields.contains_key("__role_referente_1"));
        assert!(!fields.contains_key("row_number"));
        assert!(!fields.contains_key("id"));
        assert!(!fields.contains_key("category"));
        assert_eq!(fields.get("Comune").unwrap().as_str().unwrap(), "Torino");
    }

    #[test]
    fn clean_source_path_handles_various_formats() {
        assert_eq!(
            clean_source_path("/data/email/Associazioni/Proloco.xlsx"),
            "Associazioni/Proloco.xlsx"
        );
        assert_eq!(
            clean_source_path("C:\\data\\email\\Scuole\\istituti.csv"),
            "Scuole/istituti.csv"
        );
        assert_eq!(
            clean_source_path("/archive/data/email/email/Proloco.xlsx"),
            "Proloco.xlsx"
        );
        assert_eq!(
            clean_source_path("folder\\Alpini 1.csv"),
            "folder/Alpini 1.csv"
        );
        assert_eq!(clean_source_path("Proloco.xlsx"), "Proloco.xlsx");
        assert_eq!(clean_source_path(""), "");
    }
}
