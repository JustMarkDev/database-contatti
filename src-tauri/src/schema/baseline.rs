//! The original baseline schema (migration 1).

use super::*;

pub(super) fn install_baseline_schema(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            applied_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS blacklist (
            normalized_email TEXT PRIMARY KEY COLLATE NOCASE,
            source_file TEXT NOT NULL,
            imported_at TEXT NOT NULL,
            note TEXT
        );
        CREATE TABLE IF NOT EXISTS reviewed_benchmark_cases (
            case_id TEXT PRIMARY KEY,
            email TEXT NOT NULL UNIQUE,
            evidence TEXT NOT NULL,
            category TEXT NOT NULL,
            subcategory TEXT,
            contact_kind TEXT NOT NULL,
            country TEXT NOT NULL,
            geography TEXT,
            boundary TEXT NOT NULL DEFAULT 'taxonomy',
            reviewed_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_benchmark_taxonomy ON reviewed_benchmark_cases(category, subcategory);
        CREATE TABLE IF NOT EXISTS blacklist_legacy_migrations (
            legacy_file TEXT PRIMARY KEY,
            imported_at TEXT NOT NULL,
            row_count INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS contacts (
            id TEXT PRIMARY KEY,
            email TEXT NOT NULL UNIQUE,
            email_local_part TEXT NOT NULL,
            email_domain TEXT NOT NULL,
            email_tld TEXT NOT NULL,
            status TEXT NOT NULL,
            validation_notes TEXT NOT NULL,
            source_file TEXT NOT NULL,
            category TEXT NOT NULL,
            subcategory TEXT,
            contact_kind TEXT NOT NULL DEFAULT 'non_determinato',
            paese TEXT NOT NULL DEFAULT 'Italia',
            email_kind TEXT,
            comune TEXT,
            provincia TEXT,
            regione TEXT,
            macroarea TEXT,
            website TEXT,
            source_url TEXT,
            category_id INTEGER,
            subcategory_id INTEGER,
            classification_method TEXT NOT NULL DEFAULT 'legacy',
            classification_confidence REAL NOT NULL DEFAULT 0,
            classification_evidence TEXT NOT NULL DEFAULT '[]',
            needs_review INTEGER NOT NULL DEFAULT 0,
            classification_locked INTEGER NOT NULL DEFAULT 0,
            category_locked INTEGER NOT NULL DEFAULT 0,
            subcategory_locked INTEGER NOT NULL DEFAULT 0,
            comune_locked INTEGER NOT NULL DEFAULT 0,
            provincia_locked INTEGER NOT NULL DEFAULT 0,
            regione_locked INTEGER NOT NULL DEFAULT 0,
            classification_hash TEXT
        );
        CREATE TABLE IF NOT EXISTS categories (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            sort_order INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS subcategories (
            id INTEGER PRIMARY KEY,
            category_id INTEGER NOT NULL REFERENCES categories(id),
            name TEXT NOT NULL,
            normalized_name TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            UNIQUE(category_id, normalized_name)
        );
        CREATE TABLE IF NOT EXISTS imports (
            id TEXT PRIMARY KEY,
            file_name TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            file_hash TEXT NOT NULL,
            status TEXT NOT NULL,
            parsed_rows INTEGER NOT NULL DEFAULT 0,
            contacts_found INTEGER NOT NULL DEFAULT 0,
            imported_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS contact_sources (
            id TEXT PRIMARY KEY,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            import_id TEXT REFERENCES imports(id) ON DELETE SET NULL,
            source_file TEXT NOT NULL,
            source_sheet TEXT,
            source_row INTEGER,
            raw_context TEXT NOT NULL DEFAULT '{}',
            source_url TEXT,
            UNIQUE(contact_id, source_file, source_sheet, source_row)
        );
        CREATE TABLE IF NOT EXISTS classification_reviews (
            id TEXT PRIMARY KEY,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            current_category TEXT NOT NULL,
            current_subcategory TEXT,
            proposed_category TEXT NOT NULL,
            proposed_subcategory TEXT,
            confidence REAL NOT NULL,
            evidence TEXT NOT NULL,
            reason_code TEXT,
            model TEXT NOT NULL,
            profile TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL,
            resolved_at TEXT
        );
        CREATE TABLE IF NOT EXISTS ai_classification_runs (
            id TEXT PRIMARY KEY,
            model TEXT NOT NULL,
            profile TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            status TEXT NOT NULL,
            total INTEGER NOT NULL,
            processed INTEGER NOT NULL DEFAULT 0,
            updated INTEGER NOT NULL DEFAULT 0,
            reviews_created INTEGER NOT NULL DEFAULT 0,
            skipped INTEGER NOT NULL DEFAULT 0,
            errors INTEGER NOT NULL DEFAULT 0,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            request_json TEXT NOT NULL DEFAULT '{}',
            interruption_reason TEXT,
            owner_session TEXT,
            lease_expires_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS ai_run_artifacts (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
            contact_id TEXT REFERENCES contacts(id) ON DELETE SET NULL,
            input_hash TEXT NOT NULL,
            system_prompt TEXT NOT NULL DEFAULT '',
            user_prompt TEXT NOT NULL DEFAULT '',
            raw_response TEXT,
            retry_response TEXT,
            validated_result TEXT,
            operational_log TEXT,
            validation_errors TEXT NOT NULL DEFAULT '[]',
            runtime_metadata TEXT NOT NULL DEFAULT '{}',
            catalog_version TEXT NOT NULL DEFAULT '',
            model_identity TEXT NOT NULL DEFAULT '',
            model_digest TEXT,
            settings_json TEXT NOT NULL DEFAULT '{}',
            mode TEXT NOT NULL DEFAULT 'taxonomy',
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ai_artifacts_run ON ai_run_artifacts(run_id);
        CREATE TABLE IF NOT EXISTS ai_applied_provenance (
            id TEXT PRIMARY KEY,
            run_id TEXT,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            field_name TEXT NOT NULL,
            value TEXT,
            evidence TEXT NOT NULL,
            model TEXT NOT NULL,
            prompt_version TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS ai_run_candidates (
            run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','processed','skipped','error')),
            error TEXT,
            processed_at TEXT,
            PRIMARY KEY(run_id, contact_id),
            UNIQUE(run_id, ordinal)
        );
        CREATE TABLE IF NOT EXISTS ai_field_changes (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            review_id TEXT REFERENCES classification_reviews(id) ON DELETE SET NULL,
            field_name TEXT NOT NULL,
            old_value TEXT,
            new_value TEXT,
            change_kind TEXT NOT NULL CHECK(change_kind IN ('aggiunto','modificato','in_revisione')),
            status TEXT NOT NULL CHECK(status IN ('applied','pending_review','rejected')),
            confidence REAL NOT NULL,
            evidence TEXT NOT NULL,
            created_at TEXT NOT NULL,
            resolved_at TEXT
        );
        CREATE TABLE IF NOT EXISTS ai_run_retries (
            run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            validation_errors TEXT NOT NULL,
            raw_response TEXT,
            created_at TEXT NOT NULL,
            PRIMARY KEY(run_id, contact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_ai_run_retries_run ON ai_run_retries(run_id);
        CREATE INDEX IF NOT EXISTS idx_contacts_email ON contacts(email);
        CREATE INDEX IF NOT EXISTS idx_contacts_status ON contacts(status);
        CREATE INDEX IF NOT EXISTS idx_contacts_category ON contacts(category, subcategory);
        CREATE INDEX IF NOT EXISTS idx_contacts_geo ON contacts(regione, provincia, comune);
        CREATE INDEX IF NOT EXISTS idx_reviews_status ON classification_reviews(status);
        CREATE INDEX IF NOT EXISTS idx_sources_contact ON contact_sources(contact_id);",
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}
