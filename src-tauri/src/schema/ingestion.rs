//! Lossless ingestion, DNS, mailbox verification, and gazetteer migrations.

use super::*;

pub(crate) fn migrate_compact_unrecoverable_candidates(db: &Connection) -> Result<(), String> {
    if !has_table(db, "email_candidates")? {
        return Ok(());
    }
    db.execute_batch(
        "DELETE FROM candidate_dispositions WHERE candidate_id IN (
            SELECT id FROM email_candidates
            WHERE current_disposition IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
               OR disposition_reason = 'non_contact_mailbox'
               OR status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
               OR candidate_status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
        );
        DELETE FROM classification_claims WHERE candidate_id IN (
            SELECT id FROM email_candidates
            WHERE current_disposition IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
               OR disposition_reason = 'non_contact_mailbox'
               OR status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
               OR candidate_status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
        );
        DELETE FROM verification_events WHERE candidate_id IN (
            SELECT id FROM email_candidates
            WHERE current_disposition IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
               OR disposition_reason = 'non_contact_mailbox'
               OR status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
               OR candidate_status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
        );
        DELETE FROM email_candidates
        WHERE current_disposition IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
           OR disposition_reason = 'non_contact_mailbox'
           OR status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed')
           OR candidate_status IN ('invalid_syntax', 'no_routing', 'blacklisted', 'suppressed');",
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn migrate_consent_metadata_columns(db: &Connection) -> Result<(), String> {
    for table in ["email_candidates", "contacts"] {
        let columns = table_columns(db, table)?;
        for column in ["permitted_purpose", "lawful_basis", "consent_provenance"] {
            if !columns.iter().any(|existing| existing == column) {
                db.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"), [])
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

pub(super) fn ingestion_v15_schema_rebuild_needed(db: &Connection) -> Result<bool, String> {
    if !has_table(db, "source_rows")? || !has_table(db, "email_candidates")? {
        return Ok(false);
    }
    let source_rows_sql = compact_schema_sql(&schema_object_sql(db, "table", "source_rows")?);
    let candidates_sql = compact_schema_sql(&schema_object_sql(db, "table", "email_candidates")?);
    let occurrence_index_sql = compact_schema_sql(&schema_object_sql(
        db,
        "index",
        "idx_candidates_occurrence",
    )?);
    let candidate_columns = table_columns(db, "email_candidates")?;
    Ok(
        !source_rows_sql.contains("'parsed','header','empty','malformed','failed'")
            || !candidate_columns
                .iter()
                .any(|column| column == "outreach_eligible")
            || !candidate_columns
                .iter()
                .any(|column| column == "occurrence_index")
            || !candidates_sql
                .contains("unique(source_row_id,raw_email,extraction_method,occurrence_index)")
            || !occurrence_index_sql.contains("occurrence_index"),
    )
}

pub(super) fn migrate_ingestion_v15_schema_repair(db: &Connection) -> Result<(), String> {
    if !ingestion_v15_schema_rebuild_needed(db)? {
        return Ok(());
    }
    migrate_consent_metadata_columns(db)?;
    let candidate_columns = table_columns(db, "email_candidates")?;
    for (column, definition) in [
        (
            "outreach_eligible",
            "INTEGER NOT NULL DEFAULT 0 CHECK(outreach_eligible IN (0,1))",
        ),
        ("occurrence_index", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !candidate_columns.iter().any(|existing| existing == column) {
            db.execute(
                &format!("ALTER TABLE email_candidates ADD COLUMN {column} {definition}"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }

    db.execute_batch(
        "ALTER TABLE source_rows RENAME TO source_rows_v19;
         CREATE TABLE source_rows (
            id TEXT PRIMARY KEY,
            import_id TEXT NOT NULL REFERENCES imports(id) ON DELETE CASCADE,
            source_sheet TEXT NOT NULL DEFAULT '',
            row_number INTEGER NOT NULL CHECK(row_number > 0),
            raw_context TEXT NOT NULL DEFAULT '{}',
            row_hash TEXT,
            parse_status TEXT NOT NULL DEFAULT 'parsed'
                CHECK(parse_status IN ('parsed','header','empty','malformed','failed')),
            error_code TEXT,
            error_message TEXT,
            candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
            created_at TEXT NOT NULL,
            UNIQUE(import_id, source_sheet, row_number)
         );
         INSERT INTO source_rows(
            id,import_id,source_sheet,row_number,raw_context,row_hash,parse_status,
            error_code,error_message,candidate_count,created_at
         )
         SELECT id,import_id,source_sheet,row_number,raw_context,row_hash,parse_status,
                error_code,error_message,candidate_count,created_at
         FROM source_rows_v19;
         DROP TABLE source_rows_v19;
         CREATE INDEX idx_source_rows_import ON source_rows(import_id);
         CREATE INDEX idx_source_rows_status ON source_rows(parse_status);

         ALTER TABLE email_candidates RENAME TO email_candidates_v19;
         CREATE TABLE email_candidates (
            id TEXT PRIMARY KEY,
            source_row_id TEXT REFERENCES source_rows(id) ON DELETE CASCADE,
            raw_email TEXT NOT NULL,
            canonical_email TEXT,
            comparison_key TEXT,
            normalization_method TEXT NOT NULL DEFAULT 'none',
            extraction_method TEXT,
            contact_id TEXT REFERENCES contacts(id) ON DELETE SET NULL,
            status TEXT NOT NULL DEFAULT 'unknown'
                CHECK(status IN ('unknown','linked','quarantined','suppressed','blacklisted','invalid_syntax','no_routing','temporary_dns_failure')),
            syntax_status TEXT NOT NULL DEFAULT 'unknown',
            routing_status TEXT NOT NULL DEFAULT 'unknown',
            mailbox_status TEXT NOT NULL DEFAULT 'unknown',
            candidate_status TEXT NOT NULL DEFAULT 'unknown'
                CHECK(candidate_status IN ('unknown','linked','quarantined','suppressed','blacklisted','invalid_syntax','no_routing','temporary_dns_failure')),
            current_disposition TEXT NOT NULL DEFAULT 'unknown'
                CHECK(current_disposition IN ('unknown','linked','quarantined','suppressed','blacklisted','invalid_syntax','no_routing','temporary_dns_failure')),
            disposition_reason TEXT,
            category TEXT,
            subcategory TEXT,
            source_url TEXT,
            organization_name TEXT,
            denomination TEXT,
            person_name TEXT,
            role TEXT,
            department TEXT,
            address TEXT,
            postal_code TEXT,
            official_code TEXT,
            permitted_purpose TEXT,
            lawful_basis TEXT,
            consent_provenance TEXT,
            outreach_eligible INTEGER NOT NULL DEFAULT 0 CHECK(outreach_eligible IN (0,1)),
            quality_score REAL CHECK(quality_score IS NULL OR (quality_score >= 0 AND quality_score <= 1)),
            score_factors_json TEXT NOT NULL DEFAULT '{}',
            occurrence_index INTEGER NOT NULL DEFAULT 0 CHECK(occurrence_index >= 0),
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            UNIQUE(source_row_id, raw_email, extraction_method, occurrence_index)
         );
         INSERT INTO email_candidates(
            id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
            extraction_method,contact_id,status,syntax_status,routing_status,mailbox_status,
            candidate_status,current_disposition,disposition_reason,category,subcategory,
            source_url,organization_name,denomination,person_name,role,department,address,
            postal_code,official_code,permitted_purpose,lawful_basis,consent_provenance,
            outreach_eligible,quality_score,score_factors_json,occurrence_index,
            first_seen_at,last_seen_at
         )
         SELECT id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
                extraction_method,contact_id,status,syntax_status,routing_status,mailbox_status,
                candidate_status,current_disposition,disposition_reason,category,subcategory,
                source_url,organization_name,denomination,person_name,role,department,address,
                postal_code,official_code,permitted_purpose,lawful_basis,consent_provenance,
                outreach_eligible,quality_score,score_factors_json,occurrence_index,
                first_seen_at,last_seen_at
         FROM email_candidates_v19;
         DROP TABLE email_candidates_v19;
         CREATE INDEX idx_candidates_comparison ON email_candidates(comparison_key);
         CREATE INDEX idx_candidates_contact ON email_candidates(contact_id);
         CREATE INDEX idx_candidates_status ON email_candidates(candidate_status);
         CREATE INDEX idx_candidates_disposition ON email_candidates(current_disposition);
         CREATE INDEX idx_candidates_category ON email_candidates(category,subcategory);
         CREATE INDEX idx_candidates_status_detail
            ON email_candidates(status,syntax_status,routing_status,mailbox_status);
         CREATE UNIQUE INDEX idx_candidates_occurrence
            ON email_candidates(
                source_row_id,
                ifnull(comparison_key,lower(trim(raw_email))),
                ifnull(extraction_method,'literal'),
                occurrence_index
            ) WHERE source_row_id IS NOT NULL;",
    )
    .map_err(|error| error.to_string())?;

    let foreign_key_errors: i64 = db
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|error| error.to_string())?;
    if foreign_key_errors != 0 {
        return Err(format!(
            "ingestion schema repair left {foreign_key_errors} foreign key violations"
        ));
    }
    Ok(())
}

pub(super) fn migrate_contact_source_occurrences(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "ALTER TABLE contact_sources RENAME TO contact_sources_v15;
         CREATE TABLE contact_sources (
            id TEXT PRIMARY KEY,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            import_id TEXT REFERENCES imports(id) ON DELETE SET NULL,
            source_file TEXT NOT NULL,
            source_sheet TEXT,
            source_row INTEGER,
            raw_context TEXT NOT NULL DEFAULT '{}',
            source_url TEXT,
            source_row_id TEXT REFERENCES source_rows(id) ON DELETE SET NULL,
            UNIQUE(contact_id, import_id, source_file, source_sheet, source_row)
         );
         INSERT INTO contact_sources(
            id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,
            source_url,source_row_id
         )
         SELECT id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,
                source_url,source_row_id
         FROM contact_sources_v15;
         DROP TABLE contact_sources_v15;
         CREATE INDEX idx_sources_contact ON contact_sources(contact_id);
         CREATE INDEX idx_contact_sources_source_row ON contact_sources(source_row_id);
         CREATE INDEX idx_contact_sources_import ON contact_sources(import_id);",
    )
    .map_err(|error| error.to_string())
}

pub(super) fn migrate_dns_ttl_evidence(db: &Connection) -> Result<(), String> {
    for table in ["domain_verification_cache", "verification_events"] {
        let extra_columns: &[(&str, &str)] = if table == "verification_events" {
            &[
                ("observed_ttl_seconds", "INTEGER"),
                ("effective_cache_ttl_seconds", "INTEGER"),
                ("dns_checked_at", "TEXT"),
                ("dns_expires_at", "TEXT"),
            ]
        } else {
            &[
                ("observed_ttl_seconds", "INTEGER"),
                ("effective_cache_ttl_seconds", "INTEGER"),
            ]
        };
        for (column, definition) in extra_columns {
            if !table_columns(db, table)?
                .iter()
                .any(|existing| existing == column)
            {
                db.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                    [],
                )
                .map_err(|error| error.to_string())?;
            }
        }
        db.execute(
            &format!(
                "UPDATE {table}
                 SET effective_cache_ttl_seconds=coalesce(effective_cache_ttl_seconds,ttl_seconds)
                 WHERE effective_cache_ttl_seconds IS NULL"
            ),
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn migrate_mailbox_verification_schema(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS mailbox_verification_settings (
            id INTEGER PRIMARY KEY CHECK(id=1),
            ehlo_hostname TEXT,
            envelope_sender TEXT,
            starttls_required INTEGER NOT NULL DEFAULT 1 CHECK(starttls_required IN (0,1)),
            global_rate_per_minute INTEGER NOT NULL DEFAULT 5 CHECK(global_rate_per_minute BETWEEN 1 AND 60),
            max_concurrency INTEGER NOT NULL DEFAULT 1 CHECK(max_concurrency BETWEEN 1 AND 2),
            updated_at TEXT NOT NULL
        );
        INSERT OR IGNORE INTO mailbox_verification_settings
            (id,starttls_required,global_rate_per_minute,max_concurrency,updated_at)
            VALUES (1,1,5,1,CURRENT_TIMESTAMP);

        CREATE TABLE IF NOT EXISTS mailbox_verification_jobs (
            id TEXT PRIMARY KEY,
            mode TEXT NOT NULL CHECK(mode IN ('recommended','everything')),
            status TEXT NOT NULL CHECK(status IN ('preparing','running','paused','completed','cancelled','failed')),
            stage TEXT NOT NULL DEFAULT 'dns_prepare',
            total_queued INTEGER NOT NULL DEFAULT 0,
            completed INTEGER NOT NULL DEFAULT 0,
            accepted INTEGER NOT NULL DEFAULT 0,
            rejected INTEGER NOT NULL DEFAULT 0,
            temporary INTEGER NOT NULL DEFAULT 0,
            unknown INTEGER NOT NULL DEFAULT 0,
            catch_all INTEGER NOT NULL DEFAULT 0,
            policy_blocked INTEGER NOT NULL DEFAULT 0,
            last_error TEXT,
            safety_gate TEXT,
            created_at TEXT NOT NULL,
            started_at TEXT,
            finished_at TEXT,
            updated_at TEXT NOT NULL,
            settings_json TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS idx_mailbox_jobs_status ON mailbox_verification_jobs(status,updated_at);

        CREATE TABLE IF NOT EXISTS mailbox_verification_tasks (
            id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL REFERENCES mailbox_verification_jobs(id) ON DELETE CASCADE,
            contact_id TEXT REFERENCES contacts(id) ON DELETE SET NULL,
            candidate_id TEXT REFERENCES email_candidates(id) ON DELETE SET NULL,
            comparison_key TEXT NOT NULL,
            email TEXT NOT NULL,
            domain TEXT NOT NULL,
            mx_hosts_json TEXT NOT NULL DEFAULT '[]',
            priority REAL NOT NULL DEFAULT 0,
            status TEXT NOT NULL CHECK(status IN ('queued','leased','accepted','rejected_permanent','temporary','catch_all','policy_blocked','connection_failed','not_attempted','unknown')),
            attempts INTEGER NOT NULL DEFAULT 0,
            next_attempt_at TEXT,
            leased_until TEXT,
            outcome TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(job_id,comparison_key)
        );
        CREATE INDEX IF NOT EXISTS idx_mailbox_tasks_ready ON mailbox_verification_tasks(job_id,status,next_attempt_at,priority);
        CREATE INDEX IF NOT EXISTS idx_mailbox_tasks_domain ON mailbox_verification_tasks(domain,status);

        CREATE TABLE IF NOT EXISTS mailbox_verification_attempts (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL REFERENCES mailbox_verification_tasks(id) ON DELETE CASCADE,
            job_id TEXT NOT NULL REFERENCES mailbox_verification_jobs(id) ON DELETE CASCADE,
            mx_host TEXT,
            port INTEGER NOT NULL DEFAULT 25,
            transport TEXT NOT NULL DEFAULT 'starttls',
            smtp_code INTEGER,
            enhanced_code TEXT,
            outcome TEXT NOT NULL,
            error_class TEXT,
            response_excerpt TEXT,
            duration_ms INTEGER,
            attempted_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_mailbox_attempts_task ON mailbox_verification_attempts(task_id,attempted_at);

        CREATE TABLE IF NOT EXISTS mailbox_verification_target_state (
            target_key TEXT PRIMARY KEY,
            target_kind TEXT NOT NULL CHECK(target_kind IN ('domain','mx')),
            cooldown_until TEXT,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            last_outcome TEXT,
            last_attempt_at TEXT,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_mailbox_target_cooldown ON mailbox_verification_target_state(cooldown_until);

        CREATE TABLE IF NOT EXISTS mailbox_verification_control (
            id INTEGER PRIMARY KEY CHECK(id=1),
            action TEXT NOT NULL CHECK(action IN ('running','paused','cancelled')),
            updated_at TEXT NOT NULL
        );
        INSERT OR IGNORE INTO mailbox_verification_control(id,action,updated_at)
            VALUES (1,'paused',CURRENT_TIMESTAMP);",
    )
    .map_err(|error| error.to_string())?;

    db.execute(
        "UPDATE mailbox_verification_settings SET starttls_required=1,max_concurrency=1 WHERE id=1",
        [],
    )
    .map_err(|error| error.to_string())?;

    db.execute_batch(
        "UPDATE candidate_dispositions
         SET is_current=0
         WHERE is_current=1
           AND rowid NOT IN (
             SELECT max(rowid) FROM candidate_dispositions
             WHERE is_current=1 GROUP BY candidate_id
           );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_candidate_dispositions_one_current
           ON candidate_dispositions(candidate_id) WHERE is_current=1;",
    )
    .map_err(|error| error.to_string())?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut stmt = db
        .prepare("SELECT id,COALESCE(email_comparison_key,lower(email)),email FROM contacts")
        .map_err(|error| error.to_string())?;
    let contacts = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(stmt);
    for (contact_id, key, email) in contacts {
        if crate::parsing::non_contact_mailbox_reason(&email).is_none() {
            continue;
        }
        db.execute("INSERT OR IGNORE INTO suppressions(comparison_key,canonical_email,reason,source_authority,created_at,note) VALUES (?1,?2,'non_contact_mailbox','migration',?3,'Automatic durable suppression for no-reply/bounce mailbox')", params![key,email,now]).map_err(|error| error.to_string())?;
        let mut candidates = db
            .prepare("SELECT id FROM email_candidates WHERE comparison_key=?1")
            .map_err(|error| error.to_string())?;
        let ids = candidates
            .query_map(params![key], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        drop(candidates);
        for candidate_id in ids {
            db.execute("UPDATE email_candidates SET contact_id=NULL,status='suppressed',candidate_status='suppressed',current_disposition='suppressed',disposition_reason='non_contact_mailbox',outreach_eligible=0 WHERE id=?1", params![candidate_id]).map_err(|error| error.to_string())?;
            db.execute("UPDATE candidate_dispositions SET is_current=0 WHERE candidate_id=?1 AND is_current=1", params![candidate_id]).map_err(|error| error.to_string())?;
            db.execute("INSERT INTO candidate_dispositions(id,candidate_id,disposition,reason_code,created_at) VALUES (?1,?2,'suppressed','non_contact_mailbox',?3)", params![uuid::Uuid::new_v4().to_string(), candidate_id, now]).map_err(|error| error.to_string())?;
        }
        db.execute("DELETE FROM contacts WHERE id=?1", params![contact_id])
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Routable DNS results live once per domain in `domain_verification_cache`;
/// the per-candidate copies only repeated them.
pub(crate) fn migrate_dns_cache_per_domain(db: &Connection) -> Result<(), String> {
    db.execute(
        "DELETE FROM verification_events WHERE method='dns_cache'",
        [],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_lossless_ingestion_schema(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS import_attempts (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL CHECK(status IN ('planned','running','completed','partial','failed','cancelled')),
            parser_version TEXT NOT NULL,
            manifest_version TEXT NOT NULL DEFAULT 'manifest-v1',
            started_at TEXT NOT NULL,
            completed_at TEXT,
            source_root TEXT,
            dry_run INTEGER NOT NULL DEFAULT 0 CHECK(dry_run IN (0,1)),
            report_json TEXT NOT NULL DEFAULT '{}',
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_import_attempts_status ON import_attempts(status);

        CREATE TABLE IF NOT EXISTS organizations (
            id TEXT PRIMARY KEY,
            name TEXT,
            normalized_name TEXT,
            entity_type TEXT,
            country_code TEXT,
            website TEXT,
            source_url TEXT,
            source_authority TEXT,
            first_seen_at TEXT,
            last_seen_at TEXT,
            metadata_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_organizations_name ON organizations(normalized_name);

        CREATE TABLE IF NOT EXISTS source_rows (
            id TEXT PRIMARY KEY,
            import_id TEXT NOT NULL REFERENCES imports(id) ON DELETE CASCADE,
            source_sheet TEXT NOT NULL DEFAULT '',
            row_number INTEGER NOT NULL CHECK(row_number > 0),
            raw_context TEXT NOT NULL DEFAULT '{}',
            row_hash TEXT,
            parse_status TEXT NOT NULL DEFAULT 'parsed'
                CHECK(parse_status IN ('parsed','header','empty','malformed','failed')),
            error_code TEXT,
            error_message TEXT,
            candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
            created_at TEXT NOT NULL,
            UNIQUE(import_id, source_sheet, row_number)
        );
        CREATE INDEX IF NOT EXISTS idx_source_rows_import ON source_rows(import_id);
        CREATE INDEX IF NOT EXISTS idx_source_rows_status ON source_rows(parse_status);

        CREATE TABLE IF NOT EXISTS email_candidates (
            id TEXT PRIMARY KEY,
            source_row_id TEXT REFERENCES source_rows(id) ON DELETE CASCADE,
            raw_email TEXT NOT NULL,
            canonical_email TEXT,
            comparison_key TEXT,
            normalization_method TEXT NOT NULL DEFAULT 'none',
            extraction_method TEXT,
            contact_id TEXT REFERENCES contacts(id) ON DELETE SET NULL,
            status TEXT NOT NULL DEFAULT 'unknown'
                CHECK(status IN ('unknown','linked','quarantined','suppressed','blacklisted','invalid_syntax','no_routing','temporary_dns_failure')),
            syntax_status TEXT NOT NULL DEFAULT 'unknown',
            routing_status TEXT NOT NULL DEFAULT 'unknown',
            mailbox_status TEXT NOT NULL DEFAULT 'unknown',
            candidate_status TEXT NOT NULL DEFAULT 'unknown'
                CHECK(candidate_status IN ('unknown','linked','quarantined','suppressed','blacklisted','invalid_syntax','no_routing','temporary_dns_failure')),
            current_disposition TEXT NOT NULL DEFAULT 'unknown'
                CHECK(current_disposition IN ('unknown','linked','quarantined','suppressed','blacklisted','invalid_syntax','no_routing','temporary_dns_failure')),
            disposition_reason TEXT,
            category TEXT,
            subcategory TEXT,
            source_url TEXT,
            organization_name TEXT,
            denomination TEXT,
            person_name TEXT,
            role TEXT,
            department TEXT,
            address TEXT,
            postal_code TEXT,
            official_code TEXT,
            permitted_purpose TEXT,
            lawful_basis TEXT,
            consent_provenance TEXT,
            outreach_eligible INTEGER NOT NULL DEFAULT 0 CHECK(outreach_eligible IN (0,1)),
            quality_score REAL CHECK(quality_score IS NULL OR (quality_score >= 0 AND quality_score <= 1)),
            score_factors_json TEXT NOT NULL DEFAULT '{}',
            occurrence_index INTEGER NOT NULL DEFAULT 0 CHECK(occurrence_index >= 0),
            first_seen_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            UNIQUE(source_row_id, raw_email, extraction_method, occurrence_index)
        );
        CREATE INDEX IF NOT EXISTS idx_candidates_comparison ON email_candidates(comparison_key);
        CREATE INDEX IF NOT EXISTS idx_candidates_contact ON email_candidates(contact_id);
        CREATE INDEX IF NOT EXISTS idx_candidates_status ON email_candidates(candidate_status);
        CREATE INDEX IF NOT EXISTS idx_candidates_disposition ON email_candidates(current_disposition);

        CREATE TABLE IF NOT EXISTS candidate_dispositions (
            id TEXT PRIMARY KEY,
            candidate_id TEXT NOT NULL REFERENCES email_candidates(id) ON DELETE CASCADE,
            disposition TEXT NOT NULL
                CHECK(disposition IN ('linked','quarantined','suppressed','blacklisted','invalid_syntax','no_routing','temporary_dns_failure','unknown')),
            reason_code TEXT,
            details_json TEXT NOT NULL DEFAULT '{}',
            is_current INTEGER NOT NULL DEFAULT 1 CHECK(is_current IN (0,1)),
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_candidate_dispositions_current
            ON candidate_dispositions(candidate_id, is_current);
        CREATE INDEX IF NOT EXISTS idx_candidate_dispositions_kind
            ON candidate_dispositions(disposition, is_current);

        CREATE TABLE IF NOT EXISTS suppressions (
            comparison_key TEXT PRIMARY KEY COLLATE NOCASE,
            canonical_email TEXT,
            reason TEXT NOT NULL,
            source_file TEXT,
            source_authority TEXT,
            created_at TEXT NOT NULL,
            lifted_at TEXT,
            note TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_suppressions_active
            ON suppressions(comparison_key) WHERE lifted_at IS NULL;

        CREATE TABLE IF NOT EXISTS verification_events (
            id TEXT PRIMARY KEY,
            comparison_key TEXT NOT NULL,
            candidate_id TEXT REFERENCES email_candidates(id) ON DELETE SET NULL,
            contact_id TEXT REFERENCES contacts(id) ON DELETE SET NULL,
            method TEXT NOT NULL,
            provider TEXT,
            routing_status TEXT,
            dns_rcode TEXT,
            mx_hosts_json TEXT NOT NULL DEFAULT '[]',
            ttl_seconds INTEGER,
            mailbox_status TEXT,
            error_class TEXT,
            details_json TEXT NOT NULL DEFAULT '{}',
            checked_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_verification_key ON verification_events(comparison_key, checked_at);
        CREATE INDEX IF NOT EXISTS idx_verification_contact ON verification_events(contact_id);

        CREATE TABLE IF NOT EXISTS domain_verification_cache (
            domain TEXT PRIMARY KEY COLLATE NOCASE,
            routing_status TEXT NOT NULL,
            dns_rcode TEXT,
            mx_hosts_json TEXT NOT NULL DEFAULT '[]',
            ttl_seconds INTEGER,
            checked_at TEXT NOT NULL,
            expires_at TEXT,
            error_class TEXT,
            details_json TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX IF NOT EXISTS idx_domain_cache_expiry ON domain_verification_cache(expires_at);
        CREATE INDEX IF NOT EXISTS idx_domain_cache_status ON domain_verification_cache(routing_status);

        CREATE TABLE IF NOT EXISTS classification_claims (
            id TEXT PRIMARY KEY,
            candidate_id TEXT REFERENCES email_candidates(id) ON DELETE CASCADE,
            contact_id TEXT REFERENCES contacts(id) ON DELETE CASCADE,
            source_row_id TEXT REFERENCES source_rows(id) ON DELETE CASCADE,
            category TEXT NOT NULL,
            subcategory TEXT,
            confidence REAL NOT NULL DEFAULT 0 CHECK(confidence >= 0 AND confidence <= 1),
            method TEXT NOT NULL DEFAULT 'source',
            source_authority TEXT,
            evidence_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            CHECK(candidate_id IS NOT NULL OR contact_id IS NOT NULL)
        );
        CREATE INDEX IF NOT EXISTS idx_classification_claims_candidate ON classification_claims(candidate_id);
        CREATE INDEX IF NOT EXISTS idx_classification_claims_contact ON classification_claims(contact_id);
        CREATE INDEX IF NOT EXISTS idx_classification_claims_category ON classification_claims(category, subcategory);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_classification_claims_contact_source
            ON classification_claims(contact_id, source_row_id, category, ifnull(subcategory, ''))
            WHERE contact_id IS NOT NULL;

        CREATE TABLE IF NOT EXISTS entity_contact_points (
            organization_id TEXT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            relationship_type TEXT,
            confidence REAL NOT NULL DEFAULT 0 CHECK(confidence >= 0 AND confidence <= 1),
            evidence_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(organization_id, contact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_entity_contact_points_contact ON entity_contact_points(contact_id);

        CREATE TABLE IF NOT EXISTS quality_score_snapshots (
            id TEXT PRIMARY KEY,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            score_version TEXT NOT NULL,
            routing_confidence REAL CHECK(routing_confidence IS NULL OR (routing_confidence >= 0 AND routing_confidence <= 1)),
            mailbox_confidence REAL CHECK(mailbox_confidence IS NULL OR (mailbox_confidence >= 0 AND mailbox_confidence <= 1)),
            source_confidence REAL CHECK(source_confidence IS NULL OR (source_confidence >= 0 AND source_confidence <= 1)),
            freshness_score REAL CHECK(freshness_score IS NULL OR (freshness_score >= 0 AND freshness_score <= 1)),
            entity_match_confidence REAL CHECK(entity_match_confidence IS NULL OR (entity_match_confidence >= 0 AND entity_match_confidence <= 1)),
            usefulness_score REAL CHECK(usefulness_score IS NULL OR (usefulness_score >= 0 AND usefulness_score <= 1)),
            refresh_priority REAL CHECK(refresh_priority IS NULL OR (refresh_priority >= 0 AND refresh_priority <= 1)),
            factors_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_quality_scores_contact ON quality_score_snapshots(contact_id, created_at);

        CREATE TABLE IF NOT EXISTS quality_score_components (
            snapshot_id TEXT NOT NULL REFERENCES quality_score_snapshots(id) ON DELETE CASCADE,
            component TEXT NOT NULL,
            value REAL,
            evidence_json TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY(snapshot_id, component)
        );
        CREATE INDEX IF NOT EXISTS idx_quality_components_component ON quality_score_components(component);

        CREATE INDEX IF NOT EXISTS idx_contacts_suppression_email
            ON contacts(email);",
    )
    .map_err(|e| e.to_string())?;

    for (table, name, definition) in [
        (
            "imports",
            "import_attempt_id",
            "TEXT REFERENCES import_attempts(id) ON DELETE SET NULL",
        ),
        ("imports", "content_sha256", "TEXT"),
        (
            "imports",
            "hash_algorithm",
            "TEXT NOT NULL DEFAULT 'legacy'",
        ),
        ("imports", "file_size_bytes", "INTEGER"),
        ("imports", "parser_version", "TEXT"),
        ("imports", "encoding", "TEXT"),
        ("imports", "delimiter", "TEXT"),
        ("imports", "sheet_count", "INTEGER"),
        ("imports", "started_at", "TEXT"),
        ("imports", "completed_at", "TEXT"),
        ("imports", "error_count", "INTEGER NOT NULL DEFAULT 0"),
        ("imports", "report_json", "TEXT NOT NULL DEFAULT '{}'"),
        (
            "contact_sources",
            "source_row_id",
            "TEXT REFERENCES source_rows(id) ON DELETE SET NULL",
        ),
        (
            "email_candidates",
            "status",
            "TEXT NOT NULL DEFAULT 'unknown'",
        ),
        (
            "email_candidates",
            "syntax_status",
            "TEXT NOT NULL DEFAULT 'unknown'",
        ),
        (
            "email_candidates",
            "routing_status",
            "TEXT NOT NULL DEFAULT 'unknown'",
        ),
        (
            "email_candidates",
            "mailbox_status",
            "TEXT NOT NULL DEFAULT 'unknown'",
        ),
        (
            "email_candidates",
            "current_disposition",
            "TEXT NOT NULL DEFAULT 'unknown'",
        ),
        ("email_candidates", "disposition_reason", "TEXT"),
        ("email_candidates", "category", "TEXT"),
        ("email_candidates", "subcategory", "TEXT"),
        ("email_candidates", "source_url", "TEXT"),
        ("email_candidates", "organization_name", "TEXT"),
        ("email_candidates", "denomination", "TEXT"),
        ("email_candidates", "person_name", "TEXT"),
        ("email_candidates", "role", "TEXT"),
        ("email_candidates", "department", "TEXT"),
        ("email_candidates", "address", "TEXT"),
        ("email_candidates", "postal_code", "TEXT"),
        ("email_candidates", "official_code", "TEXT"),
        (
            "email_candidates",
            "outreach_eligible",
            "INTEGER NOT NULL DEFAULT 0 CHECK(outreach_eligible IN (0,1))",
        ),
        ("email_candidates", "quality_score", "REAL"),
        (
            "email_candidates",
            "occurrence_index",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "email_candidates",
            "score_factors_json",
            "TEXT NOT NULL DEFAULT '{}'",
        ),
        ("contacts", "raw_email", "TEXT"),
        ("contacts", "email_comparison_key", "TEXT"),
        (
            "contacts",
            "normalization_method",
            "TEXT NOT NULL DEFAULT 'legacy'",
        ),
        (
            "contacts",
            "organization_id",
            "TEXT REFERENCES organizations(id) ON DELETE SET NULL",
        ),
        ("contacts", "organization_name", "TEXT"),
        ("contacts", "denomination", "TEXT"),
        ("contacts", "person_name", "TEXT"),
        ("contacts", "role", "TEXT"),
        ("contacts", "department", "TEXT"),
        ("contacts", "address", "TEXT"),
        ("contacts", "postal_code", "TEXT"),
        ("contacts", "official_code", "TEXT"),
        ("contacts", "routing_status", "TEXT"),
        ("contacts", "mailbox_status", "TEXT"),
        (
            "contacts",
            "suppression_status",
            "TEXT NOT NULL DEFAULT 'active'",
        ),
        ("contacts", "first_seen_at", "TEXT"),
        ("contacts", "last_seen_at", "TEXT"),
        ("contacts", "last_confirmed_at", "TEXT"),
        (
            "contacts",
            "source_occurrence_count",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "contacts",
            "independent_source_count",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        ("contacts", "is_role_mailbox", "INTEGER NOT NULL DEFAULT 0"),
        ("contacts", "is_pec", "INTEGER NOT NULL DEFAULT 0"),
        ("contacts", "is_disposable", "INTEGER NOT NULL DEFAULT 0"),
        ("contacts", "is_placeholder", "INTEGER NOT NULL DEFAULT 0"),
        (
            "contacts",
            "is_typo_suspected",
            "INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !table_columns(db, table)?
            .iter()
            .any(|column| column == name)
        {
            db.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {name} {definition}"),
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_imports_sha256 ON imports(content_sha256);
         CREATE INDEX IF NOT EXISTS idx_imports_attempt ON imports(import_attempt_id);
         CREATE INDEX IF NOT EXISTS idx_contact_sources_source_row ON contact_sources(source_row_id);
         CREATE INDEX IF NOT EXISTS idx_contacts_organization ON contacts(organization_id);
         CREATE INDEX IF NOT EXISTS idx_contacts_suppression_status ON contacts(suppression_status);
         CREATE INDEX IF NOT EXISTS idx_contacts_source_counts ON contacts(independent_source_count, source_occurrence_count);
         CREATE INDEX IF NOT EXISTS idx_candidates_category ON email_candidates(category, subcategory);
         CREATE INDEX IF NOT EXISTS idx_candidates_status_detail ON email_candidates(status, syntax_status, routing_status, mailbox_status);",
    )
    .map_err(|e| e.to_string())?;

    db.execute(
        "UPDATE contacts
         SET raw_email=COALESCE(raw_email,email),
             email_comparison_key=COALESCE(email_comparison_key,lower(trim(email)))
         WHERE email IS NOT NULL",
        [],
    )
    .map_err(|e| e.to_string())?;
    let comparison_duplicates: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT email_comparison_key FROM contacts
                WHERE email_comparison_key IS NOT NULL
                GROUP BY email_comparison_key HAVING COUNT(*) > 1
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if comparison_duplicates == 0 {
        db.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_contacts_comparison_key
                ON contacts(email_comparison_key) WHERE email_comparison_key IS NOT NULL;",
        )
        .map_err(|e| e.to_string())?;
    } else {
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_contacts_comparison_key
                ON contacts(email_comparison_key) WHERE email_comparison_key IS NOT NULL;",
        )
        .map_err(|e| e.to_string())?;
    }
    db.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_candidates_occurrence
            ON email_candidates(source_row_id, ifnull(comparison_key, lower(trim(raw_email))), ifnull(extraction_method, 'literal'), occurrence_index)
            WHERE source_row_id IS NOT NULL;",
    )
    .map_err(|e| e.to_string())?;

    db.execute(
        "INSERT OR IGNORE INTO suppressions(comparison_key, canonical_email, reason, source_file, created_at)
         SELECT normalized_email, normalized_email, 'blacklist', source_file, imported_at FROM blacklist",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn migrate_gazetteer(db: &Connection) -> Result<(), String> {
    db.execute_batch("SAVEPOINT gazetteer_migration;
      CREATE TABLE IF NOT EXISTS gazetteer_versions (
        id INTEGER PRIMARY KEY, semantic_version TEXT NOT NULL UNIQUE,
        status TEXT NOT NULL CHECK(status IN ('active','retired')),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
      CREATE TABLE IF NOT EXISTS gazetteer_places (
        id INTEGER PRIMARY KEY, gazetteer_version_id INTEGER NOT NULL REFERENCES gazetteer_versions(id),
        country_code TEXT NOT NULL CHECK(country_code IN ('IT','CH')),
        level TEXT NOT NULL, canonical_name TEXT NOT NULL, normalized_name TEXT NOT NULL,
        parent_id INTEGER REFERENCES gazetteer_places(id),
        UNIQUE(gazetteer_version_id,country_code,level,normalized_name));
      CREATE INDEX IF NOT EXISTS idx_gazetteer_lookup ON gazetteer_places(gazetteer_version_id,country_code,normalized_name);
      INSERT OR IGNORE INTO gazetteer_versions(id,semantic_version,status) VALUES(1,'gazetteer-v1','active');
      RELEASE gazetteer_migration;").map_err(|e| e.to_string())
}

pub(super) fn migrate_gazetteer_catalog(db: &Connection) -> Result<(), String> {
    if !table_columns(db, "gazetteer_places")?
        .iter()
        .any(|column| column == "external_id")
    {
        db.execute_batch(
            "ALTER TABLE gazetteer_places RENAME TO gazetteer_places_v1;
             CREATE TABLE gazetteer_places (
               id INTEGER PRIMARY KEY, gazetteer_version_id INTEGER NOT NULL REFERENCES gazetteer_versions(id),
               external_id TEXT, country_code TEXT NOT NULL CHECK(country_code IN ('IT','CH')),
               level TEXT NOT NULL, canonical_name TEXT NOT NULL, normalized_name TEXT NOT NULL,
               parent_id INTEGER REFERENCES gazetteer_places(id),
               UNIQUE(gazetteer_version_id,external_id));
             INSERT INTO gazetteer_places(id,gazetteer_version_id,country_code,level,canonical_name,normalized_name,parent_id)
               SELECT id,gazetteer_version_id,country_code,level,canonical_name,normalized_name,parent_id FROM gazetteer_places_v1;
             DROP TABLE gazetteer_places_v1;",
        )
        .map_err(|e| e.to_string())?;
    }
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_gazetteer_lookup
           ON gazetteer_places(gazetteer_version_id,country_code,normalized_name);
         CREATE TABLE IF NOT EXISTS gazetteer_aliases (
           place_id INTEGER NOT NULL REFERENCES gazetteer_places(id) ON DELETE CASCADE,
           alias TEXT NOT NULL, normalized_alias TEXT NOT NULL,
           PRIMARY KEY(place_id,normalized_alias));
         CREATE INDEX IF NOT EXISTS idx_gazetteer_alias_lookup
           ON gazetteer_aliases(normalized_alias);",
    )
    .map_err(|e| e.to_string())
}
