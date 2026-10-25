//! Taxonomy tables, seed data, and taxonomy-related migrations.

use super::*;

pub(crate) fn migrate_classification_architecture_v2(db: &Connection) -> Result<(), String> {
    let contact_columns = table_columns(db, "contacts")?;
    for (name, definition) in [
        (
            "taxonomy_version_id",
            "INTEGER REFERENCES taxonomy_versions(id)",
        ),
        (
            "classification_state",
            "TEXT NOT NULL DEFAULT 'unprocessed' CHECK(classification_state IN ('unprocessed','classified','not_applicable','insufficient_evidence','conflicting','taxonomy_gap'))",
        ),
        ("classification_state_reason", "TEXT"),
    ] {
        if !contact_columns.iter().any(|column| column == name) {
            db.execute(
                &format!("ALTER TABLE contacts ADD COLUMN {name} {definition}"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }

    let run_columns = table_columns(db, "ai_classification_runs")?;
    for (name, definition) in [("applied_order", "INTEGER"), ("rolled_back_at", "TEXT")] {
        if !run_columns.iter().any(|column| column == name) {
            db.execute(
                &format!("ALTER TABLE ai_classification_runs ADD COLUMN {name} {definition}"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }

    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS taxonomy_entries (
            taxonomy_version_id INTEGER NOT NULL REFERENCES taxonomy_versions(id),
            semantic_key TEXT NOT NULL,
            level TEXT NOT NULL CHECK(level IN ('category','subcategory')),
            parent_semantic_key TEXT,
            display_name TEXT NOT NULL,
            definition TEXT NOT NULL DEFAULT '{}',
            synonyms TEXT NOT NULL DEFAULT '[]',
            inclusions TEXT NOT NULL DEFAULT '[]',
            exclusions TEXT NOT NULL DEFAULT '[]',
            precedence_rules TEXT NOT NULL DEFAULT '[]',
            sort_order INTEGER NOT NULL DEFAULT 0,
            reviewer_only INTEGER NOT NULL DEFAULT 0 CHECK(reviewer_only IN (0,1)),
            PRIMARY KEY(taxonomy_version_id, level, semantic_key)
        );
        CREATE INDEX IF NOT EXISTS idx_taxonomy_entries_parent
            ON taxonomy_entries(taxonomy_version_id, parent_semantic_key, sort_order);

        CREATE TABLE IF NOT EXISTS ai_run_batches (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
            taxonomy_version_id INTEGER REFERENCES taxonomy_versions(id),
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'dispatched'
                CHECK(status IN ('dispatched','completed','failed')),
            contact_count INTEGER NOT NULL,
            dispatched_at TEXT NOT NULL,
            completed_at TEXT,
            system_prompt TEXT NOT NULL DEFAULT '',
            user_prompt TEXT NOT NULL DEFAULT '',
            raw_response TEXT,
            error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_ai_run_batches_run
            ON ai_run_batches(run_id, dispatched_at);

        CREATE TABLE IF NOT EXISTS taxonomy_change_candidates (
            id TEXT PRIMARY KEY,
            origin_run_id TEXT REFERENCES ai_classification_runs(id) ON DELETE SET NULL,
            origin_contact_id TEXT REFERENCES contacts(id) ON DELETE SET NULL,
            based_on_version_id INTEGER NOT NULL REFERENCES taxonomy_versions(id),
            change_json TEXT NOT NULL,
            evidence_json TEXT NOT NULL DEFAULT '[]',
            impact_count INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK(status IN ('pending','approved','rejected','stale','superseded')),
            resulting_version_id INTEGER REFERENCES taxonomy_versions(id),
            created_at TEXT NOT NULL,
            decided_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_taxonomy_change_candidates_status
            ON taxonomy_change_candidates(status, created_at);

        CREATE TABLE IF NOT EXISTS classification_benchmark_runs (
            id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            taxonomy_version_id INTEGER REFERENCES taxonomy_versions(id),
            settings_json TEXT NOT NULL DEFAULT '{}',
            report_json TEXT,
            status TEXT NOT NULL CHECK(status IN ('running','completed','failed')),
            started_at TEXT NOT NULL,
            finished_at TEXT
        );
        CREATE TABLE IF NOT EXISTS classification_benchmark_results (
            benchmark_run_id TEXT NOT NULL REFERENCES classification_benchmark_runs(id) ON DELETE CASCADE,
            case_id TEXT NOT NULL REFERENCES reviewed_benchmark_cases(case_id),
            expected_json TEXT NOT NULL,
            predicted_json TEXT,
            evidence_json TEXT NOT NULL DEFAULT '[]',
            raw_response TEXT,
            repair_response TEXT,
            validation_errors TEXT NOT NULL DEFAULT '[]',
            latency_ms INTEGER,
            PRIMARY KEY(benchmark_run_id, case_id)
        );
        CREATE INDEX IF NOT EXISTS idx_contacts_taxonomy_version
            ON contacts(taxonomy_version_id);
        CREATE INDEX IF NOT EXISTS idx_contacts_classification_state
            ON contacts(classification_state);",
    )
    .map_err(|error| error.to_string())?;

    let batch_columns = table_columns(db, "ai_run_batches")?;
    for (name, definition) in [
        ("system_prompt", "TEXT NOT NULL DEFAULT ''"),
        ("user_prompt", "TEXT NOT NULL DEFAULT ''"),
        ("raw_response", "TEXT"),
        ("error", "TEXT"),
    ] {
        if !batch_columns.iter().any(|column| column == name) {
            db.execute(
                &format!("ALTER TABLE ai_run_batches ADD COLUMN {name} {definition}"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }

    db.execute(
        "UPDATE classification_reviews
         SET status='rejected',resolved_at=coalesce(resolved_at,?1)
         WHERE status='pending' AND outcome<>'taxonomy_gap'",
        params![chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| error.to_string())?;

    let active_version_id: i64 = db
        .query_row(
            "SELECT id FROM taxonomy_versions WHERE status='active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    db.execute(
        "INSERT OR IGNORE INTO taxonomy_entries(
            taxonomy_version_id,semantic_key,level,parent_semantic_key,display_name,
            definition,synonyms,inclusions,exclusions,precedence_rules,sort_order,reviewer_only
         )
         SELECT ?1,c.semantic_key,'category',NULL,c.name,c.definition,c.synonyms,
                c.inclusions,c.exclusions,c.precedence_rules,c.sort_order,c.reviewer_only
         FROM categories c WHERE c.active=1 AND c.semantic_key IS NOT NULL",
        params![active_version_id],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "INSERT OR IGNORE INTO taxonomy_entries(
            taxonomy_version_id,semantic_key,level,parent_semantic_key,display_name,
            definition,synonyms,inclusions,exclusions,precedence_rules,sort_order,reviewer_only
         )
         SELECT ?1,s.semantic_key,'subcategory',c.semantic_key,s.name,s.definition,s.synonyms,
                s.inclusions,s.exclusions,s.precedence_rules,s.id,s.reviewer_only
         FROM subcategories s JOIN categories c ON c.id=s.category_id
         WHERE s.active=1 AND s.semantic_key IS NOT NULL AND c.semantic_key IS NOT NULL",
        params![active_version_id],
    )
    .map_err(|error| error.to_string())?;

    db.execute(
        "UPDATE contacts
         SET classification_state=CASE
             WHEN contact_kind='personale' THEN 'not_applicable'
             WHEN lower(trim(category)) IN ('','varie','unknown','sconosciuto') THEN 'unprocessed'
             ELSE 'classified'
         END
         WHERE classification_state='unprocessed'",
        [],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE contacts
         SET taxonomy_version_id=(
             SELECT tv.id FROM ai_run_artifacts a
             JOIN taxonomy_versions tv ON tv.semantic_version=a.catalog_version
             WHERE a.contact_id=contacts.id AND a.validated_result IS NOT NULL
             ORDER BY a.created_at DESC LIMIT 1
         )
         WHERE taxonomy_version_id IS NULL",
        [],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE ai_classification_runs SET applied_order=rowid WHERE applied_order IS NULL",
        [],
    )
    .map_err(|error| error.to_string())?;

    if has_table(db, "classification_reviews")? {
        db.execute(
            "INSERT OR IGNORE INTO taxonomy_change_candidates(
                id,origin_run_id,origin_contact_id,based_on_version_id,change_json,
                evidence_json,status,created_at,decided_at
             )
             SELECT r.id,
                    (SELECT fc.run_id FROM ai_field_changes fc WHERE fc.review_id=r.id LIMIT 1),
                    r.contact_id,?1,r.taxonomy_change,r.evidence,r.status,r.created_at,r.resolved_at
             FROM classification_reviews r
             WHERE r.outcome='taxonomy_gap' AND r.taxonomy_change IS NOT NULL",
            params![active_version_id],
        )
        .map_err(|error| error.to_string())?;
    }

    for column in [
        "classification_locked",
        "category_locked",
        "subcategory_locked",
        "comune_locked",
        "provincia_locked",
        "regione_locked",
    ] {
        if table_columns(db, "contacts")?
            .iter()
            .any(|name| name == column)
        {
            db.execute(&format!("ALTER TABLE contacts DROP COLUMN {column}"), [])
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn migrate_geography_hash_and_security_taxonomy(db: &Connection) -> Result<(), String> {
    let columns = table_columns(db, "contacts")?;
    if !columns.iter().any(|c| c == "geography_hash") {
        db.execute("ALTER TABLE contacts ADD COLUMN geography_hash TEXT", [])
            .map_err(|e| e.to_string())?;
    }
    db.execute(
        "CREATE INDEX IF NOT EXISTS idx_contacts_geography_hash ON contacts(geography_hash);",
        [],
    )
    .map_err(|e| e.to_string())?;

    if has_table(db, "categories")? && has_table(db, "subcategories")? {
        let category_id: Option<i64> = db
            .query_row(
                "SELECT id FROM categories WHERE name = 'Pubblica amministrazione'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        if let Some(category_id) = category_id {
            let version_id: Option<i64> = if has_table(db, "taxonomy_versions")? {
                db.query_row(
                    "SELECT id FROM taxonomy_versions WHERE semantic_version = ?1",
                    params![ACTIVE_TAXONOMY_VERSION],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
            } else {
                None
            };

            let subcategory_columns = table_columns(db, "subcategories")?;
            let has_semantic_key = subcategory_columns.iter().any(|c| c == "semantic_key");
            let has_definition = subcategory_columns.iter().any(|c| c == "definition");
            let has_taxonomy_version = subcategory_columns
                .iter()
                .any(|c| c == "taxonomy_version_id");
            let has_reviewer_only = subcategory_columns.iter().any(|c| c == "reviewer_only");

            let subcategory_id: Option<i64> = db
                .query_row(
                    "SELECT id FROM subcategories WHERE category_id = ?1 AND (name = ?2 OR normalized_name = ?3 OR normalized_name = 'forzedellordine')",
                    params![category_id, "Forze dell'ordine", "forze_dell_ordine"],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;

            if let Some(id) = subcategory_id {
                db.execute(
                    "UPDATE subcategories SET name = ?1, normalized_name = ?2, active = 1 WHERE id = ?3",
                    params!["Forze dell'ordine", "forze_dell_ordine", id],
                )
                .map_err(|e| e.to_string())?;
                if has_taxonomy_version && version_id.is_some() {
                    db.execute(
                        "UPDATE subcategories SET taxonomy_version_id = ?1 WHERE id = ?2",
                        params![version_id, id],
                    )
                    .map_err(|e| e.to_string())?;
                }
                if has_definition {
                    let def = "{\"it\": \"A police authority, law enforcement agency, military body, armed force unit, or fire and emergency service.\"}";
                    db.execute(
                        "UPDATE subcategories SET definition = ?1 WHERE id = ?2 AND (definition IS NULL OR definition = '{}' OR definition = '{\"it\":\"Forze dell''ordine\"}')",
                        params![def, id],
                    )
                    .map_err(|e| e.to_string())?;
                }
                if has_reviewer_only {
                    db.execute(
                        "UPDATE subcategories SET reviewer_only = 0 WHERE id = ?1",
                        params![id],
                    )
                    .map_err(|e| e.to_string())?;
                }
            } else {
                let def = "{\"it\": \"A police authority, law enforcement agency, military body, armed force unit, or fire and emergency service.\"}";
                if has_semantic_key {
                    db.execute(
                        "INSERT INTO subcategories(category_id, name, normalized_name, semantic_key, definition, taxonomy_version_id, active, reviewer_only)
                         VALUES (?1, ?2, ?3, 'law_enforcement', ?4, ?5, 1, 0)",
                        params![category_id, "Forze dell'ordine", "forze_dell_ordine", def, version_id],
                    )
                    .map_err(|e| e.to_string())?;
                } else {
                    db.execute(
                        "INSERT INTO subcategories(category_id, name, normalized_name, active) VALUES (?1, ?2, ?3, 1)",
                        params![category_id, "Forze dell'ordine", "forze_dell_ordine"],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn migrate_canonical_subcategory_labels(db: &Connection) -> Result<(), String> {
    seed_taxonomy(db)?;
    for &(category, alias, canonical) in crate::taxonomy::SUBCATEGORY_ALIASES {
        let category_id: i64 = db
            .query_row(
                "SELECT id FROM categories WHERE name=?1 AND active=1",
                params![category],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let subcategory_id: i64 = db
            .query_row(
                "SELECT s.id FROM subcategories s
                 WHERE s.category_id=?1 AND s.name=?2 AND s.active=1",
                params![category_id, canonical],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;

        db.execute(
            "UPDATE contacts
             SET subcategory=?1,category_id=?2,subcategory_id=?3
             WHERE lower(trim(category))=lower(?4)
               AND lower(trim(coalesce(subcategory,'')))=lower(?5)",
            params![canonical, category_id, subcategory_id, category, alias],
        )
        .map_err(|error| error.to_string())?;

        if has_table(db, "email_candidates")? {
            db.execute(
                "UPDATE email_candidates SET subcategory=?1
                 WHERE lower(trim(coalesce(category,'')))=lower(?2)
                   AND lower(trim(coalesce(subcategory,'')))=lower(?3)",
                params![canonical, category, alias],
            )
            .map_err(|error| error.to_string())?;
        }
        if has_table(db, "classification_claims")? {
            db.execute(
                "DELETE FROM classification_claims AS alias_claim
                 WHERE alias_claim.contact_id IS NOT NULL
                   AND lower(trim(alias_claim.category))=lower(?1)
                   AND lower(trim(coalesce(alias_claim.subcategory,'')))=lower(?2)
                   AND EXISTS(
                     SELECT 1 FROM classification_claims canonical_claim
                     WHERE canonical_claim.id<>alias_claim.id
                       AND canonical_claim.contact_id=alias_claim.contact_id
                       AND canonical_claim.source_row_id IS alias_claim.source_row_id
                       AND lower(trim(canonical_claim.category))=lower(?1)
                       AND lower(trim(coalesce(canonical_claim.subcategory,'')))=lower(?3)
                   )",
                params![category, alias, canonical],
            )
            .map_err(|error| error.to_string())?;
            db.execute(
                "UPDATE classification_claims SET subcategory=?1
                 WHERE lower(trim(category))=lower(?2)
                   AND lower(trim(coalesce(subcategory,'')))=lower(?3)",
                params![canonical, category, alias],
            )
            .map_err(|error| error.to_string())?;
        }
        if has_table(db, "reviewed_benchmark_cases")? {
            db.execute(
                "UPDATE reviewed_benchmark_cases SET subcategory=?1
                 WHERE lower(trim(category))=lower(?2)
                   AND lower(trim(coalesce(subcategory,'')))=lower(?3)",
                params![canonical, category, alias],
            )
            .map_err(|error| error.to_string())?;
        }

        db.execute(
            "UPDATE subcategories SET active=0
             WHERE category_id=?1 AND name<>?2
               AND lower(trim(name))=lower(?3)",
            params![category_id, canonical, alias],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Migration 68: "Contatto personale" is retired in favour of "Ente non
/// determinato" (the alias repair re-points contacts, claims and cases), and
/// the contacts, candidates and seeds of clinics/IRCCS, forze dell'ordine and tribunals are
/// deleted. Those three subcategories stay in the taxonomy.
pub(crate) fn migrate_retire_personal_and_sensitive_subcategories(
    db: &Connection,
) -> Result<(), String> {
    migrate_canonical_subcategory_labels(db)?;
    db.execute(
        "DELETE FROM taxonomy_entries
         WHERE level='subcategory' AND display_name='Contatto personale'",
        [],
    )
    .map_err(|error| error.to_string())?;
    const REMOVED: &str = "('clinica / irccs / struttura sanitaria','forze dell''ordine','tribunale / ufficio giudiziario')";
    let in_removed = format!("lower(trim(coalesce(subcategory,''))) IN {REMOVED}");
    for sql in [
        // Candidates first: contacts.id is only SET NULL on them.
        format!(
            "DELETE FROM email_candidates WHERE {in_removed}
             OR contact_id IN (SELECT id FROM contacts WHERE {in_removed})"
        ),
        format!("DELETE FROM contacts WHERE {in_removed}"),
        format!("DELETE FROM search_seeds WHERE {in_removed}"),
    ] {
        db.execute(&sql, []).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Migration 69: deletes the contacts, candidates and seeds of the whole Sanità
/// category (the taxonomy stays). Caritas and charities that were filed under
/// it are enti religiosi, so they move to "Caritas / ente caritativo religioso"
/// first.
pub(crate) fn migrate_remove_sanita_contacts(db: &Connection) -> Result<(), String> {
    let in_sanita = "(lower(trim(coalesce(category,''))) IN ('sanità','sanita')
         OR lower(trim(coalesce(subcategory,''))) IN (
            'ospedale / azienda ospedaliera','asl / asst / distretto sanitario',
            'clinica / irccs / struttura sanitaria','altro ente sanitario'))";
    let (category_id, subcategory_id): (i64, i64) = db
        .query_row(
            "SELECT c.id,s.id FROM categories c JOIN subcategories s ON s.category_id=c.id
             WHERE c.name='Enti religiosi' AND s.name='Caritas / ente caritativo religioso'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| error.to_string())?;
    let is_charity = "lower(coalesce(email,'')||' '||coalesce(organization_name,'')||' '||coalesce(denomination,'')||' '||coalesce(source_file,''))";
    db.execute(
        &format!(
            "UPDATE contacts
             SET category='Enti religiosi',subcategory='Caritas / ente caritativo religioso',
                 category_id=?1,subcategory_id=?2
             WHERE {in_sanita} AND ({is_charity} LIKE '%caritas%' OR {is_charity} LIKE '%caritativ%')"
        ),
        params![category_id, subcategory_id],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE email_candidates
         SET category='Enti religiosi',subcategory='Caritas / ente caritativo religioso'
         WHERE contact_id IN (SELECT id FROM contacts
                              WHERE subcategory='Caritas / ente caritativo religioso')",
        [],
    )
    .map_err(|error| error.to_string())?;
    for sql in [
        format!(
            "DELETE FROM email_candidates WHERE {in_sanita}
             OR contact_id IN (SELECT id FROM contacts WHERE {in_sanita})"
        ),
        format!("DELETE FROM contacts WHERE {in_sanita}"),
        format!("DELETE FROM search_seeds WHERE {in_sanita}"),
    ] {
        db.execute(&sql, []).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn migrate_classification_subcategory_fallbacks(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS classification_system_changes (
            id TEXT PRIMARY KEY,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            operation TEXT NOT NULL,
            field_name TEXT NOT NULL,
            old_value TEXT,
            new_value TEXT,
            evidence TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_classification_system_changes_contact
            ON classification_system_changes(contact_id,created_at);",
    )
    .map_err(|error| error.to_string())?;
    seed_taxonomy(db)?;
    let taxonomy_version_id: i64 = db
        .query_row(
            "SELECT id FROM taxonomy_versions WHERE status='active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let mut statement = db
        .prepare(
            "SELECT id,category,subcategory FROM contacts
             WHERE contact_kind <> 'personale'
               AND lower(trim(coalesce(subcategory,''))) IN ('','unknown','sconosciuto','varie','dato misto o da revisionare')",
        )
        .map_err(|error| error.to_string())?;
    let contacts = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    for (contact_id, old_category, old_subcategory) in contacts {
        let category = match crate::gazetteer::norm(&old_category).as_str() {
            "istruzione" => "Istruzione",
            "pubblica amministrazione" => "Pubblica amministrazione",
            "sanita" => "Sanità",
            "associazioni" => "Associazioni",
            "enti religiosi" => "Enti religiosi",
            "cultura" => "Cultura",
            "turismo e promozione territoriale" => "Turismo e promozione territoriale",
            _ => "Varie",
        };
        let subcategory = crate::taxonomy::fallback_subcategory(category)
            .expect("all backfill categories have a fallback");
        if old_category != category {
            db.execute(
                "INSERT INTO classification_system_changes(
                    id,contact_id,operation,field_name,old_value,new_value,evidence,created_at
                 ) VALUES(?1,?2,'subcategory-fallback-backfill','category',?3,?4,?5,?6)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    contact_id,
                    old_category,
                    category,
                    "Migrazione 39: normalizzazione della categoria e fallback tassonomico",
                    now
                ],
            )
            .map_err(|error| error.to_string())?;
        }
        if old_subcategory.as_deref() != Some(subcategory) {
            db.execute(
                "INSERT INTO classification_system_changes(
                    id,contact_id,operation,field_name,old_value,new_value,evidence,created_at
                 ) VALUES(?1,?2,'subcategory-fallback-backfill','subcategory',?3,?4,?5,?6)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    contact_id,
                    old_subcategory,
                    subcategory,
                    "Migrazione 39: assegnazione della sottocategoria generica",
                    now
                ],
            )
            .map_err(|error| error.to_string())?;
        }
    }
    db.execute(
        "UPDATE contacts SET
            category=CASE lower(trim(coalesce(category,'')))
                WHEN 'istruzione' THEN 'Istruzione'
                WHEN 'pubblica amministrazione' THEN 'Pubblica amministrazione'
                WHEN 'sanità' THEN 'Sanità'
                WHEN 'sanita' THEN 'Sanità'
                WHEN 'associazioni' THEN 'Associazioni'
                WHEN 'enti religiosi' THEN 'Enti religiosi'
                WHEN 'cultura' THEN 'Cultura'
                WHEN 'turismo e promozione territoriale' THEN 'Turismo e promozione territoriale'
                ELSE 'Varie'
            END,
            subcategory=CASE lower(trim(coalesce(category,'')))
                WHEN 'istruzione' THEN 'Altro ente formativo'
                WHEN 'pubblica amministrazione' THEN 'Altro ente pubblico'
                WHEN 'sanità' THEN 'Altro ente sanitario'
                WHEN 'sanita' THEN 'Altro ente sanitario'
                WHEN 'associazioni' THEN 'Associazione generica'
                WHEN 'enti religiosi' THEN 'Altro ente religioso'
                WHEN 'cultura' THEN 'Altro ente culturale'
                WHEN 'turismo e promozione territoriale' THEN 'Altro ente turistico'
                ELSE 'Ente non determinato'
            END,
            classification_method='taxonomy_default',
            classification_confidence=0.5,
            classification_evidence='[\"Assegnazione sistemica della sottocategoria generica\"]',
            taxonomy_version_id=?1,
            classification_hash=NULL,
            classification_state='classified',
            classification_state_reason='Sottocategoria generica assegnata in attesa di una classificazione più specifica',
            needs_review=0
         WHERE contact_kind <> 'personale'
           AND lower(trim(coalesce(subcategory,''))) IN ('','unknown','sconosciuto','varie','dato misto o da revisionare')",
        params![taxonomy_version_id],
    )
    .map_err(|error| error.to_string())?;
    if !table_columns(db, "classification_reviews")?
        .iter()
        .any(|column| column == "taxonomy_change_candidate_id")
    {
        db.execute(
            "ALTER TABLE classification_reviews ADD COLUMN taxonomy_change_candidate_id TEXT REFERENCES taxonomy_change_candidates(id) ON DELETE SET NULL",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    db.execute(
        "UPDATE classification_reviews SET taxonomy_change_candidate_id=id
         WHERE taxonomy_change_candidate_id IS NULL AND outcome='taxonomy_gap'",
        [],
    )
    .map_err(|error| error.to_string())?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS classification_follow_up_queue (
            contact_id TEXT PRIMARY KEY REFERENCES contacts(id) ON DELETE CASCADE,
            catalog_id TEXT NOT NULL REFERENCES deterministic_rule_catalogs(id),
            reason TEXT NOT NULL CHECK(reason IN ('no_match','conflict','geography_conflict','missing_subcategory')),
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_classification_follow_up_reason
            ON classification_follow_up_queue(reason, created_at);
        CREATE TABLE IF NOT EXISTS taxonomy_change_candidate_contacts (
            candidate_id TEXT NOT NULL REFERENCES taxonomy_change_candidates(id) ON DELETE CASCADE,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            PRIMARY KEY(candidate_id, contact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_taxonomy_candidate_contacts_contact
            ON taxonomy_change_candidate_contacts(contact_id);",
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "INSERT OR IGNORE INTO taxonomy_change_candidate_contacts(candidate_id,contact_id)
         SELECT id,origin_contact_id FROM taxonomy_change_candidates
         WHERE origin_contact_id IS NOT NULL",
        [],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE contacts SET
            category_id=(SELECT id FROM categories WHERE name=contacts.category AND active=1),
            subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id
                            WHERE c.name=contacts.category AND s.name=contacts.subcategory AND s.active=1)
         WHERE contact_kind <> 'personale'",
        [],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_taxonomy_candidate_grouping(db: &Connection) -> Result<(), String> {
    if !table_columns(db, "taxonomy_change_candidates")?
        .iter()
        .any(|column| column == "group_key")
    {
        db.execute(
            "ALTER TABLE taxonomy_change_candidates ADD COLUMN group_key TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    let mut statement = db
        .prepare(
            "SELECT id,change_json FROM taxonomy_change_candidates
             WHERE group_key='' OR group_key IS NULL",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut updates = Vec::new();
    for row in rows {
        let (id, change_json) = row.map_err(|error| error.to_string())?;
        if let Ok(gap) = serde_json::from_str::<crate::TaxonomyGap>(&change_json) {
            updates.push((id, crate::taxonomy::taxonomy_gap_group_key(&gap)));
        }
    }
    for (id, group_key) in updates {
        db.execute(
            "UPDATE taxonomy_change_candidates SET group_key=?1 WHERE id=?2",
            params![group_key, id],
        )
        .map_err(|error| error.to_string())?;
    }
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_taxonomy_candidate_group
             ON taxonomy_change_candidates(based_on_version_id,group_key,status);",
    )
    .map_err(|error| error.to_string())
}

pub const TAXONOMY: &[(&str, &[&str])] = &[
    (
        "Istruzione",
        &[
            "Infanzia",
            "Primaria",
            "Secondaria di primo grado",
            "Secondaria di secondo grado",
            "Istituto comprensivo",
            "Centro territoriale / CPIA",
            "Ufficio scolastico territoriale",
            "Università / alta formazione",
            "Formazione professionale",
            "Altro ente formativo",
        ],
    ),
    (
        "Pubblica amministrazione",
        &[
            "Comune",
            "Comunità montana",
            "Provincia / Città metropolitana",
            "Regione / Ente regionale",
            "Ministero / Ente statale",
            "Tribunale / Ufficio giudiziario",
            "Consorzio / Ente pubblico",
            "Altro ente pubblico",
            "Forze dell'ordine",
        ],
    ),
    (
        "Sanità",
        &[
            "Ospedale / Azienda ospedaliera",
            "ASL / ASST / Distretto sanitario",
            "Clinica / IRCCS / Struttura sanitaria",
            "Altro ente sanitario",
        ],
    ),
    (
        "Associazioni",
        &[
            "Alpini",
            "CAI",
            "NOI",
            "Associazione genitori",
            "Sportiva",
            "Culturale",
            "Lions",
            "Rotary",
            "Volontariato / solidarietà",
            "Cooperativa / Terzo settore",
            "Fondazione",
            "Associazione generica",
        ],
    ),
    (
        "Enti religiosi",
        &[
            "Parrocchia",
            "Oratorio / centro giovanile",
            "Chiesa",
            "Santuario / basilica",
            "Diocesi / curia",
            "Decanato / vicariato",
            "Clero",
            "Altro ente religioso",
            "Frati Minori",
            "Domenicani",
            "Agostiniani",
            "Salesiani",
            "Caritas / ente caritativo religioso",
        ],
    ),
    (
        "Cultura",
        &[
            "Biblioteca",
            "Teatro",
            "Cinema",
            "Museo",
            "Centro culturale",
            "Fondazione culturale",
            "Altro ente culturale",
        ],
    ),
    (
        "Turismo e promozione territoriale",
        &[
            "Pro Loco",
            "Azienda di Promozione Turistica",
            "Ufficio turistico",
            "Consorzio turistico",
            "Organizzatore di eventi e sagre",
            "Altro ente turistico",
        ],
    ),
    (
        "Varie",
        &["Ente non determinato", "Dato misto o da revisionare"],
    ),
];

pub(crate) fn migrate_taxonomy_schema(db: &Connection) -> Result<(), String> {
    db.execute_batch("SAVEPOINT taxonomy_migration;
        CREATE TABLE IF NOT EXISTS taxonomy_versions (id INTEGER PRIMARY KEY, semantic_version TEXT NOT NULL UNIQUE, status TEXT NOT NULL CHECK(status IN ('active','retired')), created_at TEXT NOT NULL, activated_at TEXT, CHECK(status <> 'active' OR activated_at IS NOT NULL));") .map_err(|e| e.to_string())?;
    for (table, extra) in [
        (
            "categories",
            "taxonomy_version_id INTEGER REFERENCES taxonomy_versions(id), semantic_key TEXT, definition TEXT NOT NULL DEFAULT '{}', synonyms TEXT NOT NULL DEFAULT '[]', inclusions TEXT NOT NULL DEFAULT '[]', exclusions TEXT NOT NULL DEFAULT '[]', precedence_rules TEXT NOT NULL DEFAULT '[]', active INTEGER NOT NULL DEFAULT 1, reviewer_only INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "subcategories",
            "taxonomy_version_id INTEGER REFERENCES taxonomy_versions(id), semantic_key TEXT, definition TEXT NOT NULL DEFAULT '{}', synonyms TEXT NOT NULL DEFAULT '[]', inclusions TEXT NOT NULL DEFAULT '[]', exclusions TEXT NOT NULL DEFAULT '[]', precedence_rules TEXT NOT NULL DEFAULT '[]', reviewer_only INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let cols = table_columns(db, table)?;
        for part in extra.split(", ") {
            let name = part.split_whitespace().next().unwrap();
            if !cols.iter().any(|c| c == name) {
                db.execute(&format!("ALTER TABLE {table} ADD COLUMN {part}"), [])
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    db.execute("UPDATE taxonomy_versions SET status='retired' WHERE status='active' AND semantic_version<>?1", params![ACTIVE_TAXONOMY_VERSION]).map_err(|e| e.to_string())?;
    db.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS idx_taxonomy_active ON taxonomy_versions(status) WHERE status='active'; CREATE UNIQUE INDEX IF NOT EXISTS idx_category_version_key ON categories(taxonomy_version_id, semantic_key); CREATE UNIQUE INDEX IF NOT EXISTS idx_subcategory_version_key ON subcategories(taxonomy_version_id, semantic_key); -- semantic-key immutability triggers are installed after the initial seed") .map_err(|e| e.to_string())?;
    db.execute("INSERT OR IGNORE INTO taxonomy_versions(semantic_version,status,created_at,activated_at) VALUES (?1,'active',?2,?2)", params![ACTIVE_TAXONOMY_VERSION, chrono::Utc::now().to_rfc3339()]).map_err(|e| e.to_string())?;
    db.execute_batch("RELEASE taxonomy_migration;")
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn seed_taxonomy(db: &Connection) -> Result<(), String> {
    let _ = db.execute_batch(
        "DROP TRIGGER IF EXISTS categories_semantic_key_immutable;
         DROP TRIGGER IF EXISTS subcategories_semantic_key_immutable;",
    );
    let version_id: Option<i64> = if has_table(db, "taxonomy_versions")? {
        db.query_row(
            "SELECT id FROM taxonomy_versions WHERE semantic_version=?1",
            params![ACTIVE_TAXONOMY_VERSION],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
    } else {
        None
    };
    for (index, (category, subcategories)) in TAXONOMY.iter().enumerate() {
        db.execute(
            "INSERT INTO categories(name, sort_order) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET sort_order = excluded.sort_order",
            params![category, index as i64],
        )
        .map_err(|err| err.to_string())?;
        let category_id: i64 = db
            .query_row(
                "SELECT id FROM categories WHERE name=?1",
                params![category],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())?;
        if let Some(vid) = version_id {
            db.execute("UPDATE categories SET taxonomy_version_id=?1, semantic_key=COALESCE(semantic_key, ?2), definition=CASE WHEN definition='{}' THEN ?3 ELSE definition END, synonyms=COALESCE(synonyms,'[]'), inclusions=COALESCE(inclusions,'[]'), exclusions=COALESCE(exclusions,'[]'), precedence_rules=COALESCE(precedence_rules,'[]') WHERE id=?4", params![vid, normalize_key(category), format!("{{\"it\":\"{}\"}}", category), category_id]).map_err(|e| e.to_string())?;
        }
        for subcategory in *subcategories {
            let (semantic_key, normalized_name, definition) = if *subcategory == "Forze dell'ordine"
            {
                (
                    "law_enforcement".to_string(),
                    "forze_dell_ordine".to_string(),
                    "{\"it\": \"A police authority, law enforcement agency, military body, armed force unit, or fire and emergency service.\"}".to_string(),
                )
            } else {
                (
                    normalize_key(subcategory),
                    normalize_key(subcategory),
                    format!("{{\"it\":\"{}\"}}", subcategory),
                )
            };
            db.execute("INSERT OR IGNORE INTO subcategories(category_id, name, normalized_name) VALUES (?1, ?2, ?3)", params![category_id, subcategory, normalized_name]).map_err(|err| err.to_string())?;
            if let Some(vid) = version_id {
                db.execute("UPDATE subcategories SET taxonomy_version_id=?1, semantic_key=COALESCE(semantic_key, ?2), definition=CASE WHEN definition='{}' THEN ?3 ELSE definition END, synonyms=COALESCE(synonyms,'[]'), inclusions=COALESCE(inclusions,'[]'), exclusions=COALESCE(exclusions,'[]'), precedence_rules=COALESCE(precedence_rules,'[]'), reviewer_only=CASE WHEN ?4='Varie' THEN 1 ELSE reviewer_only END WHERE category_id=?5 AND name=?6", params![vid, semantic_key, definition, category, category_id, subcategory]).map_err(|e| e.to_string())?;
            }
        }
    }
    db.execute_batch("CREATE TRIGGER IF NOT EXISTS categories_semantic_key_immutable BEFORE UPDATE OF semantic_key ON categories WHEN OLD.semantic_key IS NOT NEW.semantic_key AND OLD.semantic_key IS NOT NULL BEGIN SELECT RAISE(ABORT, 'semantic_key is immutable'); END; CREATE TRIGGER IF NOT EXISTS subcategories_semantic_key_immutable BEFORE UPDATE OF semantic_key ON subcategories WHEN OLD.semantic_key IS NOT NEW.semantic_key AND OLD.semantic_key IS NOT NULL BEGIN SELECT RAISE(ABORT, 'semantic_key is immutable'); END;").map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn migrate_expanded_domain_taxonomy(db: &Connection) -> Result<(), String> {
    seed_taxonomy(db)?;
    Ok(())
}

pub(crate) fn migrate_legacy_taxonomy(db: &Connection) -> Result<(), String> {
    seed_taxonomy(db)?;
    db.execute(
        "UPDATE contacts
         SET category_id=(SELECT id FROM categories WHERE name=contacts.category),
             subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id
                             WHERE c.name=contacts.category AND lower(s.name)=lower(contacts.subcategory))",
        [],
    ).map_err(|err| err.to_string())?;
    Ok(())
}
