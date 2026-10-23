//! Legacy-database rebuild helpers and additive column migrations.

use super::*;

pub(super) fn migrate_review_columns(db: &Connection) -> Result<(), String> {
    for (column, definition) in [
        ("outcome", "TEXT NOT NULL DEFAULT 'proposal'"),
        ("candidates", "TEXT NOT NULL DEFAULT '[]'"),
        ("summary", "TEXT"),
        ("taxonomy_change", "TEXT"),
    ] {
        if !table_columns(db, "classification_reviews")?.contains(&column.to_owned()) {
            db.execute(
                &format!("ALTER TABLE classification_reviews ADD COLUMN {column} {definition}"),
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn migrate_field_lock_columns(db: &Connection) -> Result<(), String> {
    for (column, definition) in [
        ("category_locked", "INTEGER NOT NULL DEFAULT 0"),
        ("subcategory_locked", "INTEGER NOT NULL DEFAULT 0"),
        ("comune_locked", "INTEGER NOT NULL DEFAULT 0"),
        ("provincia_locked", "INTEGER NOT NULL DEFAULT 0"),
        ("regione_locked", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !table_columns(db, "contacts")?.iter().any(|c| c == column) {
            db.execute(
                &format!("ALTER TABLE contacts ADD COLUMN {column} {definition}"),
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn migrate_ai_run_columns(db: &Connection) -> Result<(), String> {
    let columns = table_columns(db, "ai_classification_runs")?;
    for (name, definition) in [
        ("request_json", "TEXT NOT NULL DEFAULT '{}'"),
        ("interruption_reason", "TEXT"),
        ("owner_session", "TEXT"),
        ("lease_expires_at", "INTEGER"),
    ] {
        if !columns.iter().any(|column| column == name) {
            db.execute(
                &format!("ALTER TABLE ai_classification_runs ADD COLUMN {name} {definition}"),
                [],
            )
            .map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn backup_database(db: &Connection, path: &std::path::Path) -> Result<PathBuf, String> {
    let timestamp = chrono::Utc::now().timestamp_millis();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("contatti.sqlite3");
    let backup_path = path.with_file_name(format!("{file_name}.backup-{timestamp}.sqlite3"));
    db.backup(rusqlite::MAIN_DB, &backup_path, None)
        .map_err(|err| {
            format!(
                "Backup migrazione non riuscito ({}): {err}",
                backup_path.display()
            )
        })?;
    Ok(backup_path)
}

pub(crate) fn migration_error(
    error: impl std::fmt::Display,
    backup: Option<&std::path::Path>,
) -> String {
    match backup {
        Some(path) => format!(
            "Migrazione database non riuscita; backup conservato in {}: {error}",
            path.display()
        ),
        None => format!("Migrazione database non riuscita: {error}"),
    }
}

pub(crate) fn legacy_value(columns: &[String], candidates: &[&str], fallback: &str) -> String {
    candidates
        .iter()
        .find(|candidate| columns.iter().any(|column| column == **candidate))
        .map(|column| format!("\"{column}\""))
        .unwrap_or_else(|| fallback.to_owned())
}

pub(crate) fn contact_dependent_tables(db: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .map_err(|e| e.to_string())?;
    let tables = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut dependent = Vec::new();
    for table in tables {
        let escaped = table.replace('"', "\"\"");
        let mut foreign_keys = db
            .prepare(&format!("PRAGMA foreign_key_list(\"{escaped}\")"))
            .map_err(|e| e.to_string())?;
        let references_contacts = foreign_keys
            .query_map([], |row| row.get::<_, String>(2))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|target| target == "contacts");
        if references_contacts {
            dependent.push(table);
        }
    }
    Ok(dependent)
}

pub(crate) fn copy_common_table_columns(
    db: &Connection,
    source: &str,
    destination: &str,
) -> Result<(), String> {
    let source_columns = table_columns(db, source)?;
    let destination_columns = table_columns(db, destination)?;
    let common = destination_columns
        .into_iter()
        .filter(|column| source_columns.contains(column))
        .collect::<Vec<_>>();
    if common.is_empty() {
        return Ok(());
    }
    let quoted = common
        .iter()
        .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",");
    db.execute(
        &format!(
            "INSERT INTO \"{}\" ({quoted}) SELECT {quoted} FROM \"{}\"",
            destination.replace('"', "\"\""),
            source.replace('"', "\"\"")
        ),
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn migrate_legacy_contacts(
    db: &mut Connection,
    backup_path: Option<&std::path::Path>,
    inject_failure: bool,
) -> Result<(), String> {
    let columns = table_columns(db, "contacts")?;
    let known_dependents = [
        "contact_sources",
        "classification_reviews",
        "ai_run_candidates",
        "ai_field_changes",
    ];
    let mut dependents = contact_dependent_tables(db)?;
    if let Some(unsupported) = dependents
        .iter()
        .find(|table| !known_dependents.contains(&table.as_str()))
    {
        return Err(migration_error(
            format!("tabella dipendente da contacts non supportata: {unsupported}"),
            backup_path,
        ));
    }
    dependents.sort_by_key(|table| {
        known_dependents
            .iter()
            .position(|known| known == table)
            .unwrap_or(usize::MAX)
    });
    let email = legacy_value(&columns, &["email", "normalized_email"], "NULL");
    if email == "NULL" {
        return Err(migration_error(
            "schema legacy senza una colonna email supportata",
            backup_path,
        ));
    }
    let old_count: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .map_err(|err| migration_error(err, backup_path))?;
    let tx = db
        .transaction()
        .map_err(|err| migration_error(err, backup_path))?;
    let result = (|| -> Result<(), String> {
        for table in &dependents {
            tx.execute_batch(&format!(
                "ALTER TABLE \"{}\" RENAME TO \"{}_legacy_dependency\";",
                table.replace('"', "\"\""),
                table.replace('"', "\"\"")
            ))
            .map_err(|err| err.to_string())?;
        }
        tx.execute_batch("ALTER TABLE contacts RENAME TO contacts_legacy_archive;")
            .map_err(|err| err.to_string())?;
        setup_schema(&tx)?;
        if inject_failure {
            return Err("errore di migrazione iniettato".to_owned());
        }
        let id = legacy_value(&columns, &["id"], "lower(hex(randomblob(16)))");
        let source_file = legacy_value(&columns, &["source_file"], "'legacy'");
        let category = legacy_value(&columns, &["category", "categories"], "'Varie'");
        let subcategory = legacy_value(&columns, &["subcategory"], "NULL");
        let email_kind = legacy_value(&columns, &["email_kind"], "NULL");
        let comune = legacy_value(&columns, &["comune"], "NULL");
        let provincia = legacy_value(&columns, &["provincia"], "NULL");
        let regione = legacy_value(&columns, &["regione"], "NULL");
        let website = legacy_value(&columns, &["website"], "NULL");
        let source_url = legacy_value(&columns, &["source_url"], "NULL");
        let status = legacy_value(&columns, &["status"], "'domain_unverified'");
        let notes = legacy_value(&columns, &["validation_notes"], "'[]'");
        let local_part = format!("substr({email},1,instr({email},'@')-1)");
        let domain = format!("substr({email},instr({email},'@')+1)");
        let tld = format!(
            "CASE WHEN instr({domain},'.') > 0
             THEN '.' || replace({domain},rtrim({domain},replace({domain},'.','')),'')
             ELSE '' END"
        );
        tx.execute(
            &format!(
                "INSERT INTO contacts (
                    id,email,email_local_part,email_domain,email_tld,status,validation_notes,
                    source_file,category,subcategory,is_pec,comune,provincia,regione,website,
                    source_url,classification_method
                 )
                 SELECT {id},lower(trim({email})),{local_part},{domain},{tld},{status},{notes},
                        {source_file},{category},{subcategory},
                        CASE WHEN lower(coalesce({email_kind},''))='pec' THEN 1 ELSE 0 END,
                        {comune},{provincia},
                        {regione},{website},{source_url},'legacy'
                 FROM contacts_legacy_archive"
            ),
            [],
        )
        .map_err(|err| err.to_string())?;
        let new_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
            .map_err(|err| err.to_string())?;
        if new_count != old_count {
            return Err(format!(
                "validazione conteggio fallita ({old_count} righe sorgente, {new_count} migrate)"
            ));
        }
        let missing: i64 = tx
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM contacts_legacy_archive l
                     WHERE NOT EXISTS (
                       SELECT 1 FROM contacts n WHERE n.email=lower(trim(l.{email}))
                     )"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(|err| err.to_string())?;
        if missing != 0 {
            return Err("validazione email primaria fallita".to_owned());
        }
        for table in &dependents {
            copy_common_table_columns(&tx, &format!("{table}_legacy_dependency"), table)?;
        }
        let foreign_key_errors: i64 = tx
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        if foreign_key_errors != 0 {
            return Err(format!(
                "validazione foreign key fallita ({foreign_key_errors} violazioni)"
            ));
        }
        for table in dependents.iter().rev() {
            tx.execute_batch(&format!(
                "DROP TABLE \"{}_legacy_dependency\";",
                table.replace('"', "\"\"")
            ))
            .map_err(|err| err.to_string())?;
        }
        tx.execute_batch("DROP TABLE contacts_legacy_archive;")
            .map_err(|err| err.to_string())?;
        tx.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)
            .map_err(|err| err.to_string())?;
        tx.commit().map_err(|err| err.to_string())
    })();
    result.map_err(|err| migration_error(err, backup_path))
}

pub(crate) fn migrate_contact_columns(db: &Connection) -> Result<(), String> {
    if has_table(db, "ai_run_artifacts")? {
        for (name, definition) in [
            ("validation_errors", "TEXT NOT NULL DEFAULT '[]'"),
            ("runtime_metadata", "TEXT NOT NULL DEFAULT '{}'"),
            ("catalog_version", "TEXT NOT NULL DEFAULT ''"),
            ("model_identity", "TEXT NOT NULL DEFAULT ''"),
            ("model_digest", "TEXT"),
            ("settings_json", "TEXT NOT NULL DEFAULT '{}'"),
            ("mode", "TEXT NOT NULL DEFAULT 'taxonomy'"),
        ] {
            if !table_columns(db, "ai_run_artifacts")?
                .iter()
                .any(|column| column == name)
            {
                db.execute(
                    &format!("ALTER TABLE ai_run_artifacts ADD COLUMN {name} {definition}"),
                    [],
                )
                .map_err(|err| err.to_string())?;
            }
        }
    }
    let columns = db
        .prepare("PRAGMA table_info(contacts)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|err| err.to_string())?;
    for (name, definition) in [
        ("source_url", "TEXT"),
        ("category_id", "INTEGER"),
        ("subcategory_id", "INTEGER"),
        ("classification_method", "TEXT NOT NULL DEFAULT 'legacy'"),
        ("classification_confidence", "REAL NOT NULL DEFAULT 0"),
        ("classification_evidence", "TEXT NOT NULL DEFAULT '[]'"),
        ("needs_review", "INTEGER NOT NULL DEFAULT 0"),
        ("classification_locked", "INTEGER NOT NULL DEFAULT 0"),
        ("classification_hash", "TEXT"),
        ("contact_kind", "TEXT NOT NULL DEFAULT 'non_determinato'"),
        ("paese", "TEXT NOT NULL DEFAULT 'Italia'"),
    ] {
        if !columns.iter().any(|column| column == name) {
            db.execute(
                &format!("ALTER TABLE contacts ADD COLUMN {name} {definition}"),
                [],
            )
            .map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn drop_obsolete_contact_columns(db: &Connection) -> Result<(), String> {
    if table_columns(db, "contacts")?
        .iter()
        .any(|column| column == "classified_at")
    {
        db.execute("ALTER TABLE contacts DROP COLUMN classified_at", [])
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}
