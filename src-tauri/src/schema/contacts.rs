//! Contact list/search projection: FTS, exportable flag, PEC and geography fixes.

use super::*;

pub(crate) fn migrate_list_search_performance(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "DROP INDEX IF EXISTS idx_contacts_email;
         DROP INDEX IF EXISTS idx_contacts_suppression_email;
         DROP INDEX IF EXISTS idx_contacts_source_counts;
         DROP INDEX IF EXISTS idx_contacts_geo;

         CREATE INDEX IF NOT EXISTS idx_contacts_list_sort
             ON contacts(lower(email), id)
             WHERE status='valid' AND coalesce(suppression_status,'active')='active';

         CREATE INDEX IF NOT EXISTS idx_contacts_list_category
             ON contacts(category, subcategory, lower(email))
             WHERE status='valid' AND coalesce(suppression_status,'active')='active';

         CREATE INDEX IF NOT EXISTS idx_contacts_list_pec
             ON contacts(is_pec, lower(email))
             WHERE status='valid' AND coalesce(suppression_status,'active')='active';

         CREATE VIRTUAL TABLE IF NOT EXISTS contacts_fts USING fts5(
             email,
             source_file,
             category,
             subcategory,
             contact_kind,
             paese,
             macroarea,
             comune,
             provincia,
             regione,
             content='contacts',
             content_rowid='rowid',
             tokenize='unicode61 remove_diacritics 2'
         );

         INSERT INTO contacts_fts(contacts_fts) VALUES('rebuild');

         CREATE TRIGGER IF NOT EXISTS contacts_fts_ai AFTER INSERT ON contacts BEGIN
             INSERT INTO contacts_fts(
                 rowid, email, source_file, category, subcategory, contact_kind,
                 paese, macroarea, comune, provincia, regione
             ) VALUES (
                 new.rowid, new.email, new.source_file, new.category, new.subcategory, new.contact_kind,
                 new.paese, new.macroarea, new.comune, new.provincia, new.regione
             );
         END;

         CREATE TRIGGER IF NOT EXISTS contacts_fts_ad AFTER DELETE ON contacts BEGIN
             INSERT INTO contacts_fts(contacts_fts, rowid, email, source_file, category, subcategory,
                 contact_kind, paese, macroarea, comune, provincia, regione)
             VALUES (
                 'delete', old.rowid, old.email, old.source_file, old.category, old.subcategory,
                 old.contact_kind, old.paese, old.macroarea, old.comune, old.provincia, old.regione
             );
         END;

         CREATE TRIGGER IF NOT EXISTS contacts_fts_au AFTER UPDATE ON contacts BEGIN
             INSERT INTO contacts_fts(contacts_fts, rowid, email, source_file, category, subcategory,
                 contact_kind, paese, macroarea, comune, provincia, regione)
             VALUES (
                 'delete', old.rowid, old.email, old.source_file, old.category, old.subcategory,
                 old.contact_kind, old.paese, old.macroarea, old.comune, old.provincia, old.regione
             );
             INSERT INTO contacts_fts(
                 rowid, email, source_file, category, subcategory, contact_kind,
                 paese, macroarea, comune, provincia, regione
             ) VALUES (
                 new.rowid, new.email, new.source_file, new.category, new.subcategory, new.contact_kind,
                 new.paese, new.macroarea, new.comune, new.provincia, new.regione
             );
         END;

         UPDATE ai_run_artifacts
         SET system_prompt='', user_prompt='', raw_response=NULL, retry_response=NULL, operational_log=''
         WHERE EXISTS (
             SELECT 1 FROM ai_classification_runs r
             WHERE r.id = ai_run_artifacts.run_id
               AND r.status IN ('completed','completed_with_errors','failed','cancelled')
         );

         UPDATE ai_run_batches
         SET system_prompt='', user_prompt='', raw_response=NULL
         WHERE EXISTS (
             SELECT 1 FROM ai_classification_runs r
             WHERE r.id = ai_run_batches.run_id
               AND r.status IN ('completed','completed_with_errors','failed','cancelled')
         );

         ANALYZE;",
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn migrate_deterministic_raw_context_backfill(db: &Connection) -> Result<(), String> {
    if !has_table(db, "contacts")? || !has_table(db, "contact_sources")? {
        return Ok(());
    }

    let mut update_stmt = db
        .prepare_cached(
            "UPDATE contacts SET
            denomination = COALESCE(?2, denomination),
            address = COALESCE(?3, address),
            postal_code = COALESCE(?4, postal_code),
            comune = COALESCE(?5, comune),
            provincia = COALESCE(?6, provincia),
            regione = COALESCE(?7, regione)
         WHERE id = ?1 AND (
            (?2 IS NOT NULL AND (denomination IS NULL OR trim(denomination)='')) OR
            (?3 IS NOT NULL AND (address IS NULL OR trim(address)='')) OR
            (?4 IS NOT NULL AND (postal_code IS NULL OR trim(postal_code)='')) OR
            (?5 IS NOT NULL AND (comune IS NULL OR trim(comune)='')) OR
            (?6 IS NOT NULL AND (provincia IS NULL OR trim(provincia)='')) OR
            (?7 IS NOT NULL AND (regione IS NULL OR trim(regione)=''))
         )",
        )
        .map_err(|e| e.to_string())?;

    let mut stmt = db
        .prepare(
            "SELECT cs.contact_id, cs.raw_context
         FROM contact_sources cs
         WHERE cs.raw_context IS NOT NULL AND cs.raw_context<>'' AND cs.raw_context<>'{}'",
        )
        .map_err(|e| e.to_string())?;

    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;

    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let contact_id: String = row.get(0).map_err(|e| e.to_string())?;
        let raw_context: String = row.get(1).map_err(|e| e.to_string())?;

        let raw: serde_json::Value = match serde_json::from_str(&raw_context) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let map = match raw.as_object() {
            Some(m) => m,
            None => continue,
        };

        let mut new_denom = None;
        let mut new_addr = None;
        let mut new_cap = None;
        let mut new_comune = None;
        let mut new_prov = None;
        let mut new_reg = None;

        for (k, v) in map {
            let val = match v {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => continue,
            };
            if val.is_empty() {
                continue;
            }
            let lower = k.trim().to_ascii_lowercase();
            if new_denom.is_none()
                && matches!(
                    lower.as_str(),
                    "denominazione dell'istituto"
                        | "denominazione istituto"
                        | "denominazione"
                        | "ragione sociale"
                        | "ragione_sociale"
                        | "nome ente"
                        | "nome scuola"
                        | "struttura"
                        | "istituto"
                        | "intestazione"
                )
            {
                new_denom = Some(crate::ai_runs::normalize_field_value("Denominazione", &val));
            }
            if new_addr.is_none()
                && matches!(
                    lower.as_str(),
                    "indirizzo (via + numero civico)"
                        | "indirizzo"
                        | "via"
                        | "ubicazione"
                        | "sede"
                        | "indirizzo_sede"
                )
            {
                new_addr = Some(crate::ai_runs::normalize_field_value("Indirizzo", &val));
            }
            if new_cap.is_none()
                && matches!(
                    lower.as_str(),
                    "codice postale" | "cap" | "c.a.p." | "c.a.p" | "codice_postale"
                )
            {
                new_cap = Some(val.clone());
            }
            if new_comune.is_none()
                && matches!(
                    lower.as_str(),
                    "comune"
                        | "descrizione comune"
                        | "descrizionecomune"
                        | "comune sede"
                        | "città"
                        | "citta"
                        | "luogo"
                )
            {
                new_comune = crate::ai_enrichment::normalize_extracted_comune(&val);
            }
            if new_prov.is_none()
                && matches!(
                    lower.as_str(),
                    "provincia" | "prov" | "sigla provincia" | "sigla_provincia" | "provincia_sede"
                )
            {
                new_prov = Some(crate::ai_enrichment::normalize_extracted_province(
                    val.clone(),
                ));
            }
            if new_reg.is_none() && matches!(lower.as_str(), "regione" | "regione_sede") {
                new_reg = crate::ai_enrichment::normalize_extracted_regione(&val);
            }
        }

        if new_denom.is_some()
            || new_addr.is_some()
            || new_cap.is_some()
            || new_comune.is_some()
            || new_prov.is_some()
            || new_reg.is_some()
        {
            let _ = update_stmt.execute(params![
                contact_id, new_denom, new_addr, new_cap, new_comune, new_prov, new_reg
            ]);
        }
    }

    Ok(())
}

pub(super) fn migrate_macroarea_field(db: &Connection) -> Result<(), String> {
    let columns = table_columns(db, "contacts")?;
    if !columns.iter().any(|c| c == "macroarea") {
        db.execute("ALTER TABLE contacts ADD COLUMN macroarea TEXT", [])
            .map_err(|e| e.to_string())?;
    }
    // Values come from migration 49, which derives macroarea from regione.
    db.execute_batch("CREATE INDEX IF NOT EXISTS idx_contacts_macroarea ON contacts(macroarea);")
        .map_err(|e| e.to_string())
}

pub(super) fn migrate_geographic_sanitization(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "UPDATE contacts SET regione = 'Valle d''Aosta' WHERE lower(replace(replace(coalesce(regione,''),'’',\"'\"),'daosta','d''aosta')) IN ('valle d''aosta', 'valle d aosta', 'valle daosta', 'vda', 'val d''aoste', 'val d aoste', 'vallee d aoste', 'vallée d''aoste') OR lower(coalesce(regione,'')) LIKE '%valle%aosta%' OR lower(coalesce(regione,'')) LIKE '%valle%daosta%';
         UPDATE contacts SET regione = 'Abruzzo' WHERE lower(coalesce(regione,'')) = 'abruzzi';
         UPDATE contacts SET regione = 'Friuli Venezia Giulia' WHERE lower(coalesce(regione,'')) IN ('friuli-venezia giulia', 'friuli venezia g.', 'friuli venezia g', 'friuli v.g.', 'friuli v. g.', 'friuli vg', 'fvg', 'friuli');
         UPDATE contacts SET regione = 'Trentino Alto Adige' WHERE lower(coalesce(regione,'')) IN ('trentino-alto adige', 'trentino-alto adige/südtirol', 'trentino-alto adige/sudtirol', 'trentino alto adige sudtirol', 'alto adige', 'sudtirol', 'südtirol');
         UPDATE contacts SET regione = 'Emilia Romagna' WHERE lower(coalesce(regione,'')) IN ('emilia-romagna', 'emilia');
         UPDATE contacts SET regione = 'Liguria' WHERE lower(coalesce(regione,'')) = 'genova';
         UPDATE contacts SET provincia = 'Forlì-Cesena' WHERE lower(replace(coalesce(provincia,''),'’',\"'\")) IN ('forli', 'forlì', 'forli''', 'forli cesena', 'forlì cesena', 'forli'' cesena', 'forli-cesena', 'fc', 'cesena') OR lower(coalesce(provincia,'')) LIKE 'forli%';
         UPDATE contacts SET provincia = 'Massa-Carrara' WHERE lower(coalesce(provincia,'')) IN ('massa', 'carrara', 'massa carrara', 'massa e carrara', 'massa - carrara', 'ms') OR lower(coalesce(provincia,'')) LIKE 'massa carrar%';
         UPDATE contacts SET provincia = 'Pesaro e Urbino' WHERE lower(coalesce(provincia,'')) IN ('pesaro', 'urbino', 'pesaro urbino', 'pesaro-urbino', 'pesaro - urbino', 'pu') OR lower(coalesce(provincia,'')) LIKE 'pesaro%';
         UPDATE contacts SET provincia = 'Barletta-Andria-Trani' WHERE lower(coalesce(provincia,'')) LIKE 'barletta%' OR lower(coalesce(provincia,'')) LIKE '%andria%' OR lower(coalesce(provincia,'')) LIKE '%trani%' OR lower(coalesce(provincia,'')) = 'bt';
         UPDATE contacts SET provincia = 'Verbano-Cusio-Ossola' WHERE lower(coalesce(provincia,'')) IN ('verbano cusio ossola', 'verbano - cusio - ossola', 'verbania', 'vb') OR lower(coalesce(provincia,'')) LIKE 'verbano%';
         UPDATE contacts SET provincia = 'Sud Sardegna' WHERE lower(coalesce(provincia,'')) IN ('carbonia-iglesias', 'carbonia iglesias', 'carbonia', 'iglesias', 'ci', 'medio campidano', 'vs', 'ogliastra', 'og', 'sud sardegna', 'su') OR lower(coalesce(provincia,'')) LIKE '%carbonia%';
         UPDATE contacts SET provincia = 'Sassari' WHERE lower(coalesce(provincia,'')) IN ('olbia-tempio', 'olbia tempio', 'olbia', 'tempio', 'ot');
         UPDATE contacts SET provincia = 'Monza e Brianza' WHERE lower(coalesce(provincia,'')) IN ('monza brianza', 'monza-brianza', 'monza e della brianza', 'monza', 'mb') OR lower(coalesce(provincia,'')) LIKE 'monza%';
         UPDATE contacts SET provincia = 'Reggio Calabria' WHERE lower(coalesce(provincia,'')) IN ('reggio di calabria', 'reggio calabria', 'rc') OR lower(coalesce(provincia,'')) LIKE 'reggio calabr%';
         UPDATE contacts SET provincia = 'Reggio Emilia' WHERE lower(replace(coalesce(provincia,''),'’',\"'\")) IN ('reggio nell''emilia', 'reggio nell’emilia', 'reggio emilia', 're') OR lower(coalesce(provincia,'')) LIKE 'reggio emil%';
         UPDATE contacts SET provincia = 'Fermo' WHERE lower(coalesce(provincia,'')) IN ('fermo', 'fm', 'provincia di fermo', 'province of fermo');
         UPDATE contacts SET provincia = 'La Spezia' WHERE lower(coalesce(provincia,'')) IN ('spezia', 'sp', 'la spezia', 'provincia della spezia', 'provincia di la spezia');
         UPDATE contacts SET provincia = 'Ascoli Piceno' WHERE lower(coalesce(provincia,'')) IN ('ascoli', 'ap', 'ascoli piceno', 'provincia di ascoli piceno');
         UPDATE contacts SET provincia = 'Vibo Valentia' WHERE lower(coalesce(provincia,'')) IN ('vibo', 'vv', 'vibo valentia', 'provincia di vibo valentia');
         UPDATE contacts SET provincia = 'L''Aquila' WHERE lower(replace(coalesce(provincia,''),'’',\"'\")) IN ('aquila', 'l''aquila', 'aq', 'provincia dell''aquila');
         UPDATE contacts SET provincia = 'Bolzano' WHERE lower(coalesce(provincia,'')) IN ('bozen', 'bolzano - bozen', 'bolzano/bozen', 'alto adige', 'bz');
         UPDATE contacts SET provincia = 'Aosta' WHERE lower(replace(coalesce(provincia,''),'’',\"'\")) IN ('valle d''aosta', 'valle d’aosta', 'ao');
         UPDATE contacts SET provincia = NULL WHERE lower(coalesce(provincia,'')) IN ('centro', 'isole', 'nord est', 'nord ovest', 'sud');
         UPDATE contacts SET comune = 'Abetone Cutigliano' WHERE lower(coalesce(comune,'')) IN ('abetone', 'cutigliano', 'abetone cutigliano');
         UPDATE contacts SET comune = 'Belvì' WHERE lower(replace(replace(replace(replace(coalesce(comune,''),'’',\"'\"),'‘',\"'\"),'´',\"'\"),'`',\"'\")) IN ('belvi', 'belvì', 'belvi''');
         UPDATE contacts SET comune = 'Andorno Micca' WHERE lower(coalesce(comune,'')) IN ('adorno micca', 'adorno micca (bi)', 'andorno micca (bi)');
         UPDATE contacts SET comune = trim(substr(comune, 1, instr(comune, '(') - 1)) WHERE instr(comune, '(') > 1 AND comune LIKE '%)';
         UPDATE contacts SET comune = trim(substr(comune, 1, instr(comune, '[') - 1)) WHERE instr(comune, '[') > 1 AND comune LIKE '%]';
         UPDATE contacts SET comune = replace(replace(comune, '’', \"'\"), '‘', \"'\") WHERE comune LIKE '%’%' OR comune LIKE '%‘%';
         UPDATE contacts SET comune = NULL WHERE length(trim(comune)) = 4 AND substr(trim(comune),1,1) GLOB '[A-Za-z]' AND substr(trim(comune),2,3) GLOB '[0-9][0-9][0-9]';
         UPDATE contacts SET comune = NULL WHERE trim(comune) GLOB '*[0-9]*';
         UPDATE contacts SET regione = NULL WHERE trim(regione) GLOB '[0-9]*' OR (regione IS NOT NULL AND regione NOT IN ('Abruzzo', 'Basilicata', 'Calabria', 'Campania', 'Emilia Romagna', 'Friuli Venezia Giulia', 'Lazio', 'Liguria', 'Lombardia', 'Marche', 'Molise', 'Piemonte', 'Puglia', 'Sardegna', 'Sicilia', 'Toscana', 'Trentino Alto Adige', 'Umbria', 'Valle d''Aosta', 'Veneto'));
         UPDATE contacts SET provincia = NULL WHERE trim(provincia) GLOB '[0-9]*' OR (provincia IS NOT NULL AND provincia NOT IN ('Agrigento', 'Alessandria', 'Ancona', 'Aosta', 'Arezzo', 'Ascoli Piceno', 'Asti', 'Avellino', 'Bari', 'Barletta-Andria-Trani', 'Belluno', 'Benevento', 'Bergamo', 'Biella', 'Bologna', 'Bolzano', 'Brescia', 'Brindisi', 'Cagliari', 'Caltanissetta', 'Campobasso', 'Caserta', 'Catania', 'Catanzaro', 'Chieti', 'Como', 'Cosenza', 'Cremona', 'Crotone', 'Cuneo', 'Enna', 'Fermo', 'Ferrara', 'Firenze', 'Foggia', 'Forlì-Cesena', 'Frosinone', 'Genova', 'Gorizia', 'Grosseto', 'Imperia', 'Isernia', 'L''Aquila', 'La Spezia', 'Latina', 'Lecce', 'Lecco', 'Livorno', 'Lodi', 'Lucca', 'Macerata', 'Mantova', 'Massa-Carrara', 'Matera', 'Messina', 'Milano', 'Modena', 'Monza e Brianza', 'Napoli', 'Novara', 'Nuoro', 'Oristano', 'Padova', 'Palermo', 'Parma', 'Pavia', 'Perugia', 'Pesaro e Urbino', 'Pescara', 'Piacenza', 'Pisa', 'Pistoia', 'Pordenone', 'Potenza', 'Prato', 'Ragusa', 'Ravenna', 'Reggio Calabria', 'Reggio Emilia', 'Rieti', 'Rimini', 'Roma', 'Rovigo', 'Salerno', 'Sassari', 'Savona', 'Siena', 'Siracusa', 'Sondrio', 'Sud Sardegna', 'Taranto', 'Teramo', 'Terni', 'Torino', 'Trapani', 'Trento', 'Treviso', 'Trieste', 'Udine', 'Varese', 'Venezia', 'Verbano-Cusio-Ossola', 'Vercelli', 'Verona', 'Vibo Valentia', 'Vicenza', 'Viterbo'));"
    )
    .map_err(|error| error.to_string())
}

pub(super) fn migrate_pec_flag_unification(db: &Connection) -> Result<(), String> {
    if !table_columns(db, "contacts")?
        .iter()
        .any(|column| column == "email_kind")
    {
        return Ok(());
    }
    db.execute(
        "UPDATE contacts SET is_pec = 1 WHERE lower(coalesce(email_kind, '')) = 'pec'",
        [],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// PEC is only `is_pec` (0/1). Promote by address pattern or PEC MX host, then
/// drop the redundant `email_kind` column that produced "standard | pec" merges.
/// Materializes the exportable contact projection into `contacts.is_exportable`.
/// Triggers keep it consistent with contacts, blacklist, and suppressions inside
/// the same transaction as the write, so lists and counts no longer evaluate the
/// automated-mailbox patterns and two lookups for every row.
/// Current contact FTS triggers. The update trigger only fires for indexed columns,
/// so classification, status, and flag updates do not rewrite the FTS index.
pub(crate) const CONTACTS_FTS_TRIGGERS: &str = "
    CREATE TRIGGER IF NOT EXISTS contacts_fts_ai AFTER INSERT ON contacts BEGIN
        INSERT INTO contacts_fts(
            rowid, email, source_file, category, subcategory, contact_kind,
            paese, macroarea, comune, provincia, regione
        ) VALUES (
            new.rowid, new.email, new.source_file, new.category, new.subcategory, new.contact_kind,
            new.paese, new.macroarea, new.comune, new.provincia, new.regione
        );
    END;
    CREATE TRIGGER IF NOT EXISTS contacts_fts_ad AFTER DELETE ON contacts BEGIN
        INSERT INTO contacts_fts(contacts_fts, rowid, email, source_file, category, subcategory,
            contact_kind, paese, macroarea, comune, provincia, regione)
        VALUES (
            'delete', old.rowid, old.email, old.source_file, old.category, old.subcategory,
            old.contact_kind, old.paese, old.macroarea, old.comune, old.provincia, old.regione
        );
    END;
    CREATE TRIGGER IF NOT EXISTS contacts_fts_au AFTER UPDATE OF
        email, source_file, category, subcategory, contact_kind,
        paese, macroarea, comune, provincia, regione ON contacts BEGIN
        INSERT INTO contacts_fts(contacts_fts, rowid, email, source_file, category, subcategory,
            contact_kind, paese, macroarea, comune, provincia, regione)
        VALUES (
            'delete', old.rowid, old.email, old.source_file, old.category, old.subcategory,
            old.contact_kind, old.paese, old.macroarea, old.comune, old.provincia, old.regione
        );
        INSERT INTO contacts_fts(
            rowid, email, source_file, category, subcategory, contact_kind,
            paese, macroarea, comune, provincia, regione
        ) VALUES (
            new.rowid, new.email, new.source_file, new.category, new.subcategory, new.contact_kind,
            new.paese, new.macroarea, new.comune, new.provincia, new.regione
        );
    END;";

/// Contact-side triggers for `contacts.is_exportable`. They only write when the
/// stored flag is wrong, so a valid insert keeps the default without a second write.
pub(crate) fn contact_exportable_triggers_sql() -> String {
    let exportable = crate::contacts::exportable_contact_sql("contacts");
    let new_exportable = crate::contacts::exportable_contact_sql("new");
    format!(
        "CREATE TRIGGER IF NOT EXISTS contacts_exportable_ai AFTER INSERT ON contacts
         WHEN new.is_exportable IS NOT ({new_exportable}) BEGIN
             UPDATE contacts SET is_exportable=({exportable}) WHERE rowid=new.rowid;
         END;
         CREATE TRIGGER IF NOT EXISTS contacts_exportable_au AFTER UPDATE OF
             email, status, suppression_status, email_comparison_key ON contacts
         WHEN new.is_exportable IS NOT ({new_exportable}) BEGIN
             UPDATE contacts SET is_exportable=({exportable}) WHERE rowid=new.rowid;
         END;"
    )
}

pub(crate) fn refresh_stale_exportable_flags(db: &Connection) -> Result<usize, String> {
    let exportable = crate::contacts::exportable_contact_sql("contacts");
    db.execute(
        &format!(
            "UPDATE contacts SET is_exportable=({exportable})
             WHERE is_exportable IS NOT ({exportable})"
        ),
        [],
    )
    .map_err(|error| error.to_string())
}

/// Materializes the exportable contact projection into `contacts.is_exportable`.
/// Triggers keep it consistent with contacts, blacklist, and suppressions inside
/// the same transaction as the write, so lists and counts no longer evaluate the
/// automated-mailbox patterns and two lookups for every row. Bulk imports suspend
/// the contact-side triggers and call `refresh_stale_exportable_flags` instead.
pub(crate) fn migrate_exportable_contact_projection(db: &Connection) -> Result<(), String> {
    if !table_columns(db, "contacts")?
        .iter()
        .any(|column| column == "is_exportable")
    {
        db.execute_batch(
            "ALTER TABLE contacts ADD COLUMN is_exportable INTEGER NOT NULL DEFAULT 1;",
        )
        .map_err(|error| error.to_string())?;
    }
    let exportable = crate::contacts::exportable_contact_sql("contacts");
    let stale = format!("is_exportable IS NOT ({exportable})");
    let contacts_matching_suppression = |key: &str| {
        format!(
            "email_comparison_key={key} COLLATE NOCASE
             OR (email_comparison_key IS NULL AND email={key} COLLATE NOCASE)"
        )
    };
    let new_suppression = contacts_matching_suppression("new.comparison_key");
    let old_suppression = contacts_matching_suppression("old.comparison_key");
    let fts_triggers = if has_table(db, "contacts_fts")? {
        format!("DROP TRIGGER IF EXISTS contacts_fts_au;\n{CONTACTS_FTS_TRIGGERS}")
    } else {
        String::new()
    };
    let contact_triggers = contact_exportable_triggers_sql();
    db.execute_batch(&format!(
        "CREATE INDEX IF NOT EXISTS idx_contacts_email_nocase ON contacts(email COLLATE NOCASE);
         CREATE INDEX IF NOT EXISTS idx_contacts_comparison_key_nocase
             ON contacts(email_comparison_key COLLATE NOCASE);

         DROP INDEX IF EXISTS idx_contacts_list_sort;
         DROP INDEX IF EXISTS idx_contacts_list_category;
         DROP INDEX IF EXISTS idx_contacts_list_pec;
         CREATE INDEX idx_contacts_list_sort ON contacts(lower(email), id) WHERE is_exportable=1;
         CREATE INDEX idx_contacts_list_category
             ON contacts(category, subcategory, lower(email)) WHERE is_exportable=1;
         CREATE INDEX idx_contacts_list_pec ON contacts(is_pec, lower(email)) WHERE is_exportable=1;

         {fts_triggers}

         DROP TRIGGER IF EXISTS contacts_exportable_ai;
         DROP TRIGGER IF EXISTS contacts_exportable_au;
         {contact_triggers}

         DROP TRIGGER IF EXISTS blacklist_exportable_ai;
         CREATE TRIGGER blacklist_exportable_ai AFTER INSERT ON blacklist BEGIN
             UPDATE contacts SET is_exportable=0
             WHERE email=new.normalized_email COLLATE NOCASE AND is_exportable=1;
         END;
         DROP TRIGGER IF EXISTS blacklist_exportable_ad;
         CREATE TRIGGER blacklist_exportable_ad AFTER DELETE ON blacklist BEGIN
             UPDATE contacts SET is_exportable=({exportable})
             WHERE email=old.normalized_email COLLATE NOCASE AND {stale};
         END;
         DROP TRIGGER IF EXISTS blacklist_exportable_au;
         CREATE TRIGGER blacklist_exportable_au AFTER UPDATE ON blacklist BEGIN
             UPDATE contacts SET is_exportable=({exportable})
             WHERE (email=old.normalized_email COLLATE NOCASE
                    OR email=new.normalized_email COLLATE NOCASE) AND {stale};
         END;

         DROP TRIGGER IF EXISTS suppressions_exportable_ai;
         CREATE TRIGGER suppressions_exportable_ai AFTER INSERT ON suppressions BEGIN
             UPDATE contacts SET is_exportable=({exportable})
             WHERE ({new_suppression}) AND {stale};
         END;
         DROP TRIGGER IF EXISTS suppressions_exportable_ad;
         CREATE TRIGGER suppressions_exportable_ad AFTER DELETE ON suppressions BEGIN
             UPDATE contacts SET is_exportable=({exportable})
             WHERE ({old_suppression}) AND {stale};
         END;
         DROP TRIGGER IF EXISTS suppressions_exportable_au;
         CREATE TRIGGER suppressions_exportable_au AFTER UPDATE ON suppressions BEGIN
             UPDATE contacts SET is_exportable=({exportable})
             WHERE ({old_suppression} OR {new_suppression}) AND {stale};
         END;"
    ))
    .map_err(|error| error.to_string())?;
    refresh_stale_exportable_flags(db).map(|_| ())
}

pub(crate) fn migrate_pec_is_pec_only(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "UPDATE contacts SET is_pec = 1
         WHERE is_pec = 0
           AND (
             lower(email) LIKE '%@pec.%'
             OR lower(email) LIKE '%.pec.%'
             OR lower(email) LIKE '%legalmail%'
             OR lower(email) LIKE '%postacert%'
             OR lower(email) LIKE '%cert.%'
             OR EXISTS (
               SELECT 1 FROM domain_verification_cache d
               JOIN json_each(d.mx_hosts_json) mx
               WHERE d.domain = lower(substr(contacts.email, instr(contacts.email, '@') + 1))
                 AND (
                   lower(mx.value) LIKE '%.pec.%'
                   OR lower(mx.value) LIKE 'pec.%'
                   OR lower(mx.value) LIKE '%postacert%'
                   OR lower(mx.value) LIKE '%legalmail%'
                   OR lower(mx.value) LIKE '%cert.%'
                   OR lower(mx.value) LIKE '%sicurezzapostale.it%'
                   OR lower(mx.value) LIKE '%postecert.it%'
                   OR lower(mx.value) LIKE '%telecompost.it%'
                   OR lower(mx.value) LIKE '%telecompec.it%'
                   OR lower(mx.value) LIKE '%pec-email.com%'
                   OR lower(mx.value) LIKE '%asmecal.it%'
                   OR lower(mx.value) LIKE '%asmecam.it%'
                   OR lower(mx.value) LIKE '%asmez.it%'
                   OR lower(mx.value) LIKE '%gespec.it%'
                   OR lower(mx.value) LIKE '%twtcert.it%'
                   OR lower(mx.value) LIKE '%mypec.eu%'
                   OR lower(mx.value) LIKE '%asmepec.it%'
                 )
             )
           );",
    )
    .map_err(|error| error.to_string())?;

    // Same rule for sandbox leads (address pattern only; no MX cache there).
    if has_table(db, "search_leads")? {
        db.execute_batch(
            "UPDATE search_leads SET is_pec = 1
             WHERE is_pec = 0
               AND (
                 lower(email) LIKE '%@pec.%'
                 OR lower(email) LIKE '%.pec.%'
                 OR lower(email) LIKE '%legalmail%'
                 OR lower(email) LIKE '%postacert%'
                 OR lower(email) LIKE '%cert.%'
               );",
        )
        .map_err(|error| error.to_string())?;
    }

    let columns = table_columns(db, "contacts")?;
    if columns.iter().any(|column| column == "email_kind") {
        db.execute("ALTER TABLE contacts DROP COLUMN email_kind", [])
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// SQL expression mapping `contacts.regione` to its macroarea. Unknown or
/// multi-value regions (`"Lazio | Umbria"`) give NULL until enrichment picks one.
pub(crate) fn macroarea_sql() -> String {
    let arms: String = crate::parsing::REGIONS
        .iter()
        .filter_map(|region| {
            let macroarea = crate::ai_enrichment::macroarea_from_regione(region)?;
            let key = region.replace('-', " ").to_lowercase().replace('\'', "''");
            Some(format!(" WHEN '{key}' THEN '{macroarea}'"))
        })
        .collect();
    format!("CASE lower(replace(trim(regione),'-',' ')){arms} END")
}

/// Turns `contacts.macroarea` into a virtual column generated from `regione`,
/// so every writer (imports, deterministic rules, AI enrichment, rollback)
/// gets a macroarea that follows the region instead of a stale copy.
pub(crate) fn migrate_macroarea_from_regione(db: &Connection) -> Result<(), String> {
    let generated: bool = db
        .query_row(
            "SELECT hidden=2 FROM pragma_table_xinfo('contacts') WHERE name='macroarea'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .unwrap_or(false);
    if generated {
        return Ok(());
    }
    // DROP COLUMN refuses columns used by an index or trigger: park the FTS
    // triggers and restore them verbatim afterwards.
    let triggers: Vec<(String, String)> = db
        .prepare(
            "SELECT name,sql FROM sqlite_master
             WHERE type='trigger' AND tbl_name='contacts' AND sql LIKE '%macroarea%'",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        })
        .map_err(|error| error.to_string())?;
    for (name, _) in &triggers {
        db.execute_batch(&format!("DROP TRIGGER {name};"))
            .map_err(|error| error.to_string())?;
    }
    db.execute_batch(&format!(
        "DROP INDEX IF EXISTS idx_contacts_macroarea;
         ALTER TABLE contacts DROP COLUMN macroarea;
         ALTER TABLE contacts ADD COLUMN macroarea TEXT GENERATED ALWAYS AS ({}) VIRTUAL;
         CREATE INDEX idx_contacts_macroarea ON contacts(macroarea);",
        macroarea_sql()
    ))
    .map_err(|error| error.to_string())?;
    for (_, sql) in &triggers {
        db.execute_batch(sql).map_err(|error| error.to_string())?;
    }
    if has_table(db, "contacts_fts")? {
        db.execute_batch("INSERT INTO contacts_fts(contacts_fts) VALUES('rebuild');")
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Migration 55: PEC detection compares whole DNS labels. Migration 53 matched
/// `cert.` anywhere, so `info@concert.it` became PEC. Clears the flag only where
/// no evidence is left: address labels, PEC MX hosts, or a source `tipo_mail`.
pub(crate) fn migrate_pec_label_detection(db: &Connection) -> Result<(), String> {
    let suspects = {
        let mut statement = db
            .prepare(
                "SELECT c.id,c.email,d.mx_hosts_json FROM contacts c
                 LEFT JOIN domain_verification_cache d
                   ON d.domain=lower(substr(c.email,instr(c.email,'@')+1))
                 WHERE c.is_pec=1 AND lower(c.email) LIKE '%cert.%'",
            )
            .map_err(|error| error.to_string())?;

        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?
    };
    let mut sources = db
        .prepare("SELECT raw_context FROM contact_sources WHERE contact_id=?1")
        .map_err(|error| error.to_string())?;
    for (id, email, mx_json) in suspects {
        let mx_hosts: Vec<String> = mx_json
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        if crate::parsing::is_pec(&email, &mx_hosts) {
            continue;
        }
        let raw_contexts = sources
            .query_map(params![id], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let declared_pec = raw_contexts.iter().any(|raw| {
            let fields: std::collections::BTreeMap<String, String> =
                serde_json::from_str::<std::collections::BTreeMap<String, serde_json::Value>>(raw)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_owned())))
                    .collect();
            crate::parsing::infer_field(&fields, &["tipo_mail", "tipo mail"])
                .is_some_and(|kind| crate::parsing::is_pec_from_kind(Some(&kind), ""))
        });
        if !declared_pec {
            db.execute("UPDATE contacts SET is_pec=0 WHERE id=?1", params![id])
                .map_err(|error| error.to_string())?;
        }
    }
    if has_table(db, "search_leads")? {
        let leads = {
            let mut statement = db
                .prepare("SELECT id,email FROM search_leads WHERE is_pec=1 AND lower(email) LIKE '%cert.%'")
                .map_err(|error| error.to_string())?;

            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        for (id, email) in leads {
            if !crate::parsing::is_pec_email(&email) {
                db.execute("UPDATE search_leads SET is_pec=0 WHERE id=?1", params![id])
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

/// GeoNames named some provinces in English or by an older form (`Florence`,
/// `Roma Capitale`, `Provincia autonoma di Trento`): rewrite every stored
/// single value to the product name. Joined values (`A | B`) stay as they are.
pub(crate) fn migrate_canonical_province_names(db: &Connection) -> Result<(), String> {
    for table in ["contacts", "search_leads", "search_seeds"] {
        let mut statement = db
            .prepare(&format!(
                "SELECT DISTINCT provincia FROM {table}
                 WHERE provincia IS NOT NULL AND provincia NOT LIKE '%|%'"
            ))
            .map_err(|error| error.to_string())?;
        let values: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|error| error.to_string())?;
        for value in values {
            let canonical = crate::gazetteer::place_display_name("province", &value);
            let known = crate::parsing::PROVINCE_REGIONS
                .iter()
                .any(|(province, _)| *province == canonical);
            if known && canonical != value {
                db.execute(
                    &format!("UPDATE {table} SET provincia=?1 WHERE provincia=?2"),
                    params![canonical, value],
                )
                .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

/// Contact-side triggers for `contacts.quality_score`. They fire on insert and on
/// the columns `contact_score_sql` reads, and only write when the stored score is
/// stale, so a record whose score is unchanged is not touched.
pub(crate) fn contact_score_triggers_sql() -> String {
    let score = crate::contacts::contact_score_sql("contacts");
    let new_score = crate::contacts::contact_score_sql("new");
    let inputs = crate::contacts::CONTACT_SCORE_INPUTS;
    format!(
        "CREATE TRIGGER IF NOT EXISTS contacts_score_ai AFTER INSERT ON contacts
         WHEN new.quality_score IS NOT ({new_score}) BEGIN
             UPDATE contacts SET quality_score=({score}) WHERE rowid=new.rowid;
         END;
         CREATE TRIGGER IF NOT EXISTS contacts_score_au AFTER UPDATE OF {inputs} ON contacts
         WHEN new.quality_score IS NOT ({new_score}) BEGIN
             UPDATE contacts SET quality_score=({score}) WHERE rowid=new.rowid;
         END;"
    )
}

/// Scores the contacts inserted while the triggers were suspended (bulk import).
pub(crate) fn score_unscored_contacts(db: &Connection) -> Result<usize, String> {
    let score = crate::contacts::contact_score_sql("contacts");
    db.execute(
        &format!("UPDATE contacts SET quality_score=({score}) WHERE quality_score IS NULL"),
        [],
    )
    .map_err(|error| error.to_string())
}

/// Rescores every contact whose stored score differs from the current one
/// (freshness moves with time); unchanged records are not written.
pub(crate) fn refresh_stale_scores(db: &Connection) -> Result<usize, String> {
    let score = crate::contacts::contact_score_sql("contacts");
    db.execute(
        &format!(
            "UPDATE contacts SET quality_score=({score}) WHERE quality_score IS NOT ({score})"
        ),
        [],
    )
    .map_err(|error| error.to_string())
}

const SCORE_REFRESH_INTERVAL_SECONDS: i64 = 86_400;

/// Runs `refresh_stale_scores` at most once a day. Returns the number of
/// rescored contacts, or `None` when the last refresh is recent.
pub(crate) fn refresh_scores_if_due(db: &Connection, now: i64) -> Result<Option<usize>, String> {
    let last: Option<i64> = db
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_settings WHERE key='score_refreshed_at'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    if last.is_some_and(|last| now - last < SCORE_REFRESH_INTERVAL_SECONDS) {
        return Ok(None);
    }
    let changed = refresh_stale_scores(db)?;
    db.execute(
        "INSERT INTO app_settings(key,value,updated_at) VALUES('score_refreshed_at',?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![now.to_string(), chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| error.to_string())?;
    Ok(Some(changed))
}

/// Migration 60: one stored contact score, maintained by triggers, replaces the
/// per-query lead score and the Cerca lead sandbox score. `search_leads` loses
/// `score` and `factors`: leads are scored when promoted, as contacts.
pub(crate) fn migrate_contact_quality_score(db: &Connection) -> Result<(), String> {
    if !table_columns(db, "contacts")?
        .iter()
        .any(|column| column == "quality_score")
    {
        db.execute_batch("ALTER TABLE contacts ADD COLUMN quality_score INTEGER;")
            .map_err(|error| error.to_string())?;
    }
    db.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS contacts_score_ai;
         DROP TRIGGER IF EXISTS contacts_score_au;
         {}",
        contact_score_triggers_sql()
    ))
    .map_err(|error| error.to_string())?;
    refresh_scores_if_due(db, chrono::Utc::now().timestamp())?;

    db.execute_batch("DROP INDEX IF EXISTS idx_search_leads_run_score;")
        .map_err(|error| error.to_string())?;
    let lead_columns = table_columns(db, "search_leads")?;
    for column in ["score", "factors"] {
        if lead_columns.iter().any(|existing| existing == column) {
            db.execute(
                &format!("ALTER TABLE search_leads DROP COLUMN {column}"),
                [],
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Migration 65: "Aggiungi al database" used to promote every lead as
/// `domain_unverified`, and the merge keeps that state over a known one. Contacts
/// that were already routable lost `valid` (and the Contatti list, which shows
/// exportable rows only); new ones never got it. A contact whose domain routes
/// (its own `routing_ok`, or a `routing_ok` cache entry for a never-checked one)
/// is `valid` again. The exportable triggers refresh `is_exportable`.
pub(crate) fn migrate_promoted_contact_status(db: &Connection) -> Result<(), String> {
    db.execute(
        "UPDATE contacts SET status='valid', routing_status='routing_ok'
         WHERE status='domain_unverified' AND (
             routing_status='routing_ok'
             OR (coalesce(routing_status,'unknown')='unknown' AND EXISTS (
                 SELECT 1 FROM domain_verification_cache d
                 WHERE d.domain=lower(substr(contacts.email,instr(contacts.email,'@')+1))
                   AND d.routing_status='routing_ok')))",
        [],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Migration 64: the score measures data richness and classification specificity
/// instead of source count. Recreates the triggers and rescores every contact
/// whose score changes.
pub(crate) fn migrate_contact_score_richness(db: &Connection) -> Result<(), String> {
    migrate_contact_score_without_confidence(db)
}

/// Migration 63: the score no longer reads `classification_confidence` (a fixed
/// value per classification method). Recreates the triggers without it and
/// rescores the contacts whose score changes.
pub(crate) fn migrate_contact_score_without_confidence(db: &Connection) -> Result<(), String> {
    db.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS contacts_score_ai;
         DROP TRIGGER IF EXISTS contacts_score_au;
         {}",
        contact_score_triggers_sql()
    ))
    .map_err(|error| error.to_string())?;
    refresh_stale_scores(db).map(|_| ())
}

/// Migration 62: the classification column sorts by method label, not by the
/// confidence stored per method. Replaces the index migration 61 built for it.
pub(crate) fn migrate_classification_sort_by_method(db: &Connection) -> Result<(), String> {
    db.execute_batch("DROP INDEX IF EXISTS idx_contacts_sort_classification;")
        .map_err(|error| error.to_string())?;
    migrate_contact_sort_indexes(db)
}

/// Migration 61: one partial index per sortable column of the contacts list, so a
/// click on a column header reads an index instead of sorting every contact.
/// Each index is `(sort key, lower(email), id)` over the exportable rows; the keys
/// come from `contacts::CONTACT_SORT_COLUMNS`, which the list query also uses.
pub(crate) fn migrate_contact_sort_indexes(db: &Connection) -> Result<(), String> {
    for (key, kind) in &crate::contacts::CONTACT_SORT_COLUMNS {
        let expression = match kind {
            crate::contacts::ContactSortKind::Text(column) => {
                crate::contacts::text_sort_sql(column, "")
            }
            crate::contacts::ContactSortKind::Number(column) => column.replace('@', ""),
        };
        db.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS idx_contacts_sort_{key}
                 ON contacts({expression}, lower(email), id) WHERE is_exportable=1;"
        ))
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}
