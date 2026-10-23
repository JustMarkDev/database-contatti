//! AI run, deterministic catalog, lead discovery, and settings migrations.

use super::*;
use std::collections::BTreeSet;

pub(crate) fn migrate_app_settings(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );",
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_claims_facet_indexes(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_classification_claims_category_contact
             ON classification_claims(category, contact_id) WHERE contact_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_classification_claims_subcategory_contact
             ON classification_claims(subcategory, contact_id) WHERE contact_id IS NOT NULL;",
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn migrate_ai_rollback_support(db: &Connection) -> Result<(), String> {
    let columns = table_columns(db, "ai_field_changes")?;
    if !columns.iter().any(|c| c == "rolled_back") {
        db.execute(
            "ALTER TABLE ai_field_changes ADD COLUMN rolled_back INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    seed_taxonomy(db)
}

pub(crate) fn migrate_classification_follow_up_queue(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS classification_follow_up_queue (
            contact_id TEXT PRIMARY KEY REFERENCES contacts(id) ON DELETE CASCADE,
            catalog_id TEXT NOT NULL REFERENCES deterministic_rule_catalogs(id),
            reason TEXT NOT NULL CHECK(reason IN ('no_match','conflict','geography_conflict','missing_subcategory')),
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_classification_follow_up_reason
            ON classification_follow_up_queue(reason, created_at);",
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_system_provenance_table(db: &Connection) -> Result<(), String> {
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
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_reset_classification_hashes(db: &Connection) -> Result<(), String> {
    db.execute(
        "UPDATE contacts SET classification_hash=NULL,taxonomy_version_id=NULL",
        [],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_remove_obsolete_geography_hash(db: &Connection) -> Result<(), String> {
    db.execute("DROP INDEX IF EXISTS idx_contacts_geography_hash", [])
        .map_err(|error| error.to_string())?;
    if table_columns(db, "contacts")?
        .iter()
        .any(|name| name == "geography_hash")
    {
        db.execute("ALTER TABLE contacts DROP COLUMN geography_hash", [])
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn migrate_retired_opencode_runs(db: &Connection) -> Result<(), String> {
    db.execute(
        "UPDATE ai_classification_runs
         SET status='failed',
             interruption_reason='OpenCode ritirato: la run non può essere ripresa',
             finished_at=coalesce(finished_at,?1),
             owner_session=NULL,
             lease_expires_at=NULL
         WHERE provider='opencode' AND status IN ('running','pausing','paused')",
        params![chrono::Utc::now().to_rfc3339()],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_benchmark_lifecycle(db: &Connection) -> Result<(), String> {
    for (column, definition) in [
        ("total_cases", "INTEGER NOT NULL DEFAULT 0"),
        ("processed_cases", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !table_columns(db, "classification_benchmark_runs")?.contains(&column.to_owned()) {
            db.execute(
                &format!(
                    "ALTER TABLE classification_benchmark_runs ADD COLUMN {column} {definition}"
                ),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS classification_benchmark_controls (
            benchmark_run_id TEXT PRIMARY KEY
                REFERENCES classification_benchmark_runs(id) ON DELETE CASCADE,
            status TEXT NOT NULL CHECK(status IN ('pausing','paused','cancelled')),
            requested_at TEXT NOT NULL
        );",
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn migrate_provider_runner_and_deterministic_catalog(
    db: &Connection,
) -> Result<(), String> {
    for (table, additions) in [
        (
            "ai_classification_runs",
            vec![
                ("provider", "TEXT NOT NULL DEFAULT 'opencode'"),
                (
                    "parent_run_id",
                    "TEXT REFERENCES ai_classification_runs(id)",
                ),
                ("deterministic_catalog_id", "TEXT"),
                ("deterministic_finished_at", "TEXT"),
            ],
        ),
        (
            "ai_run_candidates",
            vec![
                ("failure_kind", "TEXT"),
                ("attempt_count", "INTEGER NOT NULL DEFAULT 0"),
            ],
        ),
        (
            "ai_run_batches",
            vec![
                ("attempt", "INTEGER NOT NULL DEFAULT 1"),
                ("origin_batch_id", "TEXT"),
                ("usage_json", "TEXT NOT NULL DEFAULT '{}'"),
                ("input_bytes", "INTEGER NOT NULL DEFAULT 0"),
            ],
        ),
        (
            "ai_run_retries",
            vec![
                ("origin_batch_id", "TEXT"),
                ("provider", "TEXT NOT NULL DEFAULT 'opencode'"),
                ("model", "TEXT NOT NULL DEFAULT ''"),
                ("attempt", "INTEGER NOT NULL DEFAULT 1"),
                ("failure_kind", "TEXT NOT NULL DEFAULT 'structural'"),
                ("next_batch_size", "INTEGER NOT NULL DEFAULT 32"),
            ],
        ),
    ] {
        let columns = table_columns(db, table)?;
        for (name, definition) in additions {
            if !columns.iter().any(|column| column == name) {
                db.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN {name} {definition}"),
                    [],
                )
                .map_err(|error| error.to_string())?;
            }
        }
    }

    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS deterministic_rule_catalogs (
            id TEXT PRIMARY KEY,
            semantic_version TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL CHECK(status IN ('pending','active','retired')),
            rules_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            activated_at TEXT
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_deterministic_catalog_active
            ON deterministic_rule_catalogs(status) WHERE status='active';
        CREATE TABLE IF NOT EXISTS deterministic_rule_attributions (
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            rule_id TEXT NOT NULL,
            rule_version INTEGER NOT NULL,
            catalog_id TEXT NOT NULL REFERENCES deterministic_rule_catalogs(id),
            run_id TEXT REFERENCES ai_classification_runs(id) ON DELETE SET NULL,
            taxonomy_version_id INTEGER NOT NULL REFERENCES taxonomy_versions(id),
            applied_at TEXT NOT NULL,
            PRIMARY KEY(contact_id, rule_id, rule_version, catalog_id)
        );
        CREATE INDEX IF NOT EXISTS idx_deterministic_attribution_rule
            ON deterministic_rule_attributions(rule_id,rule_version,catalog_id);
        CREATE TABLE IF NOT EXISTS deterministic_recheck_queue (
            contact_id TEXT PRIMARY KEY REFERENCES contacts(id) ON DELETE CASCADE,
            catalog_id TEXT NOT NULL REFERENCES deterministic_rule_catalogs(id),
            reason TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_ai_runs_parent ON ai_classification_runs(parent_run_id);
        CREATE INDEX IF NOT EXISTS idx_ai_candidates_failure
            ON ai_run_candidates(run_id,failure_kind) WHERE failure_kind IS NOT NULL;",
    )
    .map_err(|error| error.to_string())?;

    let now = chrono::Utc::now().to_rfc3339();
    db.execute(
        "INSERT OR IGNORE INTO deterministic_rule_catalogs(
            id,semantic_version,status,rules_json,created_at
         ) VALUES('exact-it-v1','1.1.0','pending',?1,?2)",
        params![crate::deterministic_rules::BUILTIN_CATALOG_JSON, now],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn migrate_deterministic_catalog_v11(db: &Connection) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    db.execute(
        "INSERT OR IGNORE INTO deterministic_rule_catalogs(
            id,semantic_version,status,rules_json,created_at
         ) VALUES('exact-it-v1','1.1.0','pending',?1,?2)",
        params![crate::deterministic_rules::BUILTIN_CATALOG_JSON, now],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE deterministic_rule_catalogs
         SET semantic_version='1.1.0', rules_json=?1
         WHERE id='exact-it-v1'",
        params![crate::deterministic_rules::BUILTIN_CATALOG_JSON],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn migrate_deterministic_catalog_v12(db: &Connection) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    db.execute(
        "INSERT OR IGNORE INTO deterministic_rule_catalogs(
            id,semantic_version,status,rules_json,created_at
         ) VALUES('exact-it-v1','1.2.0','pending',?1,?2)",
        params![crate::deterministic_rules::BUILTIN_CATALOG_JSON, now],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE deterministic_rule_catalogs
         SET semantic_version='1.2.0', rules_json=?1
         WHERE id='exact-it-v1'",
        params![crate::deterministic_rules::BUILTIN_CATALOG_JSON],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Migration 47: sandboxed web-discovery tables.
///
/// These tables back the isolated web-search experiment. They never reference
/// the contact or blacklist stores: `search_leads.run_id` cascades only to
/// `search_runs`. The statement is idempotent so a fresh database and an
/// upgrade from v46 converge on the same schema without touching existing
/// tables.
pub(crate) fn migrate_sandboxed_web_discovery(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS search_runs (
            id TEXT PRIMARY KEY,
            query_text TEXT,
            intent_topic TEXT,
            intent_role TEXT,
            intent_region TEXT,
            status TEXT NOT NULL DEFAULT 'completed',
            seed_count INTEGER NOT NULL DEFAULT 0,
            page_count INTEGER NOT NULL DEFAULT 0,
            lead_count INTEGER NOT NULL DEFAULT 0,
            pdf_found INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS search_leads (
            id TEXT PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES search_runs(id) ON DELETE CASCADE,
            email TEXT NOT NULL,
            email_comparison_key TEXT NOT NULL,
            source_url TEXT,
            retrieved_at TEXT NOT NULL,
            score INTEGER NOT NULL DEFAULT 0,
            factors TEXT NOT NULL DEFAULT '[]',
            is_pec INTEGER NOT NULL DEFAULT 0,
            person_name TEXT,
            role TEXT
        );",
    )
    .map_err(|error| error.to_string())?;
    // Migration 60 drops `score`; replaying this migration on a newer database must not fail.
    if table_columns(db, "search_leads")?
        .iter()
        .any(|column| column == "score")
    {
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_search_leads_run_score
                ON search_leads(run_id, score DESC, email_comparison_key);",
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Migration 48: open-data source imports share the discovery sandbox. Runs
/// remember which catalog source produced them and leads carry the taxonomy,
/// organization and geography the source already knows. Still no reference to
/// `contacts` or `blacklist`.
pub(crate) fn migrate_lead_source_catalog(db: &Connection) -> Result<(), String> {
    let run_columns = table_columns(db, "search_runs")?;
    if !run_columns.iter().any(|column| column == "source_id") {
        db.execute("ALTER TABLE search_runs ADD COLUMN source_id TEXT", [])
            .map_err(|error| error.to_string())?;
    }
    let lead_columns = table_columns(db, "search_leads")?;
    for column in [
        "category",
        "subcategory",
        "organization",
        "comune",
        "provincia",
        "regione",
        "website",
    ] {
        if !lead_columns.iter().any(|existing| existing == column) {
            db.execute(
                &format!("ALTER TABLE search_leads ADD COLUMN {column} TEXT"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Sandbox copies of the contacts fields `official_code` and `department`, so
/// a future promotion maps source columns one to one.
pub(crate) fn migrate_lead_source_codes(db: &Connection) -> Result<(), String> {
    let lead_columns = table_columns(db, "search_leads")?;
    for column in ["official_code", "department"] {
        if !lead_columns.iter().any(|existing| existing == column) {
            db.execute(
                &format!("ALTER TABLE search_leads ADD COLUMN {column} TEXT"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Migrations 15 and 19 still create these never-written columns, so replaying
/// the chain on an old database ends here without them.
pub(crate) fn migrate_drop_compliance_columns(db: &Connection) -> Result<(), String> {
    for table in ["email_candidates", "contacts"] {
        let columns = table_columns(db, table)?;
        for column in ["permitted_purpose", "lawful_basis", "consent_provenance"] {
            if columns.iter().any(|existing| existing == column) {
                db.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

pub(crate) fn migrate_ai_run_batch_retries(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ai_run_retries (
            run_id TEXT NOT NULL REFERENCES ai_classification_runs(id) ON DELETE CASCADE,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            validation_errors TEXT NOT NULL,
            raw_response TEXT,
            created_at TEXT NOT NULL,
            PRIMARY KEY(run_id, contact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_ai_run_retries_run ON ai_run_retries(run_id);",
    )
    .map_err(|e| e.to_string())
}

/// Migration 56: built-in catalog 1.3.0. Fire brigade, armed forces and the
/// health ministry are state bodies, not law enforcement or health providers,
/// and long tokens no longer match inside other words (`laboratorio`). The
/// deterministic hash only names the catalog id, so a later pass would skip the
/// contacts it already classified: they are re-evaluated here instead. A contact
/// that no longer matches any rule goes back to the queue as unclassified.
pub(crate) fn migrate_deterministic_catalog_v13(db: &Connection) -> Result<(), String> {
    db.execute(
        "UPDATE deterministic_rule_catalogs
         SET semantic_version='1.3.0', rules_json=?1
         WHERE id='exact-it-v1'",
        params![crate::deterministic_rules::BUILTIN_CATALOG_JSON],
    )
    .map_err(|error| error.to_string())?;
    let catalog: crate::deterministic_rules::RuleCatalog =
        serde_json::from_str(crate::deterministic_rules::BUILTIN_CATALOG_JSON)
            .map_err(|error| error.to_string())?;
    let compiled = catalog.compile();
    let contacts = {
        let mut statement = db
            .prepare(
                "SELECT id,email,denomination,source_url,category,subcategory FROM contacts
                 WHERE classification_method='deterministic'",
            )
            .map_err(|error| error.to_string())?;

        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    for (id, email, denomination, source_url, category, subcategory) in contacts {
        let matched = crate::deterministic_rules::evaluate_compiled(
            &compiled,
            crate::deterministic_rules::DeterministicEvidence {
                email: &email,
                denomination: denomination.as_deref(),
                source_url: source_url.as_deref(),
            },
        )
        .ok()
        .flatten();
        let target = matched.map(|matched| {
            let sub = matched
                .result
                .subcategory
                .or_else(|| crate::taxonomy::fallback_subcategory(matched.result.category));
            (matched.result.category, sub)
        });
        match target {
            Some((new_category, new_subcategory))
                if new_category == category && new_subcategory == subcategory.as_deref() => {}
            Some((new_category, new_subcategory)) => {
                db.execute(
                    "UPDATE contacts SET category=?1,subcategory=?2,
                        category_id=(SELECT id FROM categories WHERE name=?1),
                        subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id
                                        WHERE c.name=?1 AND s.name=?2)
                     WHERE id=?3",
                    params![new_category, new_subcategory, id],
                )
                .map_err(|error| error.to_string())?;
            }
            None => {
                db.execute(
                    "UPDATE contacts SET category='Varie',subcategory=NULL,
                        category_id=(SELECT id FROM categories WHERE name='Varie'),subcategory_id=NULL,
                        classification_method='legacy',classification_confidence=0,
                        classification_hash=NULL,classification_state='unprocessed',
                        classification_state_reason=NULL
                     WHERE id=?1",
                    params![id],
                )
                .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

/// Migration 57: repairs from the 2026-09 classification audit.
/// - `taxonomy_change_candidate_contacts` was added to migration 40 after some
///   databases had already applied it; runs failed on the first taxonomy gap.
/// - Built-in deterministic catalog 1.4.0 (universities, health authorities,
///   chambers of commerce, ministry mailboxes, library and local-part tokens).
/// - Places read from email text through GeoNames alternates or mail-provider
///   labels (`.com` → Como, `virgilio.it` → Borgo Virgilio) are cleared, with the
///   province and region derived from them. Every cleared value is logged in
///   `classification_system_changes`.
/// - Contacts Jev marked insufficient while agreeing with their category
///   (confidence ≥ 0.6) go back to `classified`.
pub(crate) fn migrate_classification_audit_repairs(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS taxonomy_change_candidate_contacts (
            candidate_id TEXT NOT NULL REFERENCES taxonomy_change_candidates(id) ON DELETE CASCADE,
            contact_id TEXT NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
            PRIMARY KEY(candidate_id, contact_id)
        );
        CREATE INDEX IF NOT EXISTS idx_taxonomy_candidate_contacts_contact
            ON taxonomy_change_candidate_contacts(contact_id);
        INSERT OR IGNORE INTO taxonomy_change_candidate_contacts(candidate_id,contact_id)
            SELECT id,origin_contact_id FROM taxonomy_change_candidates
            WHERE origin_contact_id IS NOT NULL
              AND origin_contact_id IN (SELECT id FROM contacts);",
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE deterministic_rule_catalogs
         SET semantic_version='1.4.0', rules_json=?1
         WHERE id='exact-it-v1'",
        params![crate::deterministic_rules::BUILTIN_CATALOG_JSON],
    )
    .map_err(|error| error.to_string())?;
    repair_free_text_geography(db)?;
    restore_jev_agreements(db)
}

struct SystemChange<'a> {
    operation: &'a str,
    field: &'a str,
    old_value: Option<&'a str>,
    new_value: Option<&'a str>,
    evidence: &'a str,
}

fn log_system_change(
    db: &Connection,
    contact_id: &str,
    now: &str,
    change: SystemChange<'_>,
) -> Result<(), String> {
    let SystemChange {
        operation,
        field,
        old_value,
        new_value,
        evidence,
    } = change;
    db.execute(
        "INSERT INTO classification_system_changes(
            id,contact_id,operation,field_name,old_value,new_value,evidence,created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            crate::new_contact_id(),
            contact_id,
            operation,
            field,
            old_value,
            new_value,
            evidence,
            now
        ],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Places the pre-1.4 free-text scan could read from email/denomination/URL text:
/// every token phrase against every spelling, TLD and provider labels included.
fn legacy_free_text_places(
    email: &str,
    denomination: Option<&str>,
    source_url: Option<&str>,
) -> (BTreeSet<&'static str>, BTreeSet<&'static str>) {
    let mut text = String::new();
    for value in [Some(email), denomination, source_url]
        .into_iter()
        .flatten()
    {
        text.push(' ');
        text.push_str(value);
        if let Some((local, domain)) = value.split_once('@') {
            text.push(' ');
            text.push_str(local);
            for label in domain.split('.') {
                text.push(' ');
                text.push_str(label);
            }
        }
    }
    let normed = crate::gazetteer::norm(&text);
    let tokens = normed.split_whitespace().collect::<Vec<_>>();
    let index = &*crate::gazetteer::GAZETTEER_INDEX;
    let (mut towns, mut provinces) = (BTreeSet::new(), BTreeSet::new());
    for start in 0..tokens.len() {
        for end in start + 1..=tokens.len().min(start + 6) {
            let phrase = tokens[start..end].join(" ");
            if phrase.chars().count() < 3 {
                continue;
            }
            if let Some(name) = index.municipalities.get(&phrase) {
                towns.insert(*name);
            }
            if let Some(name) = index.provinces.get(&phrase) {
                provinces.insert(*name);
            }
        }
    }
    (towns, provinces)
}

/// Comparable key for a province however it was spelled: stored values, curated
/// aliases and raw GeoNames names disagree ("Trento", "Provincia autonoma di
/// Trento", "Roma Capitale", "Provincia Verbano-Cusio-Ossola").
fn canonical_province(value: &str) -> Option<String> {
    fn strip(key: &str) -> &str {
        for prefix in [
            "provincia autonoma di ",
            "provincia di ",
            "provincia ",
            "citta metropolitana di ",
            "libero consorzio comunale di ",
        ] {
            if let Some(rest) = key.strip_prefix(prefix) {
                return rest;
            }
        }
        key.strip_suffix(" capitale").unwrap_or(key)
    }
    let key = crate::gazetteer::norm(value);
    let name = crate::gazetteer::GAZETTEER_INDEX
        .provinces
        .get(strip(&key))
        .or_else(|| crate::gazetteer::GAZETTEER_INDEX.provinces.get(&key))?;
    Some(strip(&crate::gazetteer::norm(name)).to_owned())
}

fn region_of_province(province: &str) -> Option<&'static str> {
    let key = canonical_province(province)?;
    crate::parsing::PROVINCE_REGIONS
        .iter()
        .find(|(name, _)| canonical_province(name).as_deref() == Some(key.as_str()))
        .map(|(_, region)| *region)
}

fn repair_free_text_geography(db: &Connection) -> Result<(), String> {
    let contacts = {
        let mut statement = db
            .prepare(
                "SELECT id,email,denomination,source_url,address,comune,provincia,regione
                 FROM contacts
                 WHERE (comune IS NOT NULL AND trim(comune)<>'' AND instr(comune,'|')=0)
                    OR (provincia IS NOT NULL AND trim(provincia)<>'' AND instr(provincia,'|')=0)",
            )
            .map_err(|error| error.to_string())?;

        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    let mut sources = db
        .prepare(
            "SELECT coalesce(sr.raw_context,cs.raw_context) FROM contact_sources cs
             LEFT JOIN source_rows sr ON sr.id=cs.source_row_id
             WHERE cs.contact_id=?1",
        )
        .map_err(|error| error.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    let index = &*crate::gazetteer::GAZETTEER_INDEX;
    let catalog: crate::deterministic_rules::RuleCatalog =
        serde_json::from_str(crate::deterministic_rules::BUILTIN_CATALOG_JSON)
            .map_err(|error| error.to_string())?;
    let compiled = catalog.compile();
    for (id, email, denomination, source_url, address, comune, provincia, regione) in contacts {
        let (legacy_towns, legacy_provinces) =
            legacy_free_text_places(&email, denomination.as_deref(), source_url.as_deref());
        let comune_name = comune
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.contains('|'))
            .and_then(|value| {
                index
                    .municipalities
                    .get(&crate::gazetteer::norm(value))
                    .copied()
            });
        let province_name = provincia
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.contains('|'))
            .and_then(canonical_province);
        let legacy_provinces = legacy_provinces
            .into_iter()
            .filter_map(canonical_province)
            .collect::<BTreeSet<_>>();
        let suspect_town = comune_name.filter(|name| legacy_towns.contains(name));
        let suspect_province = province_name
            .clone()
            .filter(|name| legacy_provinces.contains(name));
        if suspect_town.is_none() && suspect_province.is_none() {
            continue;
        }
        let strict = crate::ai_typesafe::extract_place_candidates(
            &crate::ai_typesafe::place_evidence_context(
                &email,
                denomination.as_deref(),
                source_url.as_deref(),
            ),
            "Italia",
        );
        // Source rows (Comune/Indirizzo columns) and the address are real evidence.
        let mut evidence_text = address.clone().unwrap_or_default();
        let raws = sources
            .query_map(params![id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        for raw in raws {
            let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(&raw) else {
                continue;
            };
            for value in fields.values().filter_map(|value| value.as_str()) {
                let lower = value.trim().to_ascii_lowercase();
                if lower.contains('@') || lower.starts_with("http") || lower.starts_with("www.") {
                    continue;
                }
                evidence_text.push(' ');
                evidence_text.push_str(value);
            }
        }
        let evidence = format!(" {} ", crate::gazetteer::norm(&evidence_text));
        let mentioned =
            |name: &str| evidence.contains(&format!(" {} ", crate::gazetteer::norm(name)));
        // Places the current rules still derive (municipal domains, strict scan).
        let rule_geo = crate::deterministic_rules::evaluate_compiled(
            &compiled,
            crate::deterministic_rules::DeterministicEvidence {
                email: &email,
                denomination: denomination.as_deref(),
                source_url: source_url.as_deref(),
            },
        )
        .ok()
        .flatten()
        .map(|matched| (matched.result.comune, matched.result.provincia))
        .unwrap_or_default();
        let strict_provinces = strict
            .admin_areas
            .iter()
            .filter_map(|name| canonical_province(name))
            .chain(strict.municipalities.iter().flat_map(|town| {
                index
                    .municipality_locations
                    .get(&crate::gazetteer::norm(town))
                    .into_iter()
                    .flatten()
                    .filter_map(|(_, province)| canonical_province(province))
            }))
            .chain(rule_geo.1.as_deref().and_then(canonical_province))
            .collect::<BTreeSet<_>>();

        let clear_town = suspect_town
            .filter(|name| !strict.municipalities.iter().any(|found| found == name))
            .filter(|name| rule_geo.0.as_deref() != Some(*name))
            .filter(|name| !mentioned(name));
        let town_provinces = clear_town
            .and_then(|name| {
                index
                    .municipality_locations
                    .get(&crate::gazetteer::norm(name))
            })
            .map(|locations| {
                locations
                    .iter()
                    .map(|(_, province)| *province)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let clear_province = province_name
            .filter(|name| {
                suspect_province.as_ref() == Some(name)
                    || town_provinces
                        .iter()
                        .any(|province| canonical_province(province).as_ref() == Some(name))
            })
            .filter(|name| !strict_provinces.contains(name))
            .filter(|name| !mentioned(name));
        let derived_region = provincia
            .as_deref()
            .filter(|_| clear_province.is_some())
            .and_then(region_of_province)
            .or_else(|| {
                town_provinces
                    .iter()
                    .find_map(|province| region_of_province(province))
                    .filter(|_| clear_province.is_some())
            });
        let clear_region = regione
            .as_deref()
            .map(str::trim)
            .filter(|value| {
                derived_region.is_some_and(|region| {
                    crate::gazetteer::norm(region) == crate::gazetteer::norm(value)
                })
            })
            .filter(|value| !mentioned(value))
            .map(str::to_owned);

        let note = "Riparazione audit 2026-09: luogo letto dal testo email tramite alias \
                    GeoNames o dominio del provider (es. .com → Como), non presente nelle fonti";
        if clear_town.is_some() {
            db.execute("UPDATE contacts SET comune=NULL WHERE id=?1", params![id])
                .map_err(|error| error.to_string())?;
            log_system_change(
                db,
                &id,
                &now,
                SystemChange {
                    operation: "geography_alias_repair",
                    field: "comune",
                    old_value: comune.as_deref(),
                    new_value: None,
                    evidence: note,
                },
            )?;
        }
        if clear_province.is_some() {
            db.execute(
                "UPDATE contacts SET provincia=NULL WHERE id=?1",
                params![id],
            )
            .map_err(|error| error.to_string())?;
            log_system_change(
                db,
                &id,
                &now,
                SystemChange {
                    operation: "geography_alias_repair",
                    field: "provincia",
                    old_value: provincia.as_deref(),
                    new_value: None,
                    evidence: note,
                },
            )?;
        }
        if let Some(old_region) = clear_region {
            db.execute("UPDATE contacts SET regione=NULL WHERE id=?1", params![id])
                .map_err(|error| error.to_string())?;
            log_system_change(
                db,
                &id,
                &now,
                SystemChange {
                    operation: "geography_alias_repair",
                    field: "regione",
                    old_value: Some(&old_region),
                    new_value: None,
                    evidence: note,
                },
            )?;
        }
    }
    Ok(())
}

fn restore_jev_agreements(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS jev_agreement_repair AS
         WITH latest AS (
             SELECT a.contact_id, a.raw_response,
                    ROW_NUMBER() OVER (PARTITION BY a.contact_id ORDER BY a.created_at DESC) AS rn
             FROM ai_run_artifacts a
             JOIN contacts c ON c.id=a.contact_id
             WHERE c.classification_state='insufficient_evidence'
               AND c.classification_state_reason='Evidenza insufficiente secondo Jev'
               AND json_valid(a.raw_response)
         )
         SELECT c.id AS contact_id, c.classification_state_reason AS old_reason,
                json_extract(l.raw_response,'$.category_confidence') AS confidence
         FROM contacts c
         JOIN latest l ON l.contact_id=c.id AND l.rn=1
         JOIN categories k ON k.name=c.category
         WHERE json_extract(l.raw_response,'$.category')=k.semantic_key
           AND json_extract(l.raw_response,'$.category_confidence')>=0.6;",
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "INSERT INTO classification_system_changes(
            id,contact_id,operation,field_name,old_value,new_value,evidence,created_at
         )
         SELECT lower(hex(randomblob(16))),contact_id,'jev_agreement_repair',
                'classification_state','insufficient_evidence','classified',
                'Riparazione audit 2026-09: Jev concordava con la categoria esistente (confidenza '
                    || printf('%.2f',confidence) || ')',
                ?1
         FROM jev_agreement_repair",
        params![chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| error.to_string())?;
    db.execute_batch(
        "UPDATE contacts SET classification_state='classified',classification_state_reason=NULL
         WHERE id IN (SELECT contact_id FROM jev_agreement_repair);
         DROP TABLE jev_agreement_repair;",
    )
    .map_err(|error| error.to_string())
}

/// Migration 58: the Cerca lead sandbox mirrors the contact record and keeps a
/// list of sites to explore.
///
/// Leads gain the address, postal code, country and extraction method a
/// contact carries. Runs gain a kind (`web`, `source`, `directory`, `crawl`)
/// so a crawl can be recorded as `running` and finished later. `search_seeds`
/// holds the websites (or organization names still without a site) that open
/// lists, directories and existing contacts point at; one row per host (or per
/// name and comune) inside each list. Still no reference to `contacts`.
pub(crate) fn migrate_lead_sandbox_mirror(db: &Connection) -> Result<(), String> {
    let lead_columns = table_columns(db, "search_leads")?;
    for column in ["address", "postal_code", "paese", "method"] {
        if !lead_columns.iter().any(|existing| existing == column) {
            db.execute(
                &format!("ALTER TABLE search_leads ADD COLUMN {column} TEXT"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }
    let run_columns = table_columns(db, "search_runs")?;
    if !run_columns.iter().any(|column| column == "kind") {
        db.execute("ALTER TABLE search_runs ADD COLUMN kind TEXT", [])
            .map_err(|error| error.to_string())?;
    }
    db.execute_batch(
        "UPDATE search_runs SET kind = CASE WHEN source_id IS NULL THEN 'web' ELSE 'source' END
         WHERE kind IS NULL;
         CREATE INDEX IF NOT EXISTS idx_search_leads_comparison_key
            ON search_leads(email_comparison_key);
         CREATE TABLE IF NOT EXISTS search_seeds (
            id TEXT PRIMARY KEY,
            list_name TEXT NOT NULL,
            url TEXT,
            host TEXT,
            name TEXT,
            category TEXT,
            subcategory TEXT,
            comune TEXT,
            provincia TEXT,
            regione TEXT,
            address TEXT,
            postal_code TEXT,
            official_code TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            lead_count INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            created_at TEXT NOT NULL,
            crawled_at TEXT
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_search_seeds_identity
            ON search_seeds(list_name, coalesce(host, lower(coalesce(name,'')) || '|' || lower(coalesce(comune,''))));
         CREATE INDEX IF NOT EXISTS idx_search_seeds_status ON search_seeds(status, list_name);",
    )
    .map_err(|error| error.to_string())
}
