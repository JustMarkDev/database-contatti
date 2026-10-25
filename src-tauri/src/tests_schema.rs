use super::*;
use crate::{ai_runs::*, blacklist::*, contacts::*, imports::*, schema::*};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::time::Duration;

fn test_contact() -> ContactRecord {
    ContactRecord {
        id: "contact-1".to_owned(),
        email: "protocollo@comune.moncalieri.to.it".to_owned(),
        email_local_part: "protocollo".to_owned(),
        email_domain: "comune.moncalieri.to".to_owned(),
        email_tld: ".it".to_owned(),
        status: EmailStatus::Valid,
        validation_notes: Vec::new(),
        source_file: "test.txt".to_owned(),
        category: "Varie".to_owned(),
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

fn load_test_contact(db: &Connection) -> ContactRecord {
    db.query_row(
        "SELECT id, email, email_local_part, email_domain, email_tld, status, validation_notes,
                source_file, category, subcategory, contact_kind, paese, is_pec, comune, provincia, regione, website,
                source_url, category_id, subcategory_id, classification_method,
                classification_confidence, classification_evidence, needs_review,
                classification_hash, macroarea, taxonomy_version_id,
                (SELECT semantic_version FROM taxonomy_versions WHERE id=contacts.taxonomy_version_id),
                classification_state, classification_state_reason
             FROM contacts WHERE id = 'contact-1'",
        [],
        row_to_contact,
    )
    .expect("load test contact")
}

#[test]
fn taxonomy_fallback_backfill_records_system_provenance() {
    let db = Connection::open_in_memory().expect("in-memory db");
    run_ordered_migrations(&db, None).expect("schema");
    insert_contact_record(&db, &test_contact()).expect("insert contact");
    migrate_classification_subcategory_fallbacks(&db).expect("fallback backfill");

    let assigned: (String, String, String) = db
        .query_row(
            "SELECT category,subcategory,classification_method FROM contacts WHERE id='contact-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        assigned,
        (
            "Varie".to_owned(),
            "Ente non determinato".to_owned(),
            "taxonomy_default".to_owned()
        )
    );
    let changes: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM classification_system_changes
             WHERE contact_id='contact-1' AND operation='subcategory-fallback-backfill'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(changes, 1);
}

#[test]
fn migration_refreshes_builtin_deterministic_catalog_to_latest() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    migrate_database(&mut db, None, false).expect("initial migration");
    db.execute(
        "UPDATE deterministic_rule_catalogs
         SET semantic_version='1.0.0', rules_json='{\"id\":\"exact-it-v1\",\"semantic_version\":\"1.0.0\",\"rules\":[]}'
         WHERE id='exact-it-v1'",
        [],
    )
    .expect("downgrade catalog fixture");
    db.pragma_update(None, "user_version", 44)
        .expect("set v44 envelope");
    db.execute("DELETE FROM schema_migrations WHERE version > 44", [])
        .expect("remove newer markers");

    migrate_database(&mut db, None, false).expect("catalog refresh migration");
    let (version, raw): (String, String) = db
        .query_row(
            "SELECT semantic_version, rules_json FROM deterministic_rule_catalogs WHERE id='exact-it-v1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("catalog row");
    assert_eq!(version, "1.4.0");
    assert!(raw.contains("it.government.regions"));
    assert!(raw.contains("pec.carabinieri.it"));
    assert!(raw.contains("it.tourism.proloco-domain-prefix"));
    assert!(raw.contains("token_evidence"));
}

#[test]
fn migration_repairs_follow_up_queue_for_existing_v41_databases() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    migrate_database(&mut db, None, false).expect("initial migration");

    // Simulate a database that reached v41 before the follow-up queue was
    // added to the already-recorded migration.
    db.execute("DROP TABLE classification_follow_up_queue", [])
        .expect("remove queue from compatibility fixture");
    db.pragma_update(None, "user_version", 41)
        .expect("set v41 compatibility envelope");
    db.execute("DELETE FROM schema_migrations WHERE version > 41", [])
        .expect("remove newer completion markers");

    migrate_database(&mut db, None, false).expect("repair migration");
    let exists: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='classification_follow_up_queue'",
            [],
            |row| row.get(0),
        )
        .expect("queue check");
    assert_eq!(exists, 1);
}

#[test]
fn fresh_schema_migrations_are_complete_versioned_and_idempotent() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    migrate_database(&mut db, None, false).expect("first migration");
    migrate_database(&mut db, None, false).expect("repeated migration");
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, LATEST_SCHEMA_VERSION);
    let applied: Vec<i64> = db
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(applied, (1..=LATEST_SCHEMA_VERSION).collect::<Vec<_>>());
    for table in [
        "contacts",
        "categories",
        "classification_reviews",
        "ai_classification_runs",
        "import_attempts",
        "source_rows",
        "email_candidates",
        "candidate_dispositions",
        "suppressions",
        "verification_events",
        "domain_verification_cache",
        "classification_claims",
        "organizations",
        "entity_contact_points",
        "quality_score_snapshots",
        "quality_score_components",
        "taxonomy_entries",
        "taxonomy_change_candidates",
        "ai_run_batches",
        "classification_benchmark_runs",
        "classification_benchmark_results",
        "classification_benchmark_controls",
        "classification_follow_up_queue",
        "taxonomy_change_candidate_contacts",
        "classification_system_changes",
    ] {
        let exists: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            )
            .expect("table check");
        assert_eq!(exists, 1, "missing {table}");
    }
    let benchmark_columns = table_columns(&db, "classification_benchmark_runs").unwrap();
    for column in ["total_cases", "processed_cases"] {
        assert!(
            benchmark_columns.iter().any(|name| name == column),
            "missing benchmark column: {column}"
        );
    }
    let contact_columns = table_columns(&db, "contacts").unwrap();
    for removed in [
        "classification_locked",
        "category_locked",
        "subcategory_locked",
        "comune_locked",
        "provincia_locked",
        "regione_locked",
    ] {
        assert!(
            !contact_columns.iter().any(|column| column == removed),
            "obsolete lock column still present: {removed}"
        );
    }
    for (table, column) in [
        ("domain_verification_cache", "observed_ttl_seconds"),
        ("domain_verification_cache", "effective_cache_ttl_seconds"),
        ("verification_events", "observed_ttl_seconds"),
        ("verification_events", "effective_cache_ttl_seconds"),
        ("verification_events", "dns_checked_at"),
        ("verification_events", "dns_expires_at"),
    ] {
        let exists: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2",
                params![table, column],
                |row| row.get(0),
            )
            .expect("column check");
        assert_eq!(exists, 1, "missing {table}.{column}");
    }
    let one_current_index: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_candidate_dispositions_one_current'",
            [],
            |row| row.get(0),
        )
        .expect("current disposition invariant index");
    assert_eq!(one_current_index, 1);
}

#[test]
fn incremental_migration_does_not_rewrite_the_database() {
    let path = migration_test_path("incremental-no-vacuum");
    let mut db = Connection::open(&path).expect("database");
    migrate_database(&mut db, Some(&path), false).expect("initial migration");
    db.execute_batch(
        "CREATE TABLE migration_padding(id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
         WITH RECURSIVE n(x) AS (
             VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x <= 16
         )
         INSERT INTO migration_padding(id,payload) SELECT x,zeroblob(1048576) FROM n;
         DELETE FROM migration_padding WHERE id % 2 = 0;",
    )
    .expect("padding fixture");
    let before: i64 = db
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .expect("page count before migration");
    db.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION - 1)
        .expect("downgrade compatibility envelope");
    db.execute(
        "DELETE FROM schema_migrations WHERE version=?1",
        params![LATEST_SCHEMA_VERSION],
    )
    .expect("remove latest completion marker");

    migrate_database(&mut db, Some(&path), false).expect("incremental migration");
    let after: i64 = db
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .expect("page count after migration");
    assert!(
        after >= before,
        "incremental migration unexpectedly compacted the database: {before} -> {after} pages"
    );
    drop(db);
    remove_migration_test_files(&path);
}

#[test]
fn lossless_ingestion_schema_is_additive_idempotent_and_blacklist_is_durable() {
    let db = Connection::open_in_memory().expect("in-memory db");
    run_ordered_migrations(&db, None).expect("schema");
    db.execute(
        "INSERT INTO blacklist(normalized_email,source_file,imported_at,note)
         VALUES('blocked@example.test','blacklist.csv','2026-01-01T00:00:00Z','legacy')",
        [],
    )
    .expect("legacy blacklist");
    db.execute(
        "INSERT INTO imports(
            id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at
         ) VALUES ('schema-import','source.csv','Folder/source.csv','legacy','completed',1,0,'2026-01-01T00:00:00Z')",
        [],
    )
    .expect("source import");
    db.execute(
        "INSERT INTO source_rows(
            id,import_id,source_sheet,row_number,raw_context,parse_status,created_at
         ) VALUES ('header-row','schema-import','Sheet1',1,'{}','header','2026-01-01T00:00:00Z')",
        [],
    )
    .expect("header source row");

    migrate_lossless_ingestion_schema(&db).expect("idempotent migration");
    let suppression: (String, String) = db
        .query_row(
            "SELECT comparison_key,reason FROM suppressions WHERE comparison_key='blocked@example.test'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("durable suppression");
    assert_eq!(
        suppression,
        ("blocked@example.test".into(), "blacklist".into())
    );

    let import_columns = table_columns(&db, "imports").expect("imports columns");
    for column in [
        "content_sha256",
        "hash_algorithm",
        "file_size_bytes",
        "parser_version",
        "encoding",
        "delimiter",
        "sheet_count",
        "report_json",
    ] {
        assert!(
            import_columns.iter().any(|candidate| candidate == column),
            "missing {column}"
        );
    }
    let candidate_columns = table_columns(&db, "email_candidates").expect("candidate columns");
    for column in [
        "raw_email",
        "canonical_email",
        "normalization_method",
        "status",
        "syntax_status",
        "routing_status",
        "mailbox_status",
        "current_disposition",
        "disposition_reason",
        "category",
        "source_url",
        "organization_name",
        "person_name",
        "role",
        "department",
        "address",
        "postal_code",
        "official_code",
        "outreach_eligible",
        "quality_score",
        "occurrence_index",
    ] {
        assert!(
            candidate_columns
                .iter()
                .any(|candidate| candidate == column),
            "missing {column}"
        );
    }
    for table in ["email_candidates", "contacts"] {
        let columns = table_columns(&db, table).expect("table columns");
        for column in ["permitted_purpose", "lawful_basis", "consent_provenance"] {
            assert!(
                !columns.iter().any(|existing| existing == column),
                "{table}.{column}"
            );
        }
    }
}

#[test]
fn lossless_ingestion_migration_rolls_back_atomically() {
    let db = Connection::open_in_memory().expect("in-memory db");
    run_ordered_migrations(&db, Some(15)).expect_err("injected final migration failure");
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version=15",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='email_candidates'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    run_ordered_migrations(&db, None).expect("retry final migration");
    assert!(has_table(&db, "email_candidates").unwrap());
}

#[test]
fn changed_file_version_keeps_a_distinct_contact_source_occurrence() {
    let db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    db.execute("INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,validation_notes,source_file,category,contact_kind,paese,classification_method,classification_confidence,classification_evidence,needs_review) VALUES('contact','case@example.test','case','example.test','test','valid','[]','Scuole/file.csv','Istruzione','istituzionale','Italia','source',1,'[]',0)", []).unwrap();
    for (import_id, hash, row_id) in [
        ("import-a", "hash-a", "row-a"),
        ("import-b", "hash-b", "row-b"),
    ] {
        db.execute("INSERT INTO imports(id,file_name,relative_path,file_hash,status,imported_at,content_sha256,hash_algorithm) VALUES(?1,'file.csv','Scuole/file.csv',?2,'completed','now',?2,'sha256')", params![import_id, hash]).unwrap();
        db.execute(
            "INSERT INTO source_rows(id,import_id,row_number,created_at) VALUES(?1,?2,1,'now')",
            params![row_id, import_id],
        )
        .unwrap();
        db.execute("INSERT INTO contact_sources(id,contact_id,import_id,source_file,source_sheet,source_row,source_row_id) VALUES(?1,'contact',?2,'Scuole/file.csv','',1,?3)", params![format!("source-{import_id}"), import_id, row_id]).unwrap();
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM contact_sources WHERE contact_id='contact'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        2
    );
}

#[test]
fn ordered_migration_failure_records_only_completed_transition_and_retries() {
    let db = Connection::open_in_memory().expect("in-memory db");

    let error = run_ordered_migrations(&db, Some(4)).expect_err("injected failure");
    assert!(error.contains("gazetteer"));
    let applied: Vec<(i64, String)> = db
        .prepare("SELECT version,name FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(applied.len(), 3);
    assert_eq!(applied[1].1, "taxonomy-state-varie");
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='gazetteer_places'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );

    run_ordered_migrations(&db, None).expect("retry");
    run_ordered_migrations(&db, None).expect("idempotent reopen");
    let completed: i64 = db
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(completed, LATEST_SCHEMA_VERSION);
    assert!(
        db.query_row("SELECT COUNT(*) FROM gazetteer_places", [], |row| row
            .get::<_, i64>(0))
            .unwrap()
            > 0
    );
}

#[test]
fn upgrade_from_v18_ends_without_compliance_columns_and_keeps_contacts() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    let error = run_ordered_migrations(&db, Some(19)).expect_err("stop before repair stamp");
    assert!(error.contains("consent-metadata-columns"));
    db.pragma_update(None, "user_version", 18).unwrap();
    db.execute(
        "INSERT INTO contacts(
            id,email,email_local_part,email_domain,email_tld,status,validation_notes,
            source_file,category,contact_kind,paese,classification_method,
            classification_confidence,classification_evidence,needs_review
         ) VALUES(
            'contact-1','protocollo@comune.moncalieri.to.it','protocollo','comune.moncalieri.to.it','it','valid','[]',
            'test.csv','Varie','istituzionale','Italia','source',1,'[]',0
         )",
        [],
    )
    .expect("insert existing contact");

    for table in ["email_candidates", "contacts"] {
        for column in ["permitted_purpose", "lawful_basis", "consent_provenance"] {
            // Simulate an incomplete version-18 schema where the column exists.
            let _ = db.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), []);
        }
    }

    migrate_database(&mut db, None, false).expect("repair migration");
    migrate_database(&mut db, None, false).expect("idempotent reopen");

    for table in ["email_candidates", "contacts"] {
        let columns = table_columns(&db, table).expect("table columns");
        for column in ["permitted_purpose", "lawful_basis", "consent_provenance"] {
            assert!(
                !columns.iter().any(|existing| existing == column),
                "kept {table}.{column}"
            );
        }
    }
    assert_eq!(load_test_contact(&db).email, test_contact().email);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version=19",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn migration_repairs_intermediate_ingestion_schema_and_candidate_queries() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    let error = run_ordered_migrations(&db, Some(20)).expect_err("stop before repair stamp");
    assert!(error.contains("ingestion-v15-schema-repair"));
    db.pragma_update(None, "user_version", 19).unwrap();
    db.execute_batch(
        "INSERT INTO imports(
            id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at
         ) VALUES ('repair-import','repair.csv','repair.csv','hash','completed',1,0,'2026-01-01');
         INSERT INTO source_rows(
            id,import_id,source_sheet,row_number,raw_context,parse_status,candidate_count,created_at
         ) VALUES ('repair-row','repair-import','Sheet1',1,'{}','header',1,'2026-01-01');
         INSERT INTO email_candidates(
            id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
            extraction_method,status,syntax_status,routing_status,mailbox_status,
            candidate_status,current_disposition,outreach_eligible,score_factors_json,
            occurrence_index,first_seen_at,last_seen_at
         ) VALUES (
            'repair-candidate','repair-row','repair@example.test','repair@example.test',
            'repair@example.test','literal','parser','quarantined','syntax_ok','unknown',
            'unknown','quarantined','quarantined',0,'{}',0,'2026-01-01','2026-01-01'
         );
         INSERT INTO candidate_dispositions(
            id,candidate_id,disposition,is_current,created_at
         ) VALUES ('repair-disposition','repair-candidate','quarantined',1,'2026-01-01');
         DROP INDEX idx_candidates_occurrence;
         ALTER TABLE email_candidates DROP COLUMN outreach_eligible;
         CREATE UNIQUE INDEX idx_candidates_occurrence
            ON email_candidates(
                source_row_id,
                ifnull(comparison_key,lower(trim(raw_email))),
                ifnull(extraction_method,'literal')
            ) WHERE source_row_id IS NOT NULL;",
    )
    .expect("simulate intermediate migration-15 schema");

    let error = run_ordered_migrations(&db, Some(20)).expect_err("injected repair failure");
    assert!(error.contains("ingestion-v15-schema-repair"));
    assert!(
        !table_columns(&db, "email_candidates")
            .unwrap()
            .iter()
            .any(|column| column == "outreach_eligible")
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM email_candidates WHERE id='repair-candidate'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );

    migrate_database(&mut db, None, false).expect("repair migration");
    migrate_database(&mut db, None, false).expect("idempotent reopen");

    let page = crate::candidates::load_candidates(
        &db,
        &crate::candidates::CandidateFilters::default(),
        25,
        0,
    )
    .expect("candidate query after repair");
    assert_eq!(page.total, 1);
    assert_eq!(page.candidates[0].id, "repair-candidate");
    assert_eq!(page.candidates[0].occurrence_index, Some(0));
    assert!(!page.candidates[0].outreach_eligible);

    let source_sql =
        compact_schema_sql(&schema_object_sql(&db, "table", "source_rows").expect("source schema"));
    let candidate_sql = compact_schema_sql(
        &schema_object_sql(&db, "table", "email_candidates").expect("candidate schema"),
    );
    let occurrence_index_sql = compact_schema_sql(
        &schema_object_sql(&db, "index", "idx_candidates_occurrence").expect("occurrence index"),
    );
    assert!(source_sql.contains("'parsed','header','empty','malformed','failed'"));
    assert!(
        candidate_sql
            .contains("unique(source_row_id,raw_email,extraction_method,occurrence_index)")
    );
    assert!(occurrence_index_sql.contains("occurrence_index"));
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM candidate_dispositions WHERE candidate_id='repair-candidate'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn current_unversioned_schema_migrations_stamp_without_data_loss() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    insert_contact_record(&db, &test_contact()).expect("insert");
    migrate_database(&mut db, None, false).expect("migration");
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(load_test_contact(&db).email, test_contact().email);
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, LATEST_SCHEMA_VERSION);
}

#[test]
fn migration_removes_obsolete_classified_at_column_without_data_loss() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    insert_contact_record(&db, &test_contact()).expect("insert");
    db.execute("ALTER TABLE contacts ADD COLUMN classified_at TEXT", [])
        .expect("legacy timestamp column");
    db.execute(
        "UPDATE contacts SET classified_at='2026-07-05T12:00:00Z'",
        [],
    )
    .expect("legacy timestamp value");
    db.pragma_update(None, "user_version", 6).unwrap();

    migrate_database(&mut db, None, false).expect("migration");

    assert!(
        !table_columns(&db, "contacts")
            .unwrap()
            .iter()
            .any(|column| column == "classified_at")
    );
    assert_eq!(load_test_contact(&db).email, test_contact().email);
}

#[test]
fn original_email_schema_migrations_preserve_values_after_retry() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    db.execute_batch(
        "CREATE TABLE contacts (
                id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, email_local_part TEXT NOT NULL,
                email_domain TEXT NOT NULL, email_tld TEXT NOT NULL, status TEXT NOT NULL,
                validation_notes TEXT NOT NULL, source_file TEXT NOT NULL, category TEXT NOT NULL,
                subcategory TEXT, email_kind TEXT, comune TEXT, provincia TEXT, regione TEXT,
                website TEXT
             );
             INSERT INTO contacts VALUES (
                'original-1','original@example.it','original','example.it','.it','valid','[]',
                'original.csv','Scuole','Primaria','standard','Torino','Torino','Piemonte',NULL
             );",
    )
    .unwrap();
    migrate_database(&mut db, None, false).expect("migration");
    migrate_database(&mut db, None, false).expect("second open");
    let values: (String, String, String, String) = db
        .query_row(
            "SELECT email,source_file,category,subcategory FROM contacts WHERE id='original-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        values,
        (
            "original@example.it".to_owned(),
            "original.csv".to_owned(),
            "Scuole".to_owned(),
            "Primaria".to_owned()
        )
    );
}

fn migration_test_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "database-contatti-{name}-{}.sqlite3",
        uuid::Uuid::new_v4()
    ))
}

fn remove_migration_test_files(path: &std::path::Path) {
    let parent = path.parent().unwrap();
    let prefix = path.file_name().unwrap().to_string_lossy().to_string();
    for entry in std::fs::read_dir(parent).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[test]
fn legacy_normalized_email_migrations_preserve_rows_and_backup() {
    let path = migration_test_path("legacy");
    {
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE contacts(
                    id TEXT PRIMARY KEY, normalized_email TEXT NOT NULL UNIQUE,
                    source_file TEXT, categories TEXT, phone TEXT
                 );
                 INSERT INTO contacts VALUES
                    ('legacy-1','First@Example.it','old.csv','Scuole','111'),
                    ('legacy-2','second@example.it','old.csv','Varie','222');",
        )
        .unwrap();
        migrate_database(&mut db, Some(&path), false).expect("legacy migration");
        let values: (i64, String, String) = db
            .query_row(
                "SELECT COUNT(*),min(email),max(source_file) FROM contacts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            values,
            (2, "first@example.it".to_owned(), "old.csv".to_owned())
        );
        assert!(!has_table(&db, "contacts_legacy_archive").unwrap());
    }
    {
        let mut reopened = Connection::open(&path).unwrap();
        migrate_database(&mut reopened, Some(&path), false).expect("second open");
        let count: i64 = reopened
            .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
    let backup_path = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .find(|entry| {
            entry.file_name().to_string_lossy().starts_with(&format!(
                "{}.backup-",
                path.file_name().unwrap().to_string_lossy()
            ))
        })
        .map(|entry| entry.path())
        .expect("migration backup");
    let backup = Connection::open(backup_path).unwrap();
    let backup_values: (i64, String) = backup
        .query_row(
            "SELECT COUNT(*),min(normalized_email) FROM contacts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(backup_values, (2, "First@Example.it".to_owned()));
    drop(backup);
    remove_migration_test_files(&path);
}

#[test]
fn failed_legacy_migrations_roll_back_and_retry_from_backup() {
    let path = migration_test_path("failure");
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE contacts(id TEXT PRIMARY KEY, normalized_email TEXT NOT NULL, phone TEXT);
             INSERT INTO contacts VALUES ('legacy-1','recover@example.it','111');",
    )
    .unwrap();
    let error = migrate_database(&mut db, Some(&path), true).unwrap_err();
    assert!(error.contains("backup conservato"));
    assert!(has_table(&db, "contacts").unwrap());
    assert!(!has_table(&db, "contacts_legacy_archive").unwrap());
    let email: String = db
        .query_row("SELECT normalized_email FROM contacts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(email, "recover@example.it");
    migrate_database(&mut db, Some(&path), false).expect("retry");
    let migrated: String = db
        .query_row("SELECT email FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(migrated, "recover@example.it");
    drop(db);
    remove_migration_test_files(&path);
}

#[test]
fn legacy_migration_preserves_known_contact_dependents_and_foreign_keys() {
    let path = migration_test_path("legacy-dependents");
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE contacts(id TEXT PRIMARY KEY, normalized_email TEXT NOT NULL UNIQUE);
             CREATE TABLE imports(id TEXT PRIMARY KEY);
             CREATE TABLE contact_sources(
               id TEXT PRIMARY KEY,contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
               import_id TEXT,source_file TEXT NOT NULL,source_sheet TEXT,source_row INTEGER,
               raw_context TEXT NOT NULL DEFAULT '{}',source_url TEXT
             );
             CREATE TABLE classification_reviews(
               id TEXT PRIMARY KEY,contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
               current_category TEXT NOT NULL,current_subcategory TEXT,proposed_category TEXT NOT NULL,
               proposed_subcategory TEXT,confidence REAL NOT NULL,evidence TEXT NOT NULL,reason_code TEXT,
               model TEXT NOT NULL,profile TEXT NOT NULL,prompt_version TEXT NOT NULL,status TEXT NOT NULL,
               created_at TEXT NOT NULL,resolved_at TEXT
             );
             CREATE TABLE ai_classification_runs(
               id TEXT PRIMARY KEY,model TEXT NOT NULL,profile TEXT NOT NULL,prompt_version TEXT NOT NULL,
               status TEXT NOT NULL,total INTEGER NOT NULL,processed INTEGER NOT NULL DEFAULT 0,
               updated INTEGER NOT NULL DEFAULT 0,reviews_created INTEGER NOT NULL DEFAULT 0,
               skipped INTEGER NOT NULL DEFAULT 0,errors INTEGER NOT NULL DEFAULT 0,started_at TEXT NOT NULL,
               finished_at TEXT
             );
             CREATE TABLE ai_run_candidates(
               run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
               contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,ordinal INTEGER NOT NULL,
               status TEXT NOT NULL,error TEXT,processed_at TEXT,PRIMARY KEY(run_id,contact_id)
             );
             CREATE TABLE ai_field_changes(
               id TEXT PRIMARY KEY,run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
               contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
               review_id TEXT REFERENCES classification_reviews(id) ON DELETE SET NULL,
               field_name TEXT NOT NULL,old_value TEXT,new_value TEXT,change_kind TEXT NOT NULL,
               status TEXT NOT NULL,confidence REAL NOT NULL,evidence TEXT NOT NULL,created_at TEXT NOT NULL,
               resolved_at TEXT
             );
             INSERT INTO contacts VALUES ('contact-1','legacy@example.it');
             INSERT INTO contact_sources(id,contact_id,source_file) VALUES ('source-1','contact-1','old.csv');
             INSERT INTO classification_reviews VALUES
               ('review-1','contact-1','Varie',NULL,'Scuole',NULL,0.7,'[]',NULL,'m','l','v','pending','now',NULL);
             INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
               VALUES ('run-1','m','l','v','paused',1,'now');
             INSERT INTO ai_run_candidates(run_id,contact_id,ordinal,status)
               VALUES ('run-1','contact-1',0,'pending');
             INSERT INTO ai_field_changes VALUES
               ('change-1','run-1','contact-1','review-1','category','Varie','Scuole',
                'in_revisione','pending_review',0.7,'[]','now',NULL);",
        )
        .unwrap();
    migrate_database(&mut db, Some(&path), false).unwrap();
    for table in [
        "contact_sources",
        "classification_reviews",
        "ai_run_candidates",
        "ai_field_changes",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
    let violations: i64 = db
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    assert!(!has_table(&db, "contacts_legacy_archive").unwrap());
    for table in [
        "contact_sources_legacy_dependency",
        "classification_reviews_legacy_dependency",
        "ai_run_candidates_legacy_dependency",
        "ai_field_changes_legacy_dependency",
    ] {
        assert!(!has_table(&db, table).unwrap(), "{table}");
    }
    db.execute("DELETE FROM contacts WHERE id='contact-1'", [])
        .unwrap();
    let sources: i64 = db
        .query_row("SELECT COUNT(*) FROM contact_sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sources, 0);
    drop(db);
    remove_migration_test_files(&path);
}

#[test]
fn legacy_migration_rejects_unknown_contact_dependency_with_backup() {
    let path = migration_test_path("legacy-unknown-fk");
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
             CREATE TABLE contacts(id TEXT PRIMARY KEY, normalized_email TEXT NOT NULL);
             CREATE TABLE custom_notes(
               id TEXT PRIMARY KEY,contact_id TEXT REFERENCES contacts(id)
             );
             INSERT INTO contacts VALUES ('contact-1','legacy@example.it');
             INSERT INTO custom_notes VALUES ('note-1','contact-1');",
    )
    .unwrap();
    let error = migrate_database(&mut db, Some(&path), false).unwrap_err();
    assert!(error.contains("backup conservato"));
    assert!(error.contains("custom_notes"));
    let value: String = db
        .query_row("SELECT normalized_email FROM contacts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(value, "legacy@example.it");
    drop(db);
    remove_migration_test_files(&path);
}

#[test]
fn blacklist_import_is_atomic_idempotent_and_case_insensitive() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    let blocked = test_contact();
    insert_contact_record(&db, &blocked).expect("blocked insert");
    let mut allowed = test_contact();
    allowed.id = "contact-2".to_owned();
    allowed.email = "allowed@example.it".to_owned();
    insert_contact_record(&db, &allowed).expect("allowed insert");
    let file = ImportFile {
        path: None,
        name: "blacklist.txt".to_owned(),
        relative_path: "blacklist.txt".to_owned(),
        bytes: b"PROTOCOLLO@COMUNE.MONCALIERI.TO.IT\nmissing@example.it".to_vec(),
    };
    let failed = import_blacklist_files_transaction(&mut db, vec![file.clone()], true);
    assert!(failed.is_err());
    let blacklist_count: i64 = db
        .query_row("SELECT COUNT(*) FROM blacklist", [], |row| row.get(0))
        .unwrap();
    let contact_count: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(blacklist_count, 0);
    assert_eq!(contact_count, 2);

    let first = import_blacklist_files_transaction(&mut db, vec![file.clone()], false).unwrap();
    assert_eq!(first.inserted, 2);
    assert_eq!(first.duplicates, 0);
    assert_eq!(first.removed_contacts, 1);
    let second = import_blacklist_files_transaction(&mut db, vec![file], false).unwrap();
    assert_eq!(second.inserted, 0);
    assert_eq!(second.duplicates, 0);
    assert_eq!(second.removed_contacts, 0);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM imports WHERE relative_path='blacklist.txt' AND hash_algorithm='sha256'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1,
        "reimporting identical blacklist bytes must not duplicate the immutable manifest"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM email_candidates WHERE current_disposition='suppressed'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        2
    );
    let remaining: String = db
        .query_row("SELECT email FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, "allowed@example.it");
}

#[test]
fn clearing_blacklist_forgets_imported_manifests_allowing_reimport() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    let file = ImportFile {
        path: None,
        name: "blacklist.txt".to_owned(),
        relative_path: "blacklist.txt".to_owned(),
        bytes: b"spam@example.test\r\nabuse@example.test\r\n".to_vec(),
    };
    let first = import_blacklist_files_transaction(&mut db, vec![file.clone()], false).unwrap();
    assert_eq!(first.inserted, 2);

    let second = import_blacklist_files_transaction(&mut db, vec![file.clone()], false).unwrap();
    assert_eq!(second.inserted, 0);
    assert_eq!(second.duplicates, 0);

    crate::blacklist::clear_blacklist_connection(&mut db).unwrap();
    let bl_count: i64 = db
        .query_row("SELECT COUNT(*) FROM blacklist", [], |r| r.get(0))
        .unwrap();
    assert_eq!(bl_count, 0);

    let third = import_blacklist_files_transaction(&mut db, vec![file], false).unwrap();
    assert_eq!(third.inserted, 2);
    let bl_count_after: i64 = db
        .query_row("SELECT COUNT(*) FROM blacklist", [], |r| r.get(0))
        .unwrap();
    assert_eq!(bl_count_after, 2);
}

#[test]
fn empty_blacklist_file_is_reported_without_mutation() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    let report = import_blacklist_files_transaction(
        &mut db,
        vec![ImportFile {
            path: None,
            name: "empty.txt".to_owned(),
            relative_path: "empty.txt".to_owned(),
            bytes: Vec::new(),
        }],
        false,
    )
    .unwrap();
    assert_eq!(report.inserted, 0);
    assert_eq!(report.duplicates, 0);
    assert_eq!(report.invalid, 1);
    assert_eq!(report.removed_contacts, 0);
}

#[test]
fn single_blacklist_entry_is_added_and_suppresses_matching_contacts() {
    let db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    db.execute(
        "INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,validation_notes,source_file,category,contact_kind,paese,classification_method,classification_confidence,classification_evidence,needs_review)
         VALUES('c1','blocked@example.test','blocked','example.test','test','valid','[]','manual.csv','Varie','generico','Italia','source',1,'[]',0)",
        [],
    )
    .unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let email = "blocked@example.test";
    let key = email.to_lowercase();
    db.execute(
        "INSERT OR IGNORE INTO blacklist(normalized_email,source_file,imported_at,note) VALUES (?1,'manual',?2,?3)",
        params![key, now, "custom note"],
    ).unwrap();
    let removed = crate::blacklist::suppress_blacklisted_email(&db, &key, "manual", &now).unwrap();
    assert_eq!(removed, 1);
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let bl_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM blacklist WHERE normalized_email='blocked@example.test'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bl_count, 1);
}

#[test]
fn legacy_blacklist_consolidation_preserves_rows_and_recovers_after_failure() {
    let legacy_path = migration_test_path("blacklist-legacy");
    {
        let legacy = Connection::open(&legacy_path).expect("legacy db");
        legacy
            .execute_batch(
                "CREATE TABLE blacklist (
                       normalized_email TEXT PRIMARY KEY COLLATE NOCASE,
                       source_file TEXT NOT NULL,
                       imported_at TEXT NOT NULL,
                       note TEXT
                     );
                     INSERT INTO blacklist VALUES
                       ('PROTOCOLLO@COMUNE.MONCALIERI.TO.IT','old.txt','2025-01-01',NULL),
                       ('other@example.it','old.txt','2025-01-02','legacy note');",
            )
            .expect("legacy rows");
    }
    let mut db = Connection::open_in_memory().expect("contacts db");
    setup_schema(&db).expect("schema");
    insert_contact_record(&db, &test_contact()).expect("contact");
    let mut second = test_contact();
    second.id = "contact-2".to_owned();
    second.email = "other@example.it".to_owned();
    insert_contact_record(&db, &second).expect("second contact");
    db.execute(
        "INSERT INTO blacklist(normalized_email,source_file,imported_at)
             VALUES ('Other@Example.IT','existing.txt','2024-01-01')",
        [],
    )
    .unwrap();

    assert!(consolidate_legacy_blacklist(&mut db, &legacy_path, true).is_err());
    let before_retry: (i64, i64, i64) = (
        db.query_row("SELECT COUNT(*) FROM blacklist", [], |row| row.get(0))
            .unwrap(),
        db.query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
            .unwrap(),
        db.query_row(
            "SELECT COUNT(*) FROM blacklist_legacy_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap(),
    );
    assert_eq!(before_retry, (1, 2, 0));

    consolidate_legacy_blacklist(&mut db, &legacy_path, false).expect("retry");
    consolidate_legacy_blacklist(&mut db, &legacy_path, false).expect("idempotent reopen");
    let after_retry: (i64, i64, i64) = (
        db.query_row("SELECT COUNT(*) FROM blacklist", [], |row| row.get(0))
            .unwrap(),
        db.query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
            .unwrap(),
        db.query_row(
            "SELECT COUNT(*) FROM blacklist_legacy_migrations",
            [],
            |row| row.get(0),
        )
        .unwrap(),
    );
    assert_eq!(after_retry, (2, 0, 1));
    let source: String = db
            .query_row(
                "SELECT source_file FROM blacklist WHERE normalized_email='other@example.it' COLLATE NOCASE",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(source, "existing.txt");
    let legacy_count: i64 = Connection::open(&legacy_path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM blacklist", [], |row| row.get(0))
        .unwrap();
    assert_eq!(legacy_count, 2);
    remove_migration_test_files(&legacy_path);
}

#[test]
fn legacy_blacklist_first_open_is_claimed_once_across_connections() {
    let legacy_path = migration_test_path("blacklist-race-legacy");
    Connection::open(&legacy_path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE blacklist(
                   normalized_email TEXT PRIMARY KEY COLLATE NOCASE,source_file TEXT NOT NULL,
                   imported_at TEXT NOT NULL,note TEXT
                 );
                 INSERT INTO blacklist VALUES ('race@example.it','old.txt','now',NULL);",
        )
        .unwrap();
    let contacts_path = migration_test_path("blacklist-race-contacts");
    {
        let mut seed = Connection::open(&contacts_path).unwrap();
        migrate_database(&mut seed, Some(&contacts_path), false).unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let contacts_path = contacts_path.clone();
            let legacy_path = legacy_path.clone();
            std::thread::spawn(move || {
                let mut db = Connection::open(contacts_path).unwrap();
                db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                barrier.wait();
                consolidate_legacy_blacklist(&mut db, &legacy_path, false)
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let db = Connection::open(&contacts_path).unwrap();
    let counts: (i64, i64) = db
        .query_row(
            "SELECT
                   (SELECT COUNT(*) FROM blacklist),
                   (SELECT COUNT(*) FROM blacklist_legacy_migrations)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 1));
    drop(db);
    remove_migration_test_files(&legacy_path);
    remove_migration_test_files(&contacts_path);
}

#[test]
fn corrupt_legacy_blacklist_returns_actionable_recovery_error() {
    let legacy_path = migration_test_path("blacklist-corrupt");
    std::fs::write(&legacy_path, b"not a sqlite database").unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let error = consolidate_legacy_blacklist(&mut db, &legacy_path, false).unwrap_err();
    assert!(error.contains(&legacy_path.display().to_string()));
    assert!(error.contains("ripristinarlo da backup"));
    remove_migration_test_files(&legacy_path);
}

#[test]
fn failed_transaction_rolls_back_and_connection_remains_recoverable() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    {
        let tx = db.transaction().expect("transaction");
        insert_contact_record(&tx, &test_contact()).expect("first insert");
        assert!(insert_contact_record(&tx, &test_contact()).is_err());
    }
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    insert_contact_record(&db, &test_contact()).expect("connection should recover");
}

#[test]
fn retired_opencode_migration_fails_unfinished_runs_and_preserves_history() {
    let db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    for (id, status) in [
        ("legacy-running", "running"),
        ("legacy-pausing", "pausing"),
        ("legacy-paused", "paused"),
        ("legacy-completed", "completed"),
    ] {
        db.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at,provider,
                owner_session,lease_expires_at
             ) VALUES(?1,'opencode/x-preview-f-free','legacy','v1',?2,1,'now',
                      'opencode','legacy-session',123)",
            params![id, status],
        )
        .unwrap();
    }

    migrate_retired_opencode_runs(&db).expect("retirement migration");
    migrate_retired_opencode_runs(&db).expect("idempotent retirement migration");

    for id in ["legacy-running", "legacy-pausing", "legacy-paused"] {
        let row: (String, String, String, Option<String>, Option<i64>) = db
            .query_row(
                "SELECT status,provider,model,owner_session,lease_expires_at
                 FROM ai_classification_runs WHERE id=?1",
                params![id],
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
            .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.1, "opencode");
        assert_eq!(row.2, "opencode/x-preview-f-free");
        assert_eq!(row.3, None);
        assert_eq!(row.4, None);
    }

    let completed: (String, String, String) = db
        .query_row(
            "SELECT status,provider,model FROM ai_classification_runs
             WHERE id='legacy-completed'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        completed,
        (
            "completed".to_owned(),
            "opencode".to_owned(),
            "opencode/x-preview-f-free".to_owned()
        )
    );
}

#[test]
fn ai_run_request_and_candidate_queue_round_trip_every_setting() {
    let db = Connection::open_in_memory().expect("in-memory db");
    setup_schema(&db).expect("schema");
    insert_contact_record(&db, &test_contact()).expect("contact");
    let request = AiClassificationRequest {
        thinking: false,
        context_size: 8192,
        timeout_seconds: 123,
        force: true,
        mode: "fill".to_owned(),
        ..Default::default()
    };
    db.execute(
        "INSERT INTO ai_classification_runs
             (id,model,profile,prompt_version,status,total,started_at,request_json)
             VALUES ('run-1','model:test','default','v1','paused',1,'2026-01-01',?1)",
        params![serde_json::to_string(&request).unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ai_run_candidates(run_id,contact_id,ordinal) VALUES ('run-1','contact-1',0)",
        [],
    )
    .unwrap();
    let json: String = db
        .query_row(
            "SELECT request_json FROM ai_classification_runs WHERE id='run-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let loaded: AiClassificationRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(loaded.thinking, request.thinking);
    assert_eq!(loaded.context_size, request.context_size);
    assert_eq!(loaded.timeout_seconds, request.timeout_seconds);
    assert_eq!(loaded.force, request.force);
    let ordinal: i64 = db
        .query_row(
            "SELECT ordinal FROM ai_run_candidates WHERE run_id='run-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ordinal, 0);
}

#[test]
fn ai_run_state_guards_pause_cancel_and_terminal_resume() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs
             (id,model,profile,prompt_version,status,total,started_at)
             VALUES ('run-1','test','laptop','v1','running',0,'now')",
        [],
    )
    .unwrap();
    transition_ai_run(&db, "run-1", &["running"], "pausing").unwrap();
    transition_ai_run(&db, "run-1", &["pausing"], "paused").unwrap();
    transition_ai_run(&db, "run-1", &["paused"], "cancelled").unwrap();
    assert!(transition_ai_run(&db, "run-1", &["paused"], "running").is_err());
    let status: String = db
        .query_row(
            "SELECT status FROM ai_classification_runs WHERE id='run-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "cancelled");
    for terminal in ["completed", "failed", "cancelled"] {
        db.execute(
            "UPDATE ai_classification_runs SET status=?1 WHERE id='run-1'",
            params![terminal],
        )
        .unwrap();
        assert!(transition_ai_run(&db, "run-1", &["running"], "pausing").is_err());
        assert!(
            transition_ai_run(&db, "run-1", &["running", "pausing", "paused"], "cancelled")
                .is_err()
        );
        let unchanged: String = db
            .query_row(
                "SELECT status FROM ai_classification_runs WHERE id='run-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unchanged, terminal);
    }
}

#[test]
fn ai_run_transition_compare_and_set_allows_one_contending_winner() {
    let path = migration_test_path("ai-transition-race");
    {
        let db = Connection::open(&path).unwrap();
        setup_schema(&db).unwrap();
        db.execute(
            "INSERT INTO ai_classification_runs
                 (id,model,profile,prompt_version,status,total,started_at)
                 VALUES ('run-1','test','laptop','v1','running',0,'now')",
            [],
        )
        .unwrap();
    }
    for _ in 0..16 {
        let db = Connection::open(&path).unwrap();
        db.execute(
            "UPDATE ai_classification_runs SET status='running' WHERE id='run-1'",
            [],
        )
        .unwrap();
        drop(db);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let contenders = ["pausing", "cancelled"].map(|next| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let db = Connection::open(path).unwrap();
                db.busy_timeout(Duration::from_secs(2)).unwrap();
                barrier.wait();
                (next, transition_ai_run(&db, "run-1", &["running"], next))
            })
        });
        barrier.wait();
        let results = contenders.map(|thread| thread.join().unwrap());
        let winners = results
            .iter()
            .filter(|(_, result)| result.is_ok())
            .collect::<Vec<_>>();
        assert_eq!(winners.len(), 1, "{results:?}");
        let final_status: String = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT status FROM ai_classification_runs WHERE id='run-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(final_status, winners[0].0);
    }
    remove_migration_test_files(&path);
}

#[test]
fn ai_run_live_lease_is_not_normalized_and_expired_owner_is_resumable() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(
            id,model,profile,prompt_version,status,total,started_at,owner_session,lease_expires_at
         ) VALUES ('live','test','laptop','v1','running',0,'now',?1,200)",
        params![APP_SESSION_ID.as_str()],
    )
    .unwrap();
    assert_eq!(normalize_orphaned_ai_runs(&db, 100).unwrap(), 0);
    let status: String = db
        .query_row(
            "SELECT status FROM ai_classification_runs WHERE id='live'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "running");
    assert_eq!(normalize_orphaned_ai_runs(&db, 201).unwrap(), 1);
    let resumed: (String, Option<String>) = db
        .query_row(
            "SELECT status,owner_session FROM ai_classification_runs WHERE id='live'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(resumed, ("paused".to_owned(), None));
}

#[test]
fn ai_run_foreign_session_is_normalized_even_with_live_lease() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(
            id,model,profile,prompt_version,status,total,started_at,owner_session,lease_expires_at
         ) VALUES
         ('alien','test','laptop','v1','pausing',0,'now','other-session',999999),
         ('mine','test','laptop','v1','running',0,'now',?1,999999)",
        params![APP_SESSION_ID.as_str()],
    )
    .unwrap();
    assert_eq!(normalize_orphaned_ai_runs(&db, 100).unwrap(), 1);
    let alien: (String, Option<String>, Option<String>) = db
        .query_row(
            "SELECT status,owner_session,interruption_reason FROM ai_classification_runs WHERE id='alien'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(alien.0, "paused");
    assert_eq!(alien.1, None);
    assert_eq!(
        alien.2.as_deref(),
        Some("Esecuzione interrotta dalla chiusura precedente")
    );
    let mine: String = db
        .query_row(
            "SELECT status FROM ai_classification_runs WHERE id='mine'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mine, "running");
}

#[test]
fn shutdown_pauses_only_runs_owned_by_the_current_session() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs
         (id,model,profile,prompt_version,status,total,started_at,owner_session,lease_expires_at)
         VALUES
         ('owned','test','laptop','v1','running',1,'now','session-a',999),
         ('other','test','laptop','v1','running',1,'now','session-b',999)",
        [],
    )
    .unwrap();
    assert_eq!(
        pause_owned_ai_runs_on_shutdown(&db, "session-a").unwrap(),
        1
    );
    let owned: (String, Option<String>, Option<i64>, Option<String>) = db
        .query_row(
            "SELECT status,owner_session,lease_expires_at,interruption_reason
             FROM ai_classification_runs WHERE id='owned'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(owned.0, "paused");
    assert_eq!(owned.1, None);
    assert_eq!(owned.2, None);
    assert_eq!(
        owned.3.as_deref(),
        Some("Pausa automatica alla chiusura dell'app")
    );
    let other: String = db
        .query_row(
            "SELECT status FROM ai_classification_runs WHERE id='other'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(other, "running");
}

#[test]
fn ai_run_atomic_claim_allows_only_one_connection() {
    let path = migration_test_path("ai-claim");
    {
        let mut seed = Connection::open(&path).unwrap();
        migrate_database(&mut seed, Some(&path), false).unwrap();
        seed.execute(
            "INSERT INTO ai_classification_runs(
                    id,model,profile,prompt_version,status,total,started_at,request_json
                 ) VALUES ('run-claim','test','laptop','v1','paused',0,'now','{}')",
            [],
        )
        .unwrap();
    }
    let mut first = Connection::open(&path).unwrap();
    let mut second = Connection::open(&path).unwrap();
    claim_ai_run(&mut first, "run-claim", 100).unwrap();
    assert!(claim_ai_run(&mut second, "run-claim", 100).is_err());
    let owner: String = first
        .query_row(
            "SELECT owner_session FROM ai_classification_runs WHERE id='run-claim'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner, APP_SESSION_ID.as_str());
    drop(first);
    drop(second);
    remove_migration_test_files(&path);
}

#[test]
fn ai_run_lease_renewal_is_owner_guarded() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at,owner_session,lease_expires_at
             ) VALUES ('run-1','test','laptop','v1','running',0,'now','owner-a',1)",
            [],
        )
        .unwrap();
    assert!(!renew_ai_lease(&db, "run-1", "owner-b", 10, 45).unwrap());
    assert!(renew_ai_lease(&db, "run-1", "owner-a", 10, 45).unwrap());
}

#[test]
fn ai_run_history_labels_added_modified_review_and_unchanged() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs
             (id,model,profile,prompt_version,status,total,started_at)
             VALUES ('run-1','test','laptop','v1','paused',1,'now')",
        [],
    )
    .unwrap();
    let contact = test_contact();
    record_ai_field_change(
        &db,
        "run-1",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "comune",
            old_value: None,
            new_value: Some("Torino"),
            confidence: 0.9,
            evidence: &[],
            pending_review: false,
        },
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "run-1",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "provincia",
            old_value: Some("Milano"),
            new_value: Some("Torino"),
            confidence: 0.8,
            evidence: &[],
            pending_review: false,
        },
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "run-1",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "regione",
            old_value: Some("Lazio"),
            new_value: Some("Piemonte"),
            confidence: 0.7,
            evidence: &[],
            pending_review: true,
        },
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "run-1",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "category",
            old_value: Some("Varie"),
            new_value: Some(" varie "),
            confidence: 0.9,
            evidence: &[],
            pending_review: false,
        },
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "run-1",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "subcategory",
            old_value: None,
            new_value: None,
            confidence: 0.9,
            evidence: &[],
            pending_review: false,
        },
    )
    .unwrap();
    let kinds: Vec<String> = db
        .prepare("SELECT change_kind FROM ai_field_changes ORDER BY field_name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(kinds, vec!["aggiunto", "modificato", "in_revisione"]);
}

fn geographic_proposal(place: &str) -> GeographicResult {
    geographic_claims(&[("municipality", place)])
}

fn geographic_claims(claims: &[(&str, &str)]) -> GeographicResult {
    GeographicResult {
        outcome: "proposal".to_owned(),
        country: None,
        claims: claims
            .iter()
            .map(|(level, place)| GeographicPlaceClaim {
                observed_place: (*place).to_owned(),
                level_hint: Some((*level).to_owned()),
                evidence: vec![GroundedSourceReference {
                    source_id: "S1".to_owned(),
                    field: "Sede".to_owned(),
                    quote: format!("{level} {place}"),
                }],
            })
            .collect(),
        summary: Some("Luoghi osservati".to_owned()),
    }
}

fn setup_geographic_apply_test() -> (Connection, ContactRecord) {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let contact = test_contact();
    insert_contact_record(&db, &contact).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at) VALUES ('geo-run','test','laptop','geography-v2','running',1,'now')",
        [],
    )
    .unwrap();
    (db, contact)
}

#[test]
fn geographic_apply_updates_only_empty_fields_and_records_actual_changes() {
    let (db, contact) = setup_geographic_apply_test();
    db.execute(
        "UPDATE contacts SET provincia='Provincia stabilita' WHERE id=?1",
        params![contact.id],
    )
    .unwrap();

    assert_eq!(
        apply_geography_result(&db, &contact, &geographic_proposal("Torino"), "geo-run").unwrap(),
        "updated"
    );
    let values: (Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT comune,provincia,regione FROM contacts WHERE id=?1",
            params![contact.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        values,
        (
            Some("Torino".to_owned()),
            Some("Provincia stabilita".to_owned()),
            Some("Piemonte".to_owned())
        )
    );
    let changes: Vec<(String, Option<String>, Option<String>)> = db
        .prepare("SELECT field_name,old_value,new_value FROM ai_field_changes ORDER BY field_name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        changes,
        vec![
            ("comune".to_owned(), None, Some("Torino".to_owned())),
            ("regione".to_owned(), None, Some("Piemonte".to_owned())),
        ]
    );
}

#[test]
fn geographic_apply_reconciles_compatible_same_and_cross_level_claims() {
    let (db, contact) = setup_geographic_apply_test();

    assert_eq!(
        apply_geography_result(
            &db,
            &contact,
            &geographic_claims(&[
                ("province", "Milano"),
                ("municipality", "Milano"),
                ("municipality", "Milano"),
                ("region", "Lombardia"),
            ]),
            "geo-run",
        )
        .unwrap(),
        "updated"
    );
    let values: (Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT comune,provincia,regione FROM contacts WHERE id=?1",
            params![contact.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        values,
        (
            Some("Milano".into()),
            Some("Milano".into()),
            Some("Lombardia".into())
        )
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM classification_reviews", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn geographic_apply_abstains_without_review_for_incompatible_claims() {
    let (db, contact) = setup_geographic_apply_test();
    let result = geographic_claims(&[("municipality", "Milano"), ("province", "Torino")]);

    assert_eq!(
        apply_geography_result(&db, &contact, &result, "geo-run").unwrap(),
        "skipped"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM classification_reviews", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn geographic_apply_abstains_without_review_for_incompatible_same_level_claims() {
    let (db, contact) = setup_geographic_apply_test();
    let result = geographic_claims(&[("municipality", "Milano"), ("municipality", "Torino")]);

    assert_eq!(
        apply_geography_result(&db, &contact, &result, "geo-run").unwrap(),
        "skipped"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM classification_reviews", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM ai_field_changes", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn geographic_apply_abstains_without_review_when_any_claim_is_ambiguous_or_invalid() {
    let (db, contact) = setup_geographic_apply_test();
    let result = geographic_claims(&[("municipality", "Milano"), ("municipality", "Non Esiste")]);

    assert_eq!(
        apply_geography_result(&db, &contact, &result, "geo-run").unwrap(),
        "skipped"
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM classification_reviews WHERE outcome='geography_ambiguous'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn geographic_apply_preserves_all_established_values_without_history() {
    let (db, contact) = setup_geographic_apply_test();
    db.execute(
        "UPDATE contacts SET comune='Milano',provincia='Milano',regione='Lombardia' WHERE id=?1",
        params![contact.id],
    )
    .unwrap();

    assert_eq!(
        apply_geography_result(&db, &contact, &geographic_proposal("Torino"), "geo-run").unwrap(),
        "skipped"
    );
    let values: (String, String, String) = db
        .query_row(
            "SELECT comune,provincia,regione FROM contacts WHERE id=?1",
            params![contact.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        values,
        (
            "Milano".to_owned(),
            "Milano".to_owned(),
            "Lombardia".to_owned()
        )
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM ai_field_changes", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn geographic_apply_reports_no_op_when_proposal_has_no_eligible_value() {
    let (db, contact) = setup_geographic_apply_test();

    assert_eq!(
        apply_geography_result(&db, &contact, &geographic_proposal("Torino"), "geo-run").unwrap(),
        "updated"
    );
    let changes_after_first_apply: i64 = db
        .query_row("SELECT count(*) FROM ai_field_changes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(changes_after_first_apply, 3);
    assert_eq!(
        apply_geography_result(&db, &contact, &geographic_proposal("Torino"), "geo-run").unwrap(),
        "skipped"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM ai_field_changes", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        changes_after_first_apply
    );
}

#[test]
fn ai_geography_derives_province_and_region_and_rejects_invalid_province() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let contact = test_contact();
    let suggestion = AiContactSuggestion {
        tipo_email: None,
        categoria: None,
        sottocategoria: None,
        tipo_contatto: None,
        paese: None,
        comune: Some("Arezzo".to_owned()),
        provincia: Some("Arz".to_owned()),
        regione: Some("Toscana".to_owned()),
        confidence: Some(0.9),
        evidence: None,
        reason_code: None,
        conflict_with_existing: None,
    };

    let geography = reconcile_ai_geography(&db, &contact, &suggestion).unwrap();
    assert_eq!(
        geography,
        (
            Some("Arezzo".to_owned()),
            Some("Arezzo".to_owned()),
            Some("Toscana".to_owned())
        )
    );
}

#[test]
fn ai_geography_rejects_a_place_when_multiple_source_urls_do_not_agree() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let mut contact = test_contact();
    contact.source_url = Some(
        "https://example.it/azienda-turistica-livigno/ | https://example.it/promozione-mantovano/"
            .to_owned(),
    );
    let suggestion = AiContactSuggestion {
        tipo_email: None,
        categoria: None,
        sottocategoria: None,
        tipo_contatto: None,
        paese: None,
        comune: Some("Mantova".to_owned()),
        provincia: Some("Mantova".to_owned()),
        regione: Some("Lombardia".to_owned()),
        confidence: Some(0.9),
        evidence: None,
        reason_code: None,
        conflict_with_existing: None,
    };

    assert_eq!(
        reconcile_ai_geography(&db, &contact, &suggestion).unwrap(),
        (None, None, None)
    );
}

#[test]
fn ai_run_checkpoint_commits_each_candidate_at_most_once() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs
             (id,model,profile,prompt_version,status,total,started_at)
             VALUES ('run-1','test','laptop','v1','paused',1,'now')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ai_run_candidates(run_id,contact_id,ordinal,status)
             VALUES ('run-1','contact-1',0,'processed')",
        [],
    )
    .unwrap();
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM ai_run_candidates WHERE run_id='run-1' AND status='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
}

#[test]
fn ai_run_progress_metrics_balance_updated_review_skipped_and_errors() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs
             (id,model,profile,prompt_version,status,total,processed,updated,reviews_created,skipped,errors,started_at,owner_session)
             VALUES ('run-metrics','test','laptop','v1','running',4,0,0,0,0,0,'now','test-session')",
        [],
    )
    .unwrap();

    let cases = [(1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1)];

    for (updated_delta, review_delta, skipped_delta, error_delta) in cases {
        db.execute(
            "UPDATE ai_classification_runs SET processed=processed+1,updated=updated+?1,
             reviews_created=reviews_created+?2,skipped=skipped+?3,errors=errors+?4
             WHERE id='run-metrics' AND owner_session='test-session'",
            params![updated_delta, review_delta, skipped_delta, error_delta],
        )
        .unwrap();
    }

    let (total, processed, updated, reviews_created, skipped, errors): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = db
        .query_row(
            "SELECT total,processed,updated,reviews_created,skipped,errors FROM ai_classification_runs WHERE id='run-metrics'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();

    assert_eq!(total, 4);
    assert_eq!(processed, 4);
    assert_eq!(updated, 1);
    assert_eq!(reviews_created, 1);
    assert_eq!(skipped, 1);
    assert_eq!(errors, 1);
    assert_eq!(processed, updated + reviews_created + skipped + errors);
}

#[test]
fn deleting_ai_run_removes_raw_artifacts_but_preserves_applied_provenance() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    db.execute("INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at) VALUES('run-retain','model@sha256:abc','desktop','taxonomy-v7','completed',1,'now')", []).unwrap();
    db.execute("INSERT INTO ai_run_artifacts(id,run_id,contact_id,input_hash,system_prompt,user_prompt,raw_response,retry_response,validated_result,operational_log,validation_errors,runtime_metadata,catalog_version,model_identity,model_digest,settings_json,mode,created_at) VALUES('artifact','run-retain','contact-1','hash','secret-system','secret-user','raw-initial','raw-final','{\"outcome\":\"proposal\"}','log','[\"first.invalid\"]','{\"ollama\":\"1\"}','taxonomy-v1','model','sha256:abc','{\"thinking\":false}','taxonomy','now')", []).unwrap();
    db.execute(
        "UPDATE contacts SET category='Istruzione' WHERE id='contact-1'",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO ai_field_changes(id,run_id,contact_id,field_name,old_value,new_value,change_kind,status,confidence,evidence,created_at) VALUES('change','run-retain','contact-1','category','Varie','Istruzione','modificato','applied',1.0,'[{\"source_id\":\"S1\"}]','now')", []).unwrap();

    let export_path = migration_test_path("artifact-export-exclusion").with_extension("csv");
    export_contacts_to_path(&db, &ListFilters::default(), &export_path).unwrap();
    let exported = std::fs::read_to_string(&export_path).unwrap();
    std::fs::remove_file(&export_path).unwrap();
    for secret in [
        "secret-system",
        "secret-user",
        "raw-initial",
        "raw-final",
        "first.invalid",
    ] {
        assert!(
            !exported.contains(secret),
            "ordinary exports must exclude raw AI artifacts"
        );
    }

    assert!(
        delete_ai_run_from_db(&mut db, "run-retain")
            .unwrap_err()
            .contains("Ripristina prima")
    );
    crate::ai_runs::rollback_ai_run_conn(&mut db, "run-retain").unwrap();
    delete_ai_run_from_db(&mut db, "run-retain").unwrap();

    for table in [
        "ai_classification_runs",
        "ai_run_artifacts",
        "ai_field_changes",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table} must be removed with the run");
    }
    let provenance: (Option<String>, String, String, String) = db
        .query_row(
            "SELECT run_id,value,model,prompt_version FROM ai_applied_provenance WHERE id='change'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        provenance,
        (
            None,
            "Istruzione".into(),
            "model@sha256:abc".into(),
            "taxonomy-v7".into()
        )
    );
}
#[test]
fn ingestion_hot_path_indexes_are_migrated() {
    let db = Connection::open_in_memory().expect("database");
    setup_schema(&db).expect("schema");
    for index in [
        "idx_verification_candidate_method",
        "idx_classification_claims_source_row",
        "idx_quality_scores_contact_version",
    ] {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                params![index],
                |row| row.get(0),
            )
            .expect("index lookup");
        assert!(exists, "missing import performance index {index}");
    }
}

#[test]
fn compact_unrecoverable_candidates_migration_preserves_temporary_dns_and_cleans_unrecoverable() {
    let mut db = Connection::open_in_memory().expect("database");
    migrate_database(&mut db, None, false).expect("migration");

    db.execute(
        "INSERT INTO email_candidates(id,raw_email,canonical_email,comparison_key,status,candidate_status,current_disposition,disposition_reason,first_seen_at,last_seen_at)
         VALUES ('c-syntax','bad@@test','bad@@test','bad@@test','invalid_syntax','invalid_syntax','invalid_syntax','syntax_invalid','now','now'),
                ('c-noroute','info@noroute.test','info@noroute.test','info@noroute.test','no_routing','no_routing','no_routing','domain_no_routing','now','now'),
                ('c-suppressed','blocked@test.it','blocked@test.it','blocked@test.it','suppressed','suppressed','suppressed','blacklist','now','now'),
                ('c-noncontact','noreply@test.it','noreply@test.it','noreply@test.it','suppressed','suppressed','suppressed','non_contact_mailbox','now','now'),
                ('c-tempdns','recoverable@temp.test','recoverable@temp.test','recoverable@temp.test','temporary_dns_failure','temporary_dns_failure','temporary_dns_failure','dns_unverified','now','now')",
        [],
    ).unwrap();

    crate::schema::migrate_compact_unrecoverable_candidates(&db).unwrap();

    let remaining: Vec<String> = db
        .prepare("SELECT id FROM email_candidates ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(remaining, vec!["c-tempdns".to_string()]);
}

#[test]
#[ignore = "ordinary classification reviews were replaced by taxonomy-gap proposals"]
fn classification_review_approval_syncs_candidate_ledger_and_verification_events() {
    let mut db = Connection::open_in_memory().expect("database");
    migrate_database(&mut db, None, false).expect("migration");

    let now = chrono::Utc::now().to_rfc3339();

    db.execute(
        "INSERT INTO contacts(id, email, email_local_part, email_domain, email_tld, status, validation_notes,
         source_file, category, subcategory, needs_review)
         VALUES ('contact-rev-1', 'info@comune.test.it', 'info', 'comune.test.it', '.it', 'valid', '[]',
         'test.csv', 'Varie', NULL, 1)",
        [],
    ).unwrap();

    db.execute(
        "INSERT INTO email_candidates(id, raw_email, canonical_email, comparison_key, contact_id, status, candidate_status, current_disposition, category, subcategory, first_seen_at, last_seen_at)
         VALUES ('cand-rev-1', 'info@comune.test.it', 'info@comune.test.it', 'info@comune.test.it', 'contact-rev-1', 'linked', 'linked', 'linked', 'Varie', NULL, ?1, ?1)",
        params![now],
    ).unwrap();

    db.execute(
        "INSERT INTO classification_reviews(id, contact_id, current_category, current_subcategory, proposed_category, proposed_subcategory, confidence, evidence, model, profile, prompt_version, status, created_at)
         VALUES ('rev-1', 'contact-rev-1', 'Varie', NULL, 'Pubblica amministrazione', 'Comune', 0.95, '[]', 'test-model', 'default', 'taxonomy-v7', 'pending', ?1)",
        params![now],
    ).unwrap();

    db.execute(
        "INSERT INTO ai_classification_runs(id, model, profile, prompt_version, status, total, started_at)
         VALUES ('run-test-1', 'test-model', 'default', 'taxonomy-v7', 'running', 1, ?1)",
        params![now],
    ).unwrap();

    db.execute(
        "INSERT INTO ai_field_changes(id, run_id, contact_id, review_id, field_name, old_value, new_value, change_kind, status, confidence, evidence, created_at)
         VALUES ('afc-1', 'run-test-1', 'contact-rev-1', 'rev-1', 'category', 'Varie', 'Pubblica amministrazione', 'in_revisione', 'pending_review', 0.95, '[]', ?1)",
        params![now],
    ).unwrap();

    approve_classification_review_conn(&mut db, "rev-1", None, None).unwrap();

    let contact_cat: (String, Option<String>, i64, String, f64) = db.query_row(
        "SELECT category, subcategory, needs_review, classification_method, classification_confidence FROM contacts WHERE id='contact-rev-1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).unwrap();
    assert_eq!(contact_cat.0, "Pubblica amministrazione");
    assert_eq!(contact_cat.1, Some("Comune".to_string()));
    assert_eq!(contact_cat.2, 0);
    assert_eq!(contact_cat.3, "manual");
    assert_eq!(contact_cat.4, 1.0);

    let cand_cat: (String, Option<String>) = db
        .query_row(
            "SELECT category, subcategory FROM email_candidates WHERE id='cand-rev-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cand_cat.0, "Pubblica amministrazione");
    assert_eq!(cand_cat.1, Some("Comune".to_string()));

    let rev_status: String = db
        .query_row(
            "SELECT status FROM classification_reviews WHERE id='rev-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rev_status, "approved");

    let afc_status: String = db
        .query_row(
            "SELECT status FROM ai_field_changes WHERE id='afc-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(afc_status, "applied");

    let verif_event: (String, Option<String>, Option<String>, String, String) = db.query_row(
        "SELECT comparison_key, candidate_id, contact_id, method, routing_status FROM verification_events WHERE contact_id='contact-rev-1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).unwrap();
    assert_eq!(verif_event.0, "info@comune.test.it");
    assert_eq!(verif_event.1, Some("cand-rev-1".to_string()));
    assert_eq!(verif_event.2, Some("contact-rev-1".to_string()));
    assert_eq!(verif_event.3, "manual_review");
    assert_eq!(verif_event.4, "routing_ok");
}

#[test]
#[ignore = "ordinary classification reviews were replaced by taxonomy-gap proposals"]
fn classification_review_rejection_updates_status_and_verification_events() {
    let mut db = Connection::open_in_memory().expect("database");
    migrate_database(&mut db, None, false).expect("migration");

    let now = chrono::Utc::now().to_rfc3339();

    db.execute(
        "INSERT INTO contacts(id, email, email_local_part, email_domain, email_tld, status, validation_notes,
         source_file, category, subcategory, needs_review)
         VALUES ('contact-rev-2', 'test@scuola.test.it', 'test', 'scuola.test.it', '.it', 'valid', '[]',
         'test.csv', 'Varie', NULL, 1)",
        [],
    ).unwrap();

    db.execute(
        "INSERT INTO email_candidates(id, raw_email, canonical_email, comparison_key, contact_id, status, candidate_status, current_disposition, category, subcategory, first_seen_at, last_seen_at)
         VALUES ('cand-rev-2', 'test@scuola.test.it', 'test@scuola.test.it', 'test@scuola.test.it', 'contact-rev-2', 'linked', 'linked', 'linked', 'Varie', NULL, ?1, ?1)",
        params![now],
    ).unwrap();

    db.execute(
        "INSERT INTO classification_reviews(id, contact_id, current_category, current_subcategory, proposed_category, proposed_subcategory, confidence, evidence, model, profile, prompt_version, status, created_at)
         VALUES ('rev-2', 'contact-rev-2', 'Varie', NULL, 'Istruzione', 'Scuola', 0.8, '[]', 'test-model', 'laptop', 'taxonomy-v7', 'pending', ?1)",
        params![now],
    ).unwrap();

    db.execute(
        "INSERT INTO ai_classification_runs(id, model, profile, prompt_version, status, total, started_at)
         VALUES ('run-test-2', 'test-model', 'default', 'taxonomy-v7', 'running', 1, ?1)",
        params![now],
    ).unwrap();

    db.execute(
        "INSERT INTO ai_field_changes(id, run_id, contact_id, review_id, field_name, old_value, new_value, change_kind, status, confidence, evidence, created_at)
         VALUES ('afc-2', 'run-test-2', 'contact-rev-2', 'rev-2', 'category', 'Varie', 'Istruzione', 'in_revisione', 'pending_review', 0.8, '[]', ?1)",
        params![now],
    ).unwrap();

    reject_classification_review_conn(&mut db, "rev-2").unwrap();

    let contact_data: (String, Option<String>, i64) = db
        .query_row(
            "SELECT category, subcategory, needs_review FROM contacts WHERE id='contact-rev-2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(contact_data.0, "Varie");
    assert_eq!(contact_data.1, None);
    assert_eq!(contact_data.2, 0);

    let cand_cat: (String, Option<String>) = db
        .query_row(
            "SELECT category, subcategory FROM email_candidates WHERE id='cand-rev-2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cand_cat.0, "Varie");
    assert_eq!(cand_cat.1, None);

    let rev_status: String = db
        .query_row(
            "SELECT status FROM classification_reviews WHERE id='rev-2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rev_status, "rejected");

    let afc_status: String = db
        .query_row(
            "SELECT status FROM ai_field_changes WHERE id='afc-2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(afc_status, "rejected");

    let verif_event: (String, Option<String>, Option<String>, String, String) = db.query_row(
        "SELECT comparison_key, candidate_id, contact_id, method, routing_status FROM verification_events WHERE contact_id='contact-rev-2'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    ).unwrap();
    assert_eq!(verif_event.0, "test@scuola.test.it");
    assert_eq!(verif_event.1, Some("cand-rev-2".to_string()));
    assert_eq!(verif_event.2, Some("contact-rev-2".to_string()));
    assert_eq!(verif_event.3, "manual_review_rejected");
    assert_eq!(verif_event.4, "routing_ok");
}

#[test]
fn taxonomy_apply_resolves_foreign_keys_and_records_audit_trail() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let contact = test_contact();
    insert_contact_record(&db, &contact).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at) VALUES ('tax-run','test','laptop','taxonomy-v7','running',1,'now')",
        [],
    )
    .unwrap();

    let result = TaxonomyResult {
        outcome: TaxonomyOutcome::Proposal,
        candidates: vec![TaxonomyCandidate {
            category_key: "istruzione".to_owned(),
            subcategory_key: Some("primaria".to_owned()),
            category_grade: EvidenceGrade::Direct,
            subcategory_grade: Some(EvidenceGrade::Direct),
            category_evidence: vec![GroundedSourceReference {
                source_id: "S1".to_owned(),
                field: "Name".to_owned(),
                quote: "Scuola Primaria Carducci".to_owned(),
            }],
            subcategory_evidence: vec![GroundedSourceReference {
                source_id: "S1".to_owned(),
                field: "Name".to_owned(),
                quote: "Scuola Primaria Carducci".to_owned(),
            }],
        }],
        gap: None,
        summary: Some("Scuola primaria statale".to_owned()),
    };

    assert_eq!(
        apply_taxonomy_result(
            &db,
            &contact,
            &result,
            "model-test",
            "hash-123",
            "taxonomy-v1",
            "tax-run"
        )
        .unwrap(),
        "updated"
    );

    let (cat, sub, cat_id, sub_id, method, hash): (String, Option<String>, Option<i64>, Option<i64>, String, Option<String>) = db.query_row(
        "SELECT category, subcategory, category_id, subcategory_id, classification_method, classification_hash FROM contacts WHERE id=?1",
        params![contact.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).unwrap();

    assert_eq!(cat, "Istruzione");
    assert_eq!(sub, Some("Primaria".to_owned()));
    assert!(cat_id.is_some());
    assert!(sub_id.is_some());
    assert_eq!(method, "ai");
    assert_eq!(hash, Some("hash-123".to_owned()));

    let expected_cat_id: i64 = db
        .query_row(
            "SELECT id FROM categories WHERE name='Istruzione' AND active=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let expected_sub_id: i64 = db.query_row("SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name='Istruzione' AND s.name='Primaria' AND s.active=1", [], |r| r.get(0)).unwrap();
    assert_eq!(cat_id, Some(expected_cat_id));
    assert_eq!(sub_id, Some(expected_sub_id));

    type ChangeRow = (
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        f64,
        String,
    );
    let changes: Vec<ChangeRow> = db
        .prepare("SELECT field_name, old_value, new_value, change_kind, status, confidence, evidence FROM ai_field_changes WHERE run_id='tax-run' ORDER BY field_name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(changes.len(), 5);
    let metadata: (String, String) = db
        .query_row(
            "SELECT v.semantic_version,c.classification_state
             FROM contacts c JOIN taxonomy_versions v ON v.id=c.taxonomy_version_id
             WHERE c.id='contact-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        metadata,
        ("taxonomy-v1".to_owned(), "classified".to_owned())
    );
    let category_change = changes
        .iter()
        .find(|change| change.0 == "category")
        .unwrap();
    assert_eq!(category_change.1, Some("Varie".to_owned()));
    assert_eq!(category_change.2, Some("Istruzione".to_owned()));
    assert_eq!(category_change.3, "modificato");
    assert_eq!(category_change.4, "applied");
    assert!((category_change.5 - 0.9).abs() < 1e-6);
    assert!(
        category_change
            .6
            .contains("[S1 · Name] Scuola Primaria Carducci"),
        "grounded citation missing: {}",
        category_change.6
    );
    assert!(
        category_change
            .6
            .contains("Motivo: Scuola primaria statale")
    );

    let subcategory_change = changes
        .iter()
        .find(|change| change.0 == "subcategory")
        .unwrap();
    assert_eq!(subcategory_change.1, None);
    assert_eq!(subcategory_change.2, Some("Primaria".to_owned()));
    assert_eq!(subcategory_change.3, "aggiunto");
    assert_eq!(subcategory_change.4, "applied");
    assert!((subcategory_change.5 - 0.9).abs() < 1e-6);
    assert!(
        subcategory_change
            .6
            .contains("[S1 · Name] Scuola Primaria Carducci"),
        "grounded citation missing: {}",
        subcategory_change.6
    );

    let list_query_changes: Vec<AiFieldChange> = db
        .prepare(
            "SELECT h.id,h.run_id,h.contact_id,h.review_id,c.email,h.field_name,h.old_value,h.new_value,
                    h.change_kind,h.status,h.confidence,h.evidence,h.created_at,h.resolved_at
             FROM ai_field_changes h JOIN contacts c ON c.id=h.contact_id
             WHERE h.run_id='tax-run'
             ORDER BY h.field_name ASC",
        )
        .unwrap()
        .query_map([], |row| {
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
                rolled_back: false,
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(list_query_changes.len(), 5);
    let listed_category = list_query_changes
        .iter()
        .find(|change| change.field_name == "category")
        .unwrap();
    assert_eq!(listed_category.email, contact.email);
    assert_eq!(
        listed_category.evidence,
        vec![
            "[S1 · Name] Scuola Primaria Carducci".to_owned(),
            "[S1 · Name] Scuola Primaria Carducci".to_owned(),
            "Motivo: Scuola primaria statale".to_owned(),
        ]
    );
    let listed_subcategory = list_query_changes
        .iter()
        .find(|change| change.field_name == "subcategory")
        .unwrap();
    assert_eq!(listed_subcategory.email, contact.email);
    assert!(!listed_subcategory.evidence.is_empty());
}

#[test]
fn taxonomy_apply_autonomously_replaces_classification_and_records_metadata() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let contact = test_contact();
    insert_contact_record(&db, &contact).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at) VALUES ('tax-run-lock','test','laptop','taxonomy-v7','running',1,'now')",
        [],
    )
    .unwrap();

    let result = TaxonomyResult {
        outcome: TaxonomyOutcome::Proposal,
        candidates: vec![TaxonomyCandidate {
            category_key: "istruzione".to_owned(),
            subcategory_key: Some("primaria".to_owned()),
            category_grade: EvidenceGrade::Direct,
            subcategory_grade: Some(EvidenceGrade::Direct),
            category_evidence: vec![GroundedSourceReference {
                source_id: "S1".to_owned(),
                field: "Name".to_owned(),
                quote: "Scuola Primaria".to_owned(),
            }],
            subcategory_evidence: vec![GroundedSourceReference {
                source_id: "S1".to_owned(),
                field: "Name".to_owned(),
                quote: "Scuola Primaria".to_owned(),
            }],
        }],
        gap: None,
        summary: Some("Scuola primaria".to_owned()),
    };

    assert_eq!(
        apply_taxonomy_result(
            &db,
            &contact,
            &result,
            "model-test",
            "hash-lock",
            "taxonomy-v1",
            "tax-run-lock"
        )
        .unwrap(),
        "updated"
    );

    let (cat, sub): (String, Option<String>) = db
        .query_row(
            "SELECT category, subcategory FROM contacts WHERE id=?1",
            params![contact.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert_eq!(cat, "Istruzione");
    assert_eq!(sub, Some("Primaria".to_owned()));

    let changes: Vec<(String, Option<String>, Option<String>)> = db
        .prepare("SELECT field_name, old_value, new_value FROM ai_field_changes WHERE run_id='tax-run-lock'")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let fields = changes
        .iter()
        .map(|(field, _, _)| field.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(fields.contains("category"));
    assert!(fields.contains("subcategory"));
    assert!(fields.contains("taxonomy_version_id"));
    assert!(fields.contains("classification_state"));
}

#[test]
fn taxonomy_has_unique_canonical_subcategories_and_resolvable_aliases() {
    let mut all_keys = std::collections::BTreeSet::new();
    for (category, subcategories) in TAXONOMY {
        let mut category_keys = std::collections::BTreeSet::new();
        for subcategory in *subcategories {
            let key = crate::gazetteer::norm(subcategory);
            assert!(
                category_keys.insert(key.clone()),
                "duplicate subcategory in {category}: {subcategory}"
            );
            assert!(
                all_keys.insert(key),
                "subcategory reused by multiple categories: {subcategory}"
            );
        }
    }
    for &(category, alias, canonical) in crate::taxonomy::SUBCATEGORY_ALIASES {
        let subcategories = TAXONOMY
            .iter()
            .find(|(candidate, _)| *candidate == category)
            .unwrap()
            .1;
        assert!(subcategories.contains(&canonical));
        assert!(!subcategories.contains(&alias));
        assert_eq!(
            crate::taxonomy::canonical_subcategory_label(category, alias),
            Some(canonical)
        );
    }
}

#[test]
fn canonical_subcategory_migration_repairs_every_known_alias_and_foreign_key() {
    let db = Connection::open_in_memory().expect("in-memory db");
    let error = run_ordered_migrations(&db, Some(38)).expect_err("stop before migration 38");
    assert!(error.contains("canonical-subcategory-labels"));

    for (index, &(category, alias, _)) in crate::taxonomy::SUBCATEGORY_ALIASES.iter().enumerate() {
        db.execute(
            "INSERT INTO contacts(
                id,email,email_local_part,email_domain,email_tld,status,validation_notes,
                source_file,category,subcategory,contact_kind,paese
             ) VALUES(?1,?2,'alias','example.test','test','valid','[]','fixture',?3,?4,
                      'istituzionale','Italia')",
            params![
                format!("alias-{index}"),
                format!("alias-{index}@example.test"),
                category,
                alias
            ],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO email_candidates(
            id,raw_email,category,subcategory,first_seen_at,last_seen_at
         ) VALUES('candidate-alias','candidate@example.test','Associazioni',
                  'Volontariato / Assistenza','now','now')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO classification_claims(
            id,contact_id,category,subcategory,confidence,created_at
         ) VALUES('claim-alias','alias-0','Pubblica amministrazione',
                  'Forze dell''ordine / Sicurezza',1,'now')",
        [],
    )
    .unwrap();

    // Stop before migration 68, which deletes the forze dell'ordine contacts.
    run_ordered_migrations(&db, Some(68)).expect_err("apply canonical label migration");
    run_ordered_migrations(&db, Some(68)).expect_err("idempotent reopen");

    for (index, &(category, _, canonical)) in
        crate::taxonomy::SUBCATEGORY_ALIASES.iter().enumerate()
    {
        let (subcategory, linked_name): (String, String) = db
            .query_row(
                "SELECT c.subcategory,s.name FROM contacts c
                 JOIN subcategories s ON s.id=c.subcategory_id
                 JOIN categories cat ON cat.id=s.category_id
                 WHERE c.id=?1 AND cat.name=?2",
                params![format!("alias-{index}"), category],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(subcategory, canonical);
        assert_eq!(linked_name, canonical);
    }
    let candidate: String = db
        .query_row(
            "SELECT subcategory FROM email_candidates WHERE id='candidate-alias'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(candidate, "Volontariato / solidarietà");
    let claim: String = db
        .query_row(
            "SELECT subcategory FROM classification_claims WHERE id='claim-alias'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claim, "Forze dell'ordine");

    run_ordered_migrations(&db, None).expect("apply retirement migration");
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(
        count("SELECT COUNT(*) FROM contacts WHERE subcategory='Forze dell''ordine'"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM subcategories WHERE name='Forze dell''ordine' AND active=1"),
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM subcategories WHERE name='Contatto personale' AND active=1"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM taxonomy_entries WHERE display_name='Contatto personale'"),
        0
    );
}

#[test]
fn geography_hash_and_security_taxonomy_migration_adds_column_index_and_subcategory() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    migrate_database(&mut db, None, false).expect("migrate db to latest");

    let contact_columns = table_columns(&db, "contacts").expect("contacts columns");
    assert!(!contact_columns.iter().any(|c| c == "geography_hash"));

    let index_exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_contacts_geography_hash')",
            [],
            |r| r.get(0),
        )
        .expect("index check");
    assert!(!index_exists, "obsolete geography hash index still exists");

    let subcategory: (String, String, Option<String>, Option<String>, i64, i64) = db
        .query_row(
            "SELECT s.name, s.normalized_name, s.semantic_key, s.definition, s.active, s.reviewer_only
             FROM subcategories s
             JOIN categories c ON c.id = s.category_id
             WHERE c.name = 'Pubblica amministrazione' AND s.name = 'Forze dell''ordine'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .expect("Forze dell'ordine subcategory query");

    assert_eq!(subcategory.0, "Forze dell'ordine");
    assert_eq!(subcategory.1, "forze_dell_ordine");
    assert_eq!(subcategory.2, Some("law_enforcement".to_string()));
    assert_eq!(
        subcategory.3,
        Some("{\"it\": \"A police authority, law enforcement agency, military body, armed force unit, or fire and emergency service.\"}".to_string())
    );
    assert_eq!(subcategory.4, 1);
    assert_eq!(subcategory.5, 0);

    crate::schema::migrate_remove_obsolete_geography_hash(&db)
        .expect("idempotent geography hash removal");
}

#[test]
fn geography_hash_and_security_taxonomy_migration_upgrades_from_v25() {
    let mut db = Connection::open_in_memory().expect("in-memory db");

    let error = run_ordered_migrations(&db, Some(26)).expect_err("stop before migration 26");
    assert!(error.contains("geography-hash-and-security-taxonomy"));
    db.pragma_update(None, "user_version", 25).unwrap();

    let columns_v25 = table_columns(&db, "contacts").unwrap();
    if columns_v25.iter().any(|c| c == "geography_hash") {
        db.execute("ALTER TABLE contacts DROP COLUMN geography_hash", [])
            .unwrap();
    }
    let subcategory_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name='Pubblica amministrazione' AND s.name='Forze dell''ordine'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    if subcategory_count > 0 {
        db.execute(
            "DELETE FROM subcategories WHERE name='Forze dell''ordine'",
            [],
        )
        .unwrap();
    }

    migrate_database(&mut db, None, false).expect("upgrade to v26");

    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, crate::schema::LATEST_SCHEMA_VERSION);

    let contact_columns = table_columns(&db, "contacts").expect("contacts columns");
    assert!(!contact_columns.iter().any(|c| c == "geography_hash"));

    let (sem_key, norm_name, def): (Option<String>, String, Option<String>) = db
        .query_row(
            "SELECT s.semantic_key, s.normalized_name, s.definition
             FROM subcategories s
             JOIN categories c ON c.id = s.category_id
             WHERE c.name = 'Pubblica amministrazione' AND s.name = 'Forze dell''ordine'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("Forze dell'ordine present after migration");

    assert_eq!(sem_key, Some("law_enforcement".to_string()));
    assert_eq!(norm_name, "forze_dell_ordine");
    assert_eq!(
        def,
        Some("{\"it\": \"A police authority, law enforcement agency, military body, armed force unit, or fire and emergency service.\"}".to_string())
    );
}

#[test]
fn deterministic_raw_context_backfill_migration_populates_empty_contact_fields() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate_database(&mut db, None, false).unwrap();

    let raw = serde_json::json!({
        "Denominazione dell'istituto": "REGINA M.CRISTINA",
        "Indirizzo (Via + numero civico)": "V.BALUARDI 1",
        "Codice Postale": "10011",
        "COMUNE": "Aglie'",
        "PROVINCIA": "TORINO",
        "REGIONE": "Piemonte",
        "Email": "asilormc@example.test"
    })
    .to_string();

    db.execute(
        "INSERT INTO contacts(id, email, email_local_part, email_domain, email_tld, status, validation_notes, source_file, category, contact_kind, paese)
         VALUES ('c1', 'asilormc@example.test', 'asilormc', 'example.test', '.test', 'valid', '[]', 'Scuole.csv', 'Istruzione', 'istituzionale', 'Italia')",
        [],
    ).unwrap();

    db.execute(
        "INSERT INTO imports(id, file_name, relative_path, file_hash, status, parsed_rows, contacts_found, imported_at)
         VALUES ('imp1', 'Scuole.csv', 'Scuole.csv', 'hash1', 'completed', 1, 1, '2026-01-01')",
        [],
    ).unwrap();

    db.execute(
        "INSERT INTO contact_sources(id, contact_id, import_id, source_file, source_sheet, source_row, raw_context, source_url)
         VALUES ('cs1', 'c1', 'imp1', 'Scuole.csv', 'Sheet1', 1, ?1, 'https://example.test')",
        params![raw],
    ).unwrap();

    crate::schema::migrate_deterministic_raw_context_backfill(&db).unwrap();

    type RawContextRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let (denom, addr, cap, comune, prov, reg, macroarea): RawContextRow = db.query_row(
        "SELECT denomination, address, postal_code, comune, provincia, regione, macroarea FROM contacts WHERE id='c1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
    ).unwrap();

    assert_eq!(denom, Some("Regina M. Cristina".to_string()));
    assert_eq!(addr, Some("V. Baluardi 1".to_string()));
    assert_eq!(cap, Some("10011".to_string()));
    assert_eq!(comune, Some("Agliè".to_string()));
    assert_eq!(prov, Some("Torino".to_string()));
    assert_eq!(reg, Some("Piemonte".to_string()));
    assert_eq!(macroarea, Some("Nord-Ovest".to_string()));
}

#[test]
fn discarding_pending_reviews_keeps_resolved_and_clears_needs_review() {
    let mut db = Connection::open_in_memory().expect("database");
    migrate_database(&mut db, None, false).expect("migration");
    let now = chrono::Utc::now().to_rfc3339();
    db.execute(
        "INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,validation_notes,source_file,category,needs_review)
         VALUES ('c1','a@example.test','a','example.test','test','valid','[]','f.csv','Varie',1),
                ('c2','b@example.test','b','example.test','test','valid','[]','f.csv','Varie',0)",
        [],
    ).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
         VALUES ('run-d','m','p','taxonomy-v7','completed',2,?1)",
        params![now],
    ).unwrap();
    db.execute(
        "INSERT INTO classification_reviews(id,contact_id,current_category,current_subcategory,proposed_category,proposed_subcategory,confidence,evidence,model,profile,prompt_version,status,created_at)
         VALUES ('rev-pending','c1','Varie',NULL,'Scuole',NULL,0.6,'[]','m','p','taxonomy-v7','pending',?1)",
        params![now],
    ).unwrap();
    db.execute(
        "INSERT INTO classification_reviews(id,contact_id,current_category,current_subcategory,proposed_category,proposed_subcategory,confidence,evidence,model,profile,prompt_version,status,resolved_at,created_at)
         VALUES ('rev-approved','c2','Varie',NULL,'Comuni','Comune',0.9,'[]','m','p','taxonomy-v7','approved',?1,?1)",
        params![now],
    ).unwrap();
    db.execute(
        "INSERT INTO ai_field_changes(id,run_id,contact_id,review_id,field_name,old_value,new_value,change_kind,status,confidence,evidence,created_at)
         VALUES ('afc-p','run-d','c1','rev-pending','category','Varie','Scuole','in_revisione','pending_review',0.6,'[]',?1)",
        params![now],
    ).unwrap();
    db.execute(
        "INSERT INTO ai_field_changes(id,run_id,contact_id,review_id,field_name,old_value,new_value,change_kind,status,confidence,evidence,created_at)
         VALUES ('afc-a','run-d','c2','rev-approved','category','Varie','Comuni','modificato','applied',0.9,'[]',?1)",
        params![now],
    ).unwrap();

    let discarded = discard_pending_classification_reviews_conn(&mut db).unwrap();

    assert_eq!(discarded, 1);
    let pending: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM classification_reviews WHERE status='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    let approved: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM classification_reviews WHERE status='approved'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(approved, 1);
    let afc_p: String = db
        .query_row(
            "SELECT status FROM ai_field_changes WHERE id='afc-p'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(afc_p, "rejected");
    let afc_a: String = db
        .query_row(
            "SELECT status FROM ai_field_changes WHERE id='afc-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(afc_a, "applied");
    let c1_review: i64 = db
        .query_row("SELECT needs_review FROM contacts WHERE id='c1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(c1_review, 0);
}

#[test]
fn retirement_migration_deletes_removed_subcategory_data_and_keeps_taxonomy() {
    let db = Connection::open_in_memory().expect("in-memory db");
    run_ordered_migrations(&db, Some(68)).expect_err("stop before migration 68");
    for (id, category, subcategory) in [
        ("keep", "Cultura", "Biblioteca"),
        ("clinic", "Sanità", "Clinica / IRCCS / Struttura sanitaria"),
        ("police", "Pubblica amministrazione", "Forze dell'ordine"),
        (
            "court",
            "Pubblica amministrazione",
            "Tribunale / Ufficio giudiziario",
        ),
    ] {
        db.execute(
            "INSERT INTO contacts(
                id,email,email_local_part,email_domain,email_tld,status,validation_notes,
                source_file,category,subcategory,contact_kind,paese
             ) VALUES(?1,?2,'x','example.test','test','valid','[]','fixture',?3,?4,
                      'istituzionale','Italia')",
            params![id, format!("{id}@example.test"), category, subcategory],
        )
        .unwrap();
        db.execute(
            "INSERT INTO classification_claims(id,contact_id,category,subcategory,confidence,created_at)
             VALUES(?1,?1,?2,?3,1,'now')",
            params![id, category, subcategory],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO search_seeds(id,list_name,name,subcategory,created_at)
         VALUES('seed-police','l','Questura','Forze dell''ordine','now'),
               ('seed-keep','l','Biblioteca','Biblioteca','now')",
        [],
    )
    .unwrap();

    run_ordered_migrations(&db, None).expect("apply retirement migration");

    let ids = |sql: &str| -> Vec<String> {
        let mut statement = db.prepare(sql).unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(ids("SELECT id FROM contacts"), ["keep"]);
    assert_eq!(ids("SELECT id FROM classification_claims"), ["keep"]);
    assert_eq!(ids("SELECT id FROM search_seeds"), ["seed-keep"]);
    assert_eq!(
        ids("SELECT name FROM subcategories WHERE active=1 AND name IN
             ('Clinica / IRCCS / Struttura sanitaria','Forze dell''ordine','Tribunale / Ufficio giudiziario')
             ORDER BY name")
        .len(),
        3
    );
}

#[test]
fn sanita_migration_deletes_health_contacts_and_rescues_caritas() {
    let db = Connection::open_in_memory().expect("in-memory db");
    run_ordered_migrations(&db, Some(69)).expect_err("stop before migration 69");
    for (id, email, category, subcategory) in [
        (
            "hospital",
            "ospedale@example.test",
            "Sanità",
            "Ospedale / Azienda ospedaliera",
        ),
        (
            "asl",
            "asl@example.test",
            "Sanità",
            "ASL / ASST / Distretto sanitario",
        ),
        (
            "other",
            "rsa@example.test",
            "Sanità",
            "Altro ente sanitario",
        ),
        (
            "caritas",
            "caritas.diocesana@example.test",
            "Sanità",
            "Altro ente sanitario",
        ),
        ("keep", "biblioteca@example.test", "Cultura", "Biblioteca"),
    ] {
        db.execute(
            "INSERT INTO contacts(
                id,email,email_local_part,email_domain,email_tld,status,validation_notes,
                source_file,category,subcategory,contact_kind,paese
             ) VALUES(?1,?2,'x','example.test','test','valid','[]','fixture',?3,?4,
                      'istituzionale','Italia')",
            params![id, email, category, subcategory],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO search_seeds(id,list_name,name,category,created_at)
         VALUES('seed-health','l','Ospedale','Sanità','now'),
               ('seed-keep','l','Biblioteca','Cultura','now')",
        [],
    )
    .unwrap();

    run_ordered_migrations(&db, None).expect("apply sanità migration");

    let ids = |sql: &str| -> Vec<String> {
        let mut statement = db.prepare(sql).unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        ids("SELECT id FROM contacts ORDER BY id"),
        ["caritas", "keep"]
    );
    assert_eq!(ids("SELECT id FROM search_seeds"), ["seed-keep"]);
    let (category, linked): (String, String) = db
        .query_row(
            "SELECT c.category,s.name FROM contacts c JOIN subcategories s ON s.id=c.subcategory_id
             WHERE c.id='caritas'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(category, "Enti religiosi");
    assert_eq!(linked, "Caritas / ente caritativo religioso");
    assert_eq!(
        ids("SELECT name FROM subcategories WHERE active=1 AND name='Altro ente sanitario'").len(),
        1
    );
}

#[test]
fn claims_facet_covering_indexes_exist_after_migration() {
    let mut db = Connection::open_in_memory().expect("database");
    migrate_database(&mut db, None, false).expect("migration");
    for name in [
        "idx_classification_claims_category_contact",
        "idx_classification_claims_subcategory_contact",
    ] {
        let present: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                params![name],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(present, 1, "missing covering index {name}");
    }
}

#[test]
fn deterministic_pass_rejects_non_mechanographic_istruzione_domains() {
    assert!(evaluate_deterministic("0genti.scolastici@istruzione.lombardia.it", None).is_none());
    assert!(evaluate_deterministic("segreteria@istruzione.lombardia.it", None).is_none());
    assert!(evaluate_deterministic("info@pec.provinciadipavia.istruzione.it", None).is_none());

    let res = evaluate_deterministic("MIIC8D0002@istruzione.it", None).unwrap();
    assert_eq!(res.category, "Istruzione");
    assert_eq!(res.subcategory, Some("Istituto comprensivo"));
    assert_eq!(res.provincia.as_deref(), Some("Milano"));
}

#[test]
fn deterministic_pass_fills_placeholders_records_changes_and_never_overwrites_established() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
         VALUES ('run-det','m','default','taxonomy-v9','running',2,'now')",
        [],
    )
    .unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    let mut established = test_contact();
    established.id = "contact-established".to_owned();
    established.email = "info@associazione.test".to_owned();
    established.email_local_part = "info".to_owned();
    established.email_domain = "associazione.test".to_owned();
    established.email_tld = ".test".to_owned();
    established.category = "Associazioni".to_owned();
    established.subcategory = Some("Rotary".to_owned());
    insert_contact_record(&db, &established).unwrap();

    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let res = evaluate_deterministic("protocollo@comune.moncalieri.to.it", None).unwrap();
    apply_deterministic_to_contact(&tx, "run-det", "contact-1", &res).unwrap();
    apply_deterministic_to_contact(&tx, "run-det", "contact-established", &res).unwrap();
    tx.commit().unwrap();

    let (cat, sub, comune, prov): (String, Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT category,subcategory,comune,provincia FROM contacts WHERE id='contact-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(cat, "Pubblica amministrazione");
    assert_eq!(sub.as_deref(), Some("Comune"));
    assert_eq!(comune.as_deref(), Some("Moncalieri"));
    assert_eq!(prov.as_deref(), Some("Torino"));

    let established_cat: String = db
        .query_row(
            "SELECT category FROM contacts WHERE id='contact-established'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(established_cat, "Associazioni");

    let changes: Vec<(String, Option<String>, Option<String>)> = db
        .prepare("SELECT field_name,old_value,new_value FROM ai_field_changes WHERE run_id='run-det' AND contact_id='contact-1' ORDER BY field_name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // macroarea is generated from regione, so it has no history row of its own.
    assert_eq!(changes.len(), 11);
    assert!(changes.iter().all(|(field, _, _)| field != "macroarea"));
    let evidence: String = db
        .query_row(
            "SELECT evidence FROM ai_field_changes WHERE run_id='run-det' AND contact_id='contact-1' AND field_name='category'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(evidence.contains("Passo deterministico"));
}

#[test]
fn deterministic_authority_does_not_replace_ai_classification() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
         VALUES ('run-authority','m','default','taxonomy-v9','running',1,'now')",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE contacts SET category='Istruzione',subcategory='Primaria',
            classification_method='ai',classification_confidence=0.91,
            classification_evidence='[\"ai\"]',classification_hash='ai-hash'
         WHERE id='contact-1'",
        [],
    )
    .unwrap();

    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let result = evaluate_deterministic("protocollo@comune.moncalieri.to.it", None).unwrap();
    apply_deterministic_to_contact_with_rule(
        &tx,
        "run-authority",
        "contact-1",
        &result,
        true,
        DeterministicRule {
            catalog_id: "exact-it-v1",
            rule_id: "test.rule",
            rule_version: 1,
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let values: (String, Option<String>, String, f64, String, Option<String>) = db
        .query_row(
            "SELECT category,subcategory,classification_method,classification_confidence,
                    classification_evidence,classification_hash
             FROM contacts WHERE id='contact-1'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(values.0, "Istruzione");
    assert_eq!(values.1.as_deref(), Some("Primaria"));
    assert_eq!(values.2, "ai");
    assert_eq!(values.3, 0.91);
    assert_eq!(values.4, "[\"ai\"]");
    assert_eq!(values.5.as_deref(), Some("ai-hash"));
}

#[test]
fn import_authority_does_not_replace_inferred_classification() {
    let mut existing = test_contact();
    existing.category = "Istruzione".to_owned();
    existing.subcategory = Some("Primaria".to_owned());
    existing.classification_method = "ai".to_owned();
    existing.classification_confidence = 0.6;

    let incoming = ImportContact {
        raw_email: existing.email.clone(),
        normalized_email: existing.email.clone(),
        comparison_key: existing.email.clone(),
        normalization_method: "literal".to_owned(),
        email_local_part: existing.email_local_part.clone(),
        email_domain: existing.email_domain.clone(),
        email_tld: existing.email_tld.clone(),
        status: EmailStatus::Valid,
        validation_notes: Vec::new(),
        source_file: std::sync::Arc::from("new.csv"),
        category: "Pubblica amministrazione".to_owned(),
        subcategory: Some("Comune".to_owned()),
        contact_kind: "istituzionale".to_owned(),
        paese: "Italia".to_owned(),
        is_pec: false,
        comune: None,
        provincia: None,
        regione: None,
        macroarea: None,
        website: None,
        source_url: None,
        source_sheet: None,
        source_row: 1,
        raw_context: std::sync::Arc::from("{}"),
        classification_method: "column".to_owned(),
        classification_confidence: 1.0,
        classification_evidence: Vec::new(),
    };
    merge_import_contact_into(&mut existing, &incoming, "");

    assert_eq!(existing.category, "Istruzione");
    assert_eq!(existing.subcategory.as_deref(), Some("Primaria"));
    assert_eq!(existing.classification_method, "ai");
    assert_eq!(existing.classification_confidence, 0.6);
}

#[test]
fn rollback_restores_previous_values_flags_changes_and_allows_rerun() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
         VALUES ('rollback-run','m','default','taxonomy-v9','cancelled',2,'now')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ai_run_batches(
            id,run_id,provider,model,status,contact_count,dispatched_at,completed_at,
            system_prompt,user_prompt,raw_response
         ) VALUES(
            'rollback-batch','rollback-run','opencode','m','completed',1,'now','now',
            'system policy','source records','raw response'
         )",
        [],
    )
    .unwrap();

    db.execute(
        "UPDATE contacts SET category='Istruzione', subcategory='Primaria', classification_hash='h1', comune='Albenga' WHERE id='contact-1'",
        [],
    )
    .unwrap();
    let contact = load_test_contact(&db);
    let scuole = vec!["[S1 · Name] scuola".to_owned()];
    let albenga = vec!["[S1 · Sorgente] albenga".to_owned()];
    record_ai_field_change(
        &db,
        "rollback-run",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "category",
            old_value: Some("Varie"),
            new_value: Some("Istruzione"),
            confidence: 0.9,
            evidence: &scuole,
            pending_review: false,
        },
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "rollback-run",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "subcategory",
            old_value: None,
            new_value: Some("Primaria"),
            confidence: 0.9,
            evidence: &scuole,
            pending_review: false,
        },
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "rollback-run",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "comune",
            old_value: None,
            new_value: Some("Albenga"),
            confidence: 0.0,
            evidence: &albenga,
            pending_review: false,
        },
    )
    .unwrap();

    let report = crate::ai_runs::rollback_ai_run_conn(&mut db, "rollback-run").unwrap();
    assert_eq!(report.reverted, 3);
    assert_eq!(report.contacts_reverted, 1);
    assert_eq!(report.runs_rolled_back, 1);

    let (cat, sub, comune, hash): (String, Option<String>, Option<String>, Option<String>) = db
        .query_row(
            "SELECT category,subcategory,comune,classification_hash FROM contacts WHERE id='contact-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(cat, "Varie");
    assert_eq!(sub, None);
    assert_eq!(comune, None);
    assert_eq!(hash, None, "rolled-back contacts must be re-eligible");

    let rolled_back: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM ai_field_changes WHERE run_id='rollback-run' AND rolled_back=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rolled_back, 3);
    let retained_log: (String, String, String) = db
        .query_row(
            "SELECT system_prompt,user_prompt,raw_response FROM ai_run_batches
             WHERE id='rollback-batch'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        retained_log,
        (
            "system policy".to_owned(),
            "source records".to_owned(),
            "raw response".to_owned()
        )
    );

    let again = crate::ai_runs::rollback_ai_run_conn(&mut db, "rollback-run").unwrap();
    assert_eq!(again.reverted, 0, "second rollback must be a no-op");
}

#[test]
fn rollback_stack_unwinds_one_run_at_a_time_in_reverse_order() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    for (run_id, started) in [("run-one", "2026-01-01"), ("run-two", "2026-01-02")] {
        db.execute(
            "INSERT INTO ai_classification_runs(
                id,model,profile,prompt_version,status,total,started_at
             ) VALUES(?1,'m','default','taxonomy-v1','completed',1,?2)",
            params![run_id, started],
        )
        .unwrap();
    }
    let contact = load_test_contact(&db);
    db.execute(
        "UPDATE contacts SET category='Istruzione' WHERE id=?1",
        params![contact.id],
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "run-one",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "category",
            old_value: Some("Varie"),
            new_value: Some("Istruzione"),
            confidence: 1.0,
            evidence: &[],
            pending_review: false,
        },
    )
    .unwrap();
    db.execute(
        "UPDATE contacts SET category='Cultura' WHERE id=?1",
        params![contact.id],
    )
    .unwrap();
    record_ai_field_change(
        &db,
        "run-two",
        &contact,
        FieldChange {
            review_id: None,
            field_name: "category",
            old_value: Some("Istruzione"),
            new_value: Some("Cultura"),
            confidence: 1.0,
            evidence: &[],
            pending_review: false,
        },
    )
    .unwrap();

    let first = crate::ai_runs::rollback_ai_scope_conn(&mut db, "latest").unwrap();
    assert_eq!(first.runs_rolled_back, 1);
    assert_eq!(
        db.query_row(
            "SELECT category FROM contacts WHERE id='contact-1'",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "Istruzione"
    );
    let second = crate::ai_runs::rollback_ai_scope_conn(&mut db, "latest").unwrap();
    assert_eq!(second.runs_rolled_back, 1);
    assert_eq!(
        db.query_row(
            "SELECT category FROM contacts WHERE id='contact-1'",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .unwrap(),
        "Varie"
    );
}

#[test]
fn full_candidates_include_every_exportable_institutional_contact() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    let mut personal = test_contact();
    personal.id = "contact-personal".to_owned();
    personal.contact_kind = "personale".to_owned();
    personal.email = "personal@example.test".to_owned();
    personal.email_local_part = "personal".to_owned();
    personal.email_domain = "example.test".to_owned();
    insert_contact_record(&db, &personal).unwrap();
    let mut suppressed = test_contact();
    suppressed.id = "contact-suppressed".to_owned();
    suppressed.email = "noreply@example.test".to_owned();
    suppressed.email_local_part = "noreply".to_owned();
    insert_contact_record(&db, &suppressed).unwrap();
    let ids = load_ai_candidate_ids(&db, "full").unwrap();
    assert!(ids.contains(&"contact-1".to_owned()));
    assert!(!ids.contains(&"contact-personal".to_owned()));
    assert!(
        !ids.contains(&"contact-suppressed".to_owned()),
        "non-exportable contacts are never sent to paid AI"
    );
}

#[test]
fn audit_repair_migration_clears_alias_geography_and_restores_jev_agreements() {
    let mut db = Connection::open_in_memory().expect("in-memory db");
    migrate_database(&mut db, None, false).expect("initial migration");
    let insert = |id: &str, email: &str, geo: [Option<&str>; 3]| {
        let mut contact = test_contact();
        contact.id = id.to_owned();
        contact.email = email.to_owned();
        insert_contact_record(&db, &contact).expect("contact");
        db.execute(
            "UPDATE contacts SET comune=?1,provincia=?2,regione=?3 WHERE id=?4",
            params![geo[0], geo[1], geo[2], id],
        )
        .expect("geography");
    };
    let como = [Some("Como"), Some("Como"), Some("Lombardia")];
    // `.com` read as Como: cleared at every level.
    insert("alias-com", "utente@example.com", como);
    // `virgilio.it` read as Borgo Virgilio (Mantova): cleared.
    insert(
        "alias-virgilio",
        "utente@virgilio.it",
        [Some("Borgo Virgilio"), Some("Mantova"), Some("Lombardia")],
    );
    // The contact's own municipal domain: kept.
    insert("own-domain", "urp@comune.como.it", como);
    // A source row names the town: kept.
    insert("source-row", "altro@example.com", como);
    db.execute(
        "INSERT INTO contact_sources(id,contact_id,source_file,raw_context)
         VALUES('src-1','source-row','Comuni.csv','{\"Comune\":\"Como\"}')",
        [],
    )
    .expect("source row");
    // No free-text path to the value (manual or imported): kept.
    insert(
        "unrelated",
        "utente@example.test",
        [Some("Lecce"), Some("Lecce"), Some("Puglia")],
    );

    // Jev agreed with the existing category at 0.72 but stayed under the bar.
    db.execute(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
         VALUES('run-audit','cascade/jev','default','cascade-jev-v1','completed',2,'2026-09-27T00:00:00Z')",
        [],
    )
    .expect("run");
    for (id, category, confidence) in [("agree", "Istruzione", 0.72), ("weak", "Istruzione", 0.45)]
    {
        insert(id, &format!("{id}@example.test"), [None, None, None]);
        db.execute(
            "UPDATE contacts SET category=?1,classification_state='insufficient_evidence',
                classification_state_reason='Evidenza insufficiente secondo Jev' WHERE id=?2",
            params![category, id],
        )
        .expect("insufficient state");
        db.execute(
            "INSERT INTO ai_run_artifacts(id,run_id,contact_id,input_hash,raw_response,created_at)
             VALUES(?1,'run-audit',?1,'hash',?2,'2026-09-27T00:00:00Z')",
            params![
                id,
                serde_json::json!({"category": "istruzione", "category_confidence": confidence})
                    .to_string()
            ],
        )
        .expect("artifact");
    }
    // Databases that applied migration 40 before the table existed.
    db.execute_batch("DROP TABLE taxonomy_change_candidate_contacts")
        .expect("simulate missing table");

    // Migration 57 is the audit repair; later migrations re-run after it.
    db.pragma_update(None, "user_version", 56)
        .expect("previous envelope");
    db.execute("DELETE FROM schema_migrations WHERE version>=57", [])
        .expect("remove marker");
    migrate_database(&mut db, None, false).expect("upgrade to audit repair");
    // Idempotent: a second pass finds nothing left to change.
    let logged_once: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM classification_system_changes",
            [],
            |row| row.get(0),
        )
        .unwrap();
    crate::schema::migrate_classification_audit_repairs(&db).expect("second pass");
    let logged_twice: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM classification_system_changes",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(logged_once, logged_twice);

    let geo = |id: &str| -> (Option<String>, Option<String>, Option<String>) {
        db.query_row(
            "SELECT comune,provincia,regione FROM contacts WHERE id=?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(geo("alias-com"), (None, None, None));
    assert_eq!(geo("alias-virgilio"), (None, None, None));
    assert_eq!(geo("own-domain").0.as_deref(), Some("Como"));
    assert_eq!(geo("source-row").1.as_deref(), Some("Como"));
    assert_eq!(geo("unrelated").0.as_deref(), Some("Lecce"));
    let repairs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM classification_system_changes
             WHERE operation='geography_alias_repair' AND contact_id='alias-com'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(repairs, 3, "every cleared value is logged");

    let state = |id: &str| -> String {
        db.query_row(
            "SELECT classification_state FROM contacts WHERE id=?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(state("agree"), "classified");
    assert_eq!(state("weak"), "insufficient_evidence");
    assert!(has_table(&db, "taxonomy_change_candidate_contacts").unwrap());
    let catalog: String = db
        .query_row(
            "SELECT semantic_version FROM deterministic_rule_catalogs WHERE id='exact-it-v1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(catalog, "1.4.0");
}

#[test]
fn geographic_apply_changes_country_only_to_a_supported_value() {
    let (db, contact) = setup_geographic_apply_test();
    let mut result = geographic_proposal("Torino");
    result.country = Some("Francia".to_owned());
    apply_geography_result(&db, &contact, &result, "geo-run").unwrap();
    let paese = |db: &Connection| -> String {
        db.query_row(
            "SELECT paese FROM contacts WHERE id=?1",
            params![contact.id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(paese(&db), contact.paese);

    result.country = Some("Svizzera".to_owned());
    assert_eq!(
        apply_geography_result(&db, &contact, &result, "geo-run").unwrap(),
        "updated"
    );
    assert_eq!(paese(&db), "Svizzera");
    let logged: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM ai_field_changes WHERE field_name='paese'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(logged, 1);
}
