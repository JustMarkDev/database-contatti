use crate::normalize_key;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;

mod ai;
mod baseline;
mod contacts;
mod ingestion;
mod legacy;
mod taxonomy;

pub(crate) use ai::*;
use baseline::*;
pub(crate) use contacts::*;
pub(crate) use ingestion::*;
pub(crate) use legacy::*;
pub use taxonomy::*;

const MIGRATION_NAMES: [&str; LATEST_SCHEMA_VERSION as usize] = [
    "baseline-schema",
    "taxonomy-state-varie",
    "field-locks",
    "gazetteer",
    "artifacts-provenance",
    "reviewed-benchmark",
    "legacy-taxonomy-links",
    "obsolete-contact-columns",
    "source-url-backfill",
    "ai-run-leases",
    "review-outcomes",
    "final-integrity",
    "versioned-administrative-catalog",
    "contact-fk-cascade-indexes",
    "lossless-ingestion-ledger",
    "contact-source-import-identity",
    "dns-ttl-evidence",
    "mailbox-verification-queue",
    "consent-metadata-columns",
    "ingestion-v15-schema-repair",
    "ingestion-performance-indexes",
    "pec-flag-unification",
    "geographic-sanitization",
    "macroarea-geografica",
    "compact-unrecoverable-candidates",
    "geography-hash-and-security-taxonomy",
    "deterministic-raw-context-backfill",
    "expanded-domain-taxonomy",
    "ai-run-batch-retries",
    "claims-facet-covering-indexes",
    "ai-rollback-support",
    "classification-architecture-v2",
    "provider-runner-and-deterministic-catalog",
    "retire-opencode-runs",
    "benchmark-lifecycle",
    "reset-classification-hashes",
    "remove-obsolete-geography-hash",
    "canonical-subcategory-labels",
    "classification-subcategory-fallbacks",
    "taxonomy-candidate-grouping",
    "classification-system-provenance",
    "classification-follow-up-queue-repair",
    "app-settings-and-ai-providers",
    "list-search-performance",
    "deterministic-catalog-v1.1",
    "deterministic-catalog-v1.2",
    "sandboxed-web-discovery",
    "lead-source-catalog",
    "macroarea-from-regione",
    "lead-source-codes",
    "drop-compliance-columns",
    "dns-cache-per-domain",
    "pec-is-pec-only",
    "exportable-contact-projection",
    "pec-label-detection",
    "deterministic-catalog-v1.3",
    "classification-audit-repairs",
    "lead-sandbox-mirror",
    "canonical-province-names",
    "contact-quality-score",
    "contact-sort-indexes",
    "classification-sort-by-method",
    "contact-score-without-confidence",
    "contact-score-richness",
    "promoted-contact-status",
    "contextdev-credits",
    "apt-subcategory-label",
    "retire-personal-and-sensitive-subcategories",
    "remove-sanita-contacts-keep-caritas",
];

pub(crate) fn run_ordered_migrations(db: &Connection, fail_at: Option<i64>) -> Result<(), String> {
    db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY,name TEXT NOT NULL UNIQUE,applied_at TEXT NOT NULL);").map_err(|e| e.to_string())?;
    for (offset, name) in MIGRATION_NAMES.iter().enumerate() {
        let version = offset as i64 + 1;
        let applied: Option<String> = db
            .query_row(
                "SELECT name FROM schema_migrations WHERE version=?1",
                params![version],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(applied) = applied {
            if applied != *name {
                return Err(format!("migration {version} name mismatch: {applied}"));
            }
            continue;
        }
        let rebuild_ingestion_tables = version == 20 && ingestion_v15_schema_rebuild_needed(db)?;
        if rebuild_ingestion_tables {
            if !db.is_autocommit() {
                return Err(
                    "migration 20 requires an autocommit connection to rebuild ingestion tables"
                        .to_owned(),
                );
            }
            db.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA legacy_alter_table=ON;")
                .map_err(|error| error.to_string())?;
        }
        if let Err(error) = db.execute_batch("SAVEPOINT ordered_schema_migration") {
            if rebuild_ingestion_tables {
                let _ = db.execute_batch("PRAGMA legacy_alter_table=OFF; PRAGMA foreign_keys=ON;");
            }
            return Err(error.to_string());
        }
        let result = apply_migration(db, version).and_then(|_| {
            if fail_at == Some(version) {
                return Err(format!("injected migration failure at {name}"));
            }
            db.execute(
                "INSERT INTO schema_migrations(version,name,applied_at) VALUES(?1,?2,?3)",
                params![version, name, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
            // Each migration commits on its own, so the envelope version moves with
            // its marker. Otherwise a later failure leaves user_version behind the
            // committed markers and migrate_database would re-run them.
            let envelope: i64 = db
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .map_err(|e| e.to_string())?;
            if envelope < version {
                db.pragma_update(None, "user_version", version)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        if let Err(error) = result {
            let _ = db.execute_batch(
                "ROLLBACK TO ordered_schema_migration; RELEASE ordered_schema_migration",
            );
            if rebuild_ingestion_tables {
                let _ = db.execute_batch("PRAGMA legacy_alter_table=OFF; PRAGMA foreign_keys=ON;");
            }
            return Err(error);
        }
        if let Err(error) = db.execute_batch("RELEASE ordered_schema_migration") {
            if rebuild_ingestion_tables {
                let _ = db.execute_batch("PRAGMA legacy_alter_table=OFF; PRAGMA foreign_keys=ON;");
            }
            return Err(error.to_string());
        }
        if rebuild_ingestion_tables {
            db.execute_batch("PRAGMA legacy_alter_table=OFF; PRAGMA foreign_keys=ON;")
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn apply_migration(db: &Connection, version: i64) -> Result<(), String> {
    match version {
        1 => install_baseline_schema(db),
        2 => { migrate_contact_columns(db)?; migrate_taxonomy_schema(db)?; seed_taxonomy(db) },
        3 => migrate_field_lock_columns(db),
        4 => migrate_gazetteer(db),
        5 => { migrate_contact_columns(db)?; migrate_ai_run_columns(db) },
        6 => crate::benchmark::persist_reviewed_benchmark(db),
        7 => migrate_legacy_taxonomy(db),
        8 => drop_obsolete_contact_columns(db),
        9 => db.execute("UPDATE contacts SET source_url=website WHERE (source_url IS NULL OR trim(source_url)='') AND website IS NOT NULL AND trim(website)<>''", []).map(|_| ()).map_err(|e| e.to_string()),
        10 => { migrate_ai_run_columns(db)?; if !table_columns(db,"ai_classification_runs")?.contains(&"mode".to_owned()) { db.execute("ALTER TABLE ai_classification_runs ADD COLUMN mode TEXT NOT NULL DEFAULT 'fill'",[]).map_err(|e|e.to_string())?; } Ok(()) },
        11 => migrate_review_columns(db),
        12 => db.execute_batch("CREATE INDEX IF NOT EXISTS idx_contacts_contact_kind ON contacts(contact_kind); CREATE INDEX IF NOT EXISTS idx_contacts_paese ON contacts(paese);").map_err(|e|e.to_string()),
        13 => { migrate_gazetteer_catalog(db)?; crate::gazetteer::seed(db) },
        14 => db.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_reviews_contact ON classification_reviews(contact_id);
             CREATE INDEX IF NOT EXISTS idx_ai_provenance_contact ON ai_applied_provenance(contact_id);
             CREATE INDEX IF NOT EXISTS idx_ai_candidates_contact ON ai_run_candidates(contact_id);
             CREATE INDEX IF NOT EXISTS idx_ai_field_changes_contact ON ai_field_changes(contact_id);
             CREATE INDEX IF NOT EXISTS idx_ai_field_changes_run ON ai_field_changes(run_id);
             CREATE INDEX IF NOT EXISTS idx_ai_artifacts_contact ON ai_run_artifacts(contact_id);",
        ).map_err(|e| e.to_string()),
        15 => migrate_lossless_ingestion_schema(db),
        16 => migrate_contact_source_occurrences(db),
        17 => migrate_dns_ttl_evidence(db),
        18 => migrate_mailbox_verification_schema(db),
        19 => migrate_consent_metadata_columns(db),
        20 => migrate_ingestion_v15_schema_repair(db),
        21 => db.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_verification_candidate_method
                 ON verification_events(candidate_id, method);
             CREATE INDEX IF NOT EXISTS idx_classification_claims_source_row
                 ON classification_claims(source_row_id);
             CREATE INDEX IF NOT EXISTS idx_quality_scores_contact_version
                 ON quality_score_snapshots(contact_id, score_version);",
        ).map_err(|error| error.to_string()),
        22 => migrate_pec_flag_unification(db),
        23 => migrate_geographic_sanitization(db),
        24 => migrate_macroarea_field(db),
        25 => migrate_compact_unrecoverable_candidates(db),
        26 => migrate_geography_hash_and_security_taxonomy(db),
        27 => migrate_deterministic_raw_context_backfill(db),
        28 => migrate_expanded_domain_taxonomy(db),
        29 => migrate_ai_run_batch_retries(db),
        30 => migrate_claims_facet_indexes(db),
        31 => migrate_ai_rollback_support(db),
        32 => migrate_classification_architecture_v2(db),
        33 => migrate_provider_runner_and_deterministic_catalog(db),
        34 => migrate_retired_opencode_runs(db),
        35 => migrate_benchmark_lifecycle(db),
        36 => migrate_reset_classification_hashes(db),
        37 => migrate_remove_obsolete_geography_hash(db),
        38 => migrate_canonical_subcategory_labels(db),
        39 => migrate_classification_subcategory_fallbacks(db),
        40 => migrate_taxonomy_candidate_grouping(db),
        41 => migrate_system_provenance_table(db),
        42 => migrate_classification_follow_up_queue(db),
        43 => migrate_app_settings(db),
        44 => migrate_list_search_performance(db),
        45 => migrate_deterministic_catalog_v11(db),
        46 => migrate_deterministic_catalog_v12(db),
        47 => migrate_sandboxed_web_discovery(db),
        48 => migrate_lead_source_catalog(db),
        49 => migrate_macroarea_from_regione(db),
        50 => migrate_lead_source_codes(db),
        51 => migrate_drop_compliance_columns(db),
        52 => migrate_dns_cache_per_domain(db),
        53 => migrate_pec_is_pec_only(db),
        54 => migrate_exportable_contact_projection(db),
        55 => migrate_pec_label_detection(db),
        56 => migrate_deterministic_catalog_v13(db),
        57 => migrate_classification_audit_repairs(db),
        58 => migrate_lead_sandbox_mirror(db),
        59 => migrate_canonical_province_names(db),
        60 => migrate_contact_quality_score(db),
        61 => migrate_contact_sort_indexes(db),
        62 => migrate_classification_sort_by_method(db),
        63 => migrate_contact_score_without_confidence(db),
        64 => migrate_contact_score_richness(db),
        65 => migrate_promoted_contact_status(db),
        66 => migrate_contextdev_credits(db),
        67 => migrate_canonical_subcategory_labels(db),
        68 => migrate_retire_personal_and_sensitive_subcategories(db),
        69 => migrate_remove_sanita_contacts(db),
        _ => Err(format!("unknown migration {version}")),
    }
}

pub(crate) fn schema_object_sql(
    db: &Connection,
    object_type: &str,
    name: &str,
) -> Result<String, String> {
    db.query_row(
        "SELECT coalesce(sql,'') FROM sqlite_master WHERE type=?1 AND name=?2",
        params![object_type, name],
        |row| row.get(0),
    )
    .optional()
    .map(|sql| sql.unwrap_or_default())
    .map_err(|error| error.to_string())
}

pub(crate) fn compact_schema_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<String>()
        .to_ascii_lowercase()
}

pub(crate) fn setup_schema(db: &Connection) -> Result<(), String> {
    run_ordered_migrations(db, None)
}

pub(crate) const LATEST_SCHEMA_VERSION: i64 = 69;

pub(crate) const ACTIVE_TAXONOMY_VERSION: &str = "taxonomy-v1";

pub(crate) fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, String> {
    // table_xinfo, unlike table_info, lists generated columns (contacts.macroarea).
    db.prepare(&format!("PRAGMA table_xinfo({table})"))
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|err| err.to_string())
}

pub fn has_table(db: &Connection, table: &str) -> Result<bool, String> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        params![table],
        |row| row.get(0),
    )
    .map_err(|err| err.to_string())
}

pub fn migrate_database(
    db: &mut Connection,
    database_path: Option<&std::path::Path>,
    inject_failure: bool,
) -> Result<(), String> {
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|err| err.to_string())?;
    if version > LATEST_SCHEMA_VERSION {
        return Err(format!(
            "Versione database {version} non supportata (massima {LATEST_SCHEMA_VERSION})"
        ));
    }

    if version < LATEST_SCHEMA_VERSION && has_table(db, "schema_migrations")? {
        db.execute(
            "DELETE FROM schema_migrations WHERE version>?1",
            params![version],
        )
        .map_err(|err| err.to_string())?;
    }
    if version == LATEST_SCHEMA_VERSION {
        let applied = {
            let mut statement = db
                .prepare("SELECT version,name FROM schema_migrations ORDER BY version")
                .map_err(|error| error.to_string())?;

            statement
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        if applied.len() == MIGRATION_NAMES.len()
            && applied.iter().enumerate().all(|(offset, (version, name))| {
                *version == offset as i64 + 1 && name == MIGRATION_NAMES[offset]
            })
        {
            return Ok(());
        }
        setup_schema(db)?;
        return Ok(());
    }
    if !has_table(db, "contacts")? {
        run_ordered_migrations(db, inject_failure.then_some(4))?;
        db.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    let columns = table_columns(db, "contacts")?;
    let baseline = [
        "id",
        "email",
        "email_local_part",
        "email_domain",
        "email_tld",
        "status",
        "validation_notes",
        "source_file",
        "category",
        "subcategory",
    ];
    let is_supported_email_schema = baseline
        .iter()
        .all(|required| columns.iter().any(|column| column == required));
    if is_supported_email_schema {
        run_ordered_migrations(db, inject_failure.then_some(4))?;
        db.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    let backup_path = match database_path {
        Some(path) => Some(backup_database(db, path)?),
        None => None,
    };
    migrate_legacy_contacts(db, backup_path.as_deref(), inject_failure)
}
