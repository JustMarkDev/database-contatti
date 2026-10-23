use crate::{
    ContactList, ContactRecord, ContactView, EmailStatus, FilterOption, ImportContact, ListFilters,
    MAX_PAGE_SIZE,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(crate) fn suppress_non_contact_mailbox(
    db: &Connection,
    comparison_key: &str,
    canonical_email: &str,
    source_file: &str,
    created_at: &str,
) -> Result<(), String> {
    let key = comparison_key.trim().to_lowercase();
    db.execute(
        "INSERT INTO suppressions(
            comparison_key,canonical_email,reason,source_file,source_authority,created_at,lifted_at,note
         ) VALUES (?1,?2,'non_contact_mailbox',?3,'automatic_import',?4,NULL,
                   'Automated sender/bounce mailbox; retained only as candidate evidence')
         ON CONFLICT(comparison_key) DO UPDATE SET
            canonical_email=excluded.canonical_email,
            reason='non_contact_mailbox',
            source_file=excluded.source_file,
            source_authority='automatic_import',
            lifted_at=NULL,
            note=excluded.note",
        params![key, canonical_email, source_file, created_at],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE contacts SET suppression_status='suppressed'
         WHERE email_comparison_key=?1 COLLATE NOCASE OR email=?2 COLLATE NOCASE",
        params![key, canonical_email],
    )
    .map_err(|error| error.to_string())?;
    let mut stmt = db
        .prepare("SELECT id FROM email_candidates WHERE comparison_key=?1 COLLATE NOCASE AND current_disposition<>'suppressed'")
        .map_err(|error| error.to_string())?;
    let candidate_ids = stmt
        .query_map(params![key], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(stmt);
    for candidate_id in candidate_ids {
        db.execute(
            "UPDATE candidate_dispositions SET is_current=0 WHERE candidate_id=?1 AND is_current=1",
            params![candidate_id],
        )
        .map_err(|error| error.to_string())?;
        db.execute("UPDATE email_candidates SET contact_id=NULL,status='suppressed',candidate_status='suppressed',current_disposition='suppressed',disposition_reason='non_contact_mailbox',outreach_eligible=0 WHERE id=?1",params![candidate_id]).map_err(|error|error.to_string())?;
        db.execute("INSERT INTO candidate_dispositions(id,candidate_id,disposition,reason_code,is_current,created_at) VALUES (?1,?2,'suppressed','non_contact_mailbox',1,?3)",params![uuid::Uuid::new_v4().to_string(),candidate_id,created_at]).map_err(|error|error.to_string())?;
    }
    Ok(())
}

pub(crate) struct SaveContactResult {
    pub(crate) contact_id: String,
    pub(crate) is_new: bool,
    pub(crate) existing_before_import: bool,
    #[allow(dead_code)]
    pub(crate) source_added: bool,
}

#[derive(Default)]
pub(crate) struct ContactImportCache {
    records: HashMap<String, (ContactRecord, bool)>,
    dirty: HashSet<String>,
    pending_sources: Vec<PendingContactSource>,
    /// Cached once per import: whether any legacy rows lack email_comparison_key.
    legacy_lookup_needed: Option<bool>,
}

struct PendingContactSource {
    contact_id: String,
    import_id: Option<String>,
    source_file: std::sync::Arc<str>,
    source_sheet: Option<String>,
    source_row: i64,
    raw_context: std::sync::Arc<str>,
    source_url: Option<String>,
    source_row_id: Option<String>,
}

#[allow(dead_code)]
pub(crate) fn save_contact(
    db: &Connection,
    contact: &ImportContact,
    extra_category: &str,
    import_id: Option<&str>,
    source_row_id: Option<&str>,
    seen_at: &str,
) -> Result<(SaveContactResult, ContactRecord), String> {
    let category = standard_category([contact.category.as_str(), extra_category].into_iter());
    let mut merged = contact_record(contact, category, new_contact_id());

    let mut stmt = db
        .prepare_cached(
            "SELECT id, email, email_local_part, email_domain, email_tld, status, validation_notes,
                source_file, category, subcategory, contact_kind, paese, is_pec, comune, provincia, regione, website,
                source_url, category_id, subcategory_id, classification_method,
                classification_confidence, classification_evidence, needs_review,
                classification_hash, macroarea, taxonomy_version_id,
                (SELECT semantic_version FROM taxonomy_versions WHERE id=contacts.taxonomy_version_id),
                classification_state, classification_state_reason
            FROM contacts
             WHERE email_comparison_key=?1",
        )
        .map_err(|err| err.to_string())?;
    let existing_rows = stmt
        .query_map(params![contact.comparison_key], row_to_contact)
        .map_err(|err| err.to_string())?;
    let mut had_existing = false;
    for existing in existing_rows {
        had_existing = true;
        merged = merge_contact_records(existing.map_err(|err| err.to_string())?, merged);
    }
    drop(stmt);

    if !had_existing {
        let mut legacy_stmt = db
            .prepare_cached(
                "SELECT id, email, email_local_part, email_domain, email_tld, status, validation_notes,
                    source_file, category, subcategory, contact_kind, paese, is_pec, comune, provincia, regione, website,
                    source_url, category_id, subcategory_id, classification_method,
                    classification_confidence, classification_evidence, needs_review,
                    classification_hash, macroarea, taxonomy_version_id,
                    (SELECT semantic_version FROM taxonomy_versions WHERE id=contacts.taxonomy_version_id),
                    classification_state, classification_state_reason
                 FROM contacts
                 WHERE email_comparison_key IS NULL AND email=?1 COLLATE NOCASE",
            )
            .map_err(|err| err.to_string())?;
        let legacy_rows = legacy_stmt
            .query_map(params![contact.normalized_email], row_to_contact)
            .map_err(|err| err.to_string())?;
        for existing in legacy_rows {
            had_existing = true;
            merged = merge_contact_records(existing.map_err(|err| err.to_string())?, merged);
        }
    }

    if had_existing {
        update_contact_record(db, &merged)?;
    } else {
        insert_contact_record(db, &merged)?;
    }

    let _ = db.execute(
        "UPDATE contacts SET raw_email=COALESCE(raw_email,?1),
             email_comparison_key=COALESCE(email_comparison_key,?2),
             normalization_method=COALESCE(NULLIF(normalization_method,'legacy'),?3),
             routing_status=COALESCE(routing_status,?4),
             mailbox_status=COALESCE(mailbox_status,'unknown'),
             suppression_status='active',
             first_seen_at=COALESCE(first_seen_at,?5),last_seen_at=?5
         WHERE id=?6",
        params![
            contact.raw_email,
            contact.comparison_key,
            contact.normalization_method,
            if matches!(contact.status, EmailStatus::Valid) {
                "routing_ok"
            } else {
                "unknown"
            },
            seen_at,
            merged.id,
        ],
    );
    let source_added = insert_contact_source(db, &merged.id, contact, import_id, source_row_id)?;
    Ok((
        SaveContactResult {
            contact_id: merged.id.clone(),
            is_new: !had_existing,
            existing_before_import: had_existing,
            source_added,
        },
        merged,
    ))
}

fn queue_contact_source(
    cache: &mut ContactImportCache,
    contact_id: &str,
    contact: &ImportContact,
    import_id: Option<&str>,
    source_row_id: Option<&str>,
) {
    cache.pending_sources.push(PendingContactSource {
        contact_id: contact_id.to_owned(),
        import_id: import_id.map(str::to_owned),
        source_file: contact.source_file.clone(),
        source_sheet: contact.source_sheet.clone(),
        source_row: contact.source_row as i64,
        raw_context: contact.raw_context.clone(),
        source_url: contact.source_url.clone(),
        source_row_id: source_row_id.map(str::to_owned),
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn save_contact_for_import(
    db: &Connection,
    contact: &ImportContact,
    extra_category: &str,
    import_id: Option<&str>,
    source_row_id: Option<&str>,
    seen_at: &str,
    cache: &mut ContactImportCache,
) -> Result<SaveContactResult, String> {
    if let Some((existing, existing_before_import)) = cache.records.get_mut(&contact.comparison_key)
    {
        merge_import_contact_into(existing, contact, extra_category);
        let contact_id = existing.id.clone();
        let existing_before_import = *existing_before_import;
        queue_contact_source(cache, &contact_id, contact, import_id, source_row_id);
        cache.dirty.insert(contact.comparison_key.clone());
        return Ok(SaveContactResult {
            contact_id,
            is_new: false,
            existing_before_import,
            source_added: true,
        });
    }

    let category = standard_category([contact.category.as_str(), extra_category].into_iter());
    let mut merged = contact_record(contact, category, new_contact_id());

    let mut stmt = db
        .prepare_cached(
            "SELECT id, email, email_local_part, email_domain, email_tld, status, validation_notes,
                source_file, category, subcategory, contact_kind, paese, is_pec, comune, provincia, regione, website,
                source_url, category_id, subcategory_id, classification_method,
                classification_confidence, classification_evidence, needs_review,
                classification_hash, macroarea, taxonomy_version_id,
                NULL,
                classification_state, classification_state_reason
            FROM contacts
             WHERE email_comparison_key=?1",
        )
        .map_err(|err| err.to_string())?;
    let existing_rows = stmt
        .query_map(params![contact.comparison_key], row_to_contact)
        .map_err(|err| err.to_string())?;
    let mut had_existing = false;
    for existing in existing_rows {
        had_existing = true;
        merged = merge_contact_records(existing.map_err(|err| err.to_string())?, merged);
    }
    drop(stmt);

    if !had_existing {
        let needs_legacy = match cache.legacy_lookup_needed {
            Some(needed) => needed,
            None => {
                let needed: bool = db
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM contacts WHERE email_comparison_key IS NULL)",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or(false);
                cache.legacy_lookup_needed = Some(needed);
                needed
            }
        };
        if needs_legacy {
            let mut legacy_stmt = db
                .prepare_cached(
                    "SELECT id, email, email_local_part, email_domain, email_tld, status, validation_notes,
                        source_file, category, subcategory, contact_kind, paese, is_pec, comune, provincia, regione, website,
                        source_url, category_id, subcategory_id, classification_method,
                        classification_confidence, classification_evidence, needs_review,
                        classification_hash, macroarea, taxonomy_version_id,
                        NULL,
                        classification_state, classification_state_reason
                     FROM contacts
                     WHERE email_comparison_key IS NULL AND email=?1 COLLATE NOCASE",
                )
                .map_err(|err| err.to_string())?;
            let legacy_rows = legacy_stmt
                .query_map(params![contact.normalized_email], row_to_contact)
                .map_err(|err| err.to_string())?;
            for existing in legacy_rows {
                had_existing = true;
                merged = merge_contact_records(existing.map_err(|err| err.to_string())?, merged);
            }
        }
    }

    if had_existing {
        update_contact_record(db, &merged)?;
        let _ = db.execute(
            "UPDATE contacts SET raw_email=COALESCE(raw_email,?1),
                 email_comparison_key=COALESCE(email_comparison_key,?2),
                 normalization_method=COALESCE(NULLIF(normalization_method,'legacy'),?3),
                 routing_status=COALESCE(routing_status,?4),
                 mailbox_status=COALESCE(mailbox_status,'unknown'),
                 suppression_status='active',
                 first_seen_at=COALESCE(first_seen_at,?5),last_seen_at=?5
             WHERE id=?6",
            params![
                contact.raw_email,
                contact.comparison_key,
                contact.normalization_method,
                if matches!(contact.status, EmailStatus::Valid) {
                    "routing_ok"
                } else {
                    "unknown"
                },
                seen_at,
                merged.id,
            ],
        );
    } else {
        insert_contact_record_for_import(db, &merged, contact, seen_at)?;
    }
    queue_contact_source(cache, &merged.id, contact, import_id, source_row_id);
    let outcome = SaveContactResult {
        contact_id: merged.id.clone(),
        is_new: !had_existing,
        existing_before_import: had_existing,
        source_added: true,
    };
    cache.records.insert(
        contact.comparison_key.clone(),
        (merged, outcome.existing_before_import),
    );
    Ok(outcome)
}

pub(crate) fn flush_contact_import_cache(
    db: &Connection,
    cache: &mut ContactImportCache,
    seen_at: &str,
) -> Result<usize, String> {
    let mut touch = db
        .prepare_cached(
            "UPDATE contacts SET suppression_status='active',last_seen_at=?1 WHERE id=?2",
        )
        .map_err(|error| error.to_string())?;
    for key in &cache.dirty {
        let Some((record, _)) = cache.records.get(key) else {
            continue;
        };
        update_contact_record(db, record)?;
        touch
            .execute(params![seen_at, record.id])
            .map_err(|error| error.to_string())?;
    }
    flush_pending_contact_sources(db, cache)
}

fn flush_pending_contact_sources(
    db: &Connection,
    cache: &mut ContactImportCache,
) -> Result<usize, String> {
    if cache.pending_sources.is_empty() {
        return Ok(0);
    }
    let mut insert = db
        .prepare_cached(
            "INSERT OR IGNORE INTO contact_sources(id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,source_url,source_row_id)
             VALUES (lower(hex(randomblob(16))),?1,?2,?3,?4,?5,?6,?7,?8)",
        )
        .map_err(|err| err.to_string())?;
    let mut inserted = 0usize;
    for source in cache.pending_sources.drain(..) {
        inserted += insert
            .execute(params![
                source.contact_id,
                source.import_id,
                source.source_file.as_ref(),
                source.source_sheet,
                source.source_row,
                source.raw_context.as_ref(),
                source.source_url,
                source.source_row_id,
            ])
            .map_err(|err| err.to_string())?;
    }
    Ok(inserted)
}

/// Bulk imports drop the per-row contact triggers and rebuild their effects once
/// at the end, inside the same transaction: the FTS index, the exportable flag and
/// the score of the new contacts.
pub(crate) fn suspend_contact_triggers(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "DROP TRIGGER IF EXISTS contacts_fts_ai;
         DROP TRIGGER IF EXISTS contacts_fts_ad;
         DROP TRIGGER IF EXISTS contacts_fts_au;
         DROP TRIGGER IF EXISTS contacts_exportable_ai;
         DROP TRIGGER IF EXISTS contacts_exportable_au;
         DROP TRIGGER IF EXISTS contacts_score_ai;
         DROP TRIGGER IF EXISTS contacts_score_au;",
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn resume_contact_triggers(db: &Connection) -> Result<(), String> {
    if crate::has_table(db, "contacts_fts")? {
        db.execute_batch(&format!(
            "{}\nINSERT INTO contacts_fts(contacts_fts) VALUES('rebuild');",
            crate::schema::CONTACTS_FTS_TRIGGERS
        ))
        .map_err(|error| error.to_string())?;
    }
    if crate::schema::table_columns(db, "contacts")?
        .iter()
        .any(|column| column == "is_exportable")
    {
        db.execute_batch(&crate::schema::contact_exportable_triggers_sql())
            .map_err(|error| error.to_string())?;
        crate::schema::refresh_stale_exportable_flags(db)?;
    }
    if crate::schema::table_columns(db, "contacts")?
        .iter()
        .any(|column| column == "quality_score")
    {
        db.execute_batch(&crate::schema::contact_score_triggers_sql())
            .map_err(|error| error.to_string())?;
        crate::schema::score_unscored_contacts(db)?;
    }
    Ok(())
}

fn contact_record(contact: &ImportContact, category: String, id: String) -> ContactRecord {
    let macroarea = contact.macroarea.clone().or_else(|| {
        contact
            .regione
            .as_deref()
            .and_then(crate::ai_enrichment::macroarea_from_regione)
            .map(String::from)
    });
    let classification_state = if contact.contact_kind == "personale" {
        "not_applicable".to_owned()
    } else if matches!(
        category.trim().to_ascii_lowercase().as_str(),
        "" | "varie" | "unknown" | "sconosciuto"
    ) {
        "unprocessed".to_owned()
    } else {
        "classified".to_owned()
    };
    ContactRecord {
        id,
        email: contact.normalized_email.clone(),
        email_local_part: contact.email_local_part.clone(),
        email_domain: contact.email_domain.clone(),
        email_tld: contact.email_tld.clone(),
        status: contact.status,
        validation_notes: contact.validation_notes.clone(),
        source_file: contact.source_file.to_string(),
        category,
        subcategory: contact.subcategory.clone(),
        contact_kind: contact.contact_kind.clone(),
        paese: contact.paese.clone(),
        is_pec: contact.is_pec,
        comune: contact.comune.clone(),
        provincia: contact.provincia.clone(),
        regione: contact.regione.clone(),
        macroarea,
        website: contact.website.clone(),
        source_url: contact.source_url.clone(),
        category_id: None,
        subcategory_id: None,
        classification_method: contact.classification_method.clone(),
        classification_confidence: contact.classification_confidence,
        classification_evidence: contact.classification_evidence.clone(),
        needs_review: contact.subcategory.as_deref() == Some("Dato misto o da revisionare")
            || contact
                .classification_evidence
                .iter()
                .any(|value| value.contains("non attribuibile")),
        classification_hash: None,
        taxonomy_version_id: None,
        taxonomy_version: None,
        classification_state,
        classification_state_reason: None,
    }
}

#[allow(dead_code)]
fn insert_contact_source(
    db: &Connection,
    contact_id: &str,
    contact: &ImportContact,
    import_id: Option<&str>,
    source_row_id: Option<&str>,
) -> Result<bool, String> {
    db.prepare_cached(
        "INSERT OR IGNORE INTO contact_sources(id,contact_id,import_id,source_file,source_sheet,source_row,raw_context,source_url,source_row_id)
         VALUES (lower(hex(randomblob(16))),?1,?2,?3,?4,?5,?6,?7,?8)",
    ).map_err(|err| err.to_string())?
    .execute(params![
        contact_id,
        import_id,
        contact.source_file.as_ref(),
        contact.source_sheet,
        contact.source_row as i64,
        contact.raw_context.as_ref(),
        contact.source_url,
        source_row_id
    ])
    .map(|changed| changed == 1)
    .map_err(|err| err.to_string())
}

pub(crate) fn update_contact_record(
    db: &Connection,
    contact: &ContactRecord,
) -> Result<(), String> {
    db.prepare_cached(
        "UPDATE contacts SET email=?1,email_local_part=?2,email_domain=?3,email_tld=?4,status=?5,
         validation_notes=?6,source_file=?7,category=?8,subcategory=?9,contact_kind=?10,paese=?11,
         is_pec=?12,comune=?13,provincia=?14,regione=?15,website=?16,source_url=?17,
         classification_method=?18,classification_confidence=?19,needs_review=?20,
         classification_evidence=?21,classification_state=?22,classification_state_reason=?23,
         category_id=(SELECT id FROM categories WHERE name=?8),
         subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name=?8 AND s.name=?9)
         WHERE id=?24",
    ).map_err(|e| e.to_string())?
    .execute(
        params![
            contact.email, contact.email_local_part, contact.email_domain, contact.email_tld,
            contact.status.as_str(), serde_json::to_string(&contact.validation_notes).map_err(|e| e.to_string())?,
            contact.source_file, contact.category, contact.subcategory, contact.contact_kind, contact.paese,
            i64::from(contact.is_pec), contact.comune, contact.provincia, contact.regione, contact.website,
            contact.source_url, contact.classification_method, contact.classification_confidence,
            contact.needs_review,
            serde_json::to_string(&contact.classification_evidence).map_err(|error| error.to_string())?,
            contact.classification_state, contact.classification_state_reason,
            contact.id,
        ],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

/// Persisted id/hash of a string (claim ids, classification hashes). SipHash-1-3
/// with zero keys over the bytes plus a 0xff terminator: the exact output
/// `DefaultHasher` gave for `str`, pinned here because std does not promise that
/// algorithm across Rust releases and stored values must not change.
pub(crate) fn simple_hash(value: &str) -> String {
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }
    let mut message = value.as_bytes().to_vec();
    message.push(0xff);
    let mut v = [
        0x736f_6d65_7073_6575u64,
        0x646f_7261_6e64_6f6d,
        0x6c79_6765_6e65_7261,
        0x7465_6462_7974_6573,
    ];
    let (chunks, tail) = message.as_chunks::<8>();
    for chunk in chunks {
        let word = u64::from_le_bytes(*chunk);
        v[3] ^= word;
        round(&mut v);
        v[0] ^= word;
    }
    let mut last = [0u8; 8];
    last[..tail.len()].copy_from_slice(tail);
    let last = u64::from_le_bytes(last) | ((message.len() as u64) << 56);
    v[3] ^= last;
    round(&mut v);
    v[0] ^= last;
    v[2] ^= 0xff;
    for _ in 0..3 {
        round(&mut v);
    }
    format!("{:016x}", v[0] ^ v[1] ^ v[2] ^ v[3])
}

pub(crate) fn insert_contact_record(
    db: &Connection,
    contact: &ContactRecord,
) -> Result<(), String> {
    db.prepare_cached(
        "INSERT INTO contacts (
            id, email, email_local_part, email_domain, email_tld, status, validation_notes,
            source_file, category, subcategory, contact_kind, paese, is_pec, comune, provincia, regione, website,
            source_url, classification_method, classification_confidence, classification_evidence,
            needs_review, category_id, subcategory_id,
            classification_state, classification_state_reason
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                  ?16, ?17, ?18, ?19, ?20, ?21, ?22,
                  (SELECT id FROM categories WHERE name=?9),
                  (SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name=?9 AND lower(s.name)=lower(?10)),
                  ?23, ?24)",
    ).map_err(|err| err.to_string())?
    .execute(
        params![
            contact.id,
            contact.email,
            contact.email_local_part,
            contact.email_domain,
            contact.email_tld,
            contact.status.as_str(),
            serde_json::to_string(&contact.validation_notes).map_err(|err| err.to_string())?,
            contact.source_file,
            contact.category,
            contact.subcategory,
            contact.contact_kind,
            contact.paese,
            i64::from(contact.is_pec),
            contact.comune,
            contact.provincia,
            contact.regione,
            contact.website,
            contact.source_url,
            contact.classification_method,
            contact.classification_confidence,
            serde_json::to_string(&contact.classification_evidence)
                .map_err(|err| err.to_string())?,
            contact.needs_review,
            contact.classification_state,
            contact.classification_state_reason,
        ],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn insert_contact_record_for_import(
    db: &Connection,
    contact: &ContactRecord,
    imported: &ImportContact,
    seen_at: &str,
) -> Result<(), String> {
    let routing = if matches!(imported.status, EmailStatus::Valid) {
        "routing_ok"
    } else {
        "unknown"
    };
    let notes_json = if contact.validation_notes.is_empty() {
        "[]".to_owned()
    } else {
        serde_json::to_string(&contact.validation_notes).map_err(|err| err.to_string())?
    };
    let evidence_json = if contact.classification_evidence.is_empty() {
        "[]".to_owned()
    } else {
        serde_json::to_string(&contact.classification_evidence).map_err(|err| err.to_string())?
    };
    db.prepare_cached(
        "INSERT INTO contacts (
            id, email, email_local_part, email_domain, email_tld, status, validation_notes,
            source_file, category, subcategory, contact_kind, paese, is_pec, comune, provincia, regione, website,
            source_url, classification_method, classification_confidence, classification_evidence,
            needs_review, category_id, subcategory_id,
            classification_state, classification_state_reason,
            raw_email, email_comparison_key, normalization_method, routing_status, mailbox_status,
            suppression_status, first_seen_at, last_seen_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                  ?16, ?17, ?18, ?19, ?20, ?21, ?22,
                  (SELECT id FROM categories WHERE name=?9),
                  (SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name=?9 AND lower(s.name)=lower(?10)),
                  ?23, ?24,
                  ?25, ?26, ?27, ?28, 'unknown', 'active', ?29, ?29)",
    )
    .map_err(|err| err.to_string())?
    .execute(params![
        contact.id,
        contact.email,
        contact.email_local_part,
        contact.email_domain,
        contact.email_tld,
        contact.status.as_str(),
        notes_json,
        contact.source_file,
        contact.category,
        contact.subcategory,
        contact.contact_kind,
        contact.paese,
        i64::from(contact.is_pec),
        contact.comune,
        contact.provincia,
        contact.regione,
        contact.website,
        contact.source_url,
        contact.classification_method,
        contact.classification_confidence,
        evidence_json,
        contact.needs_review,
        contact.classification_state,
        contact.classification_state_reason,
        imported.raw_email,
        imported.comparison_key,
        imported.normalization_method,
        routing,
        seen_at,
    ])
    .map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn recompute_primary_classification(
    db: &Connection,
    contact_id: &str,
) -> Result<(), String> {
    let claim: Option<(String, Option<String>, f64, String)> = db
        .query_row(
            "SELECT category,subcategory,confidence,method
             FROM classification_claims
             WHERE contact_id=?1
             ORDER BY CASE WHEN lower(category)='varie' THEN 1 ELSE 0 END,
                 confidence DESC,
                 lower(category), lower(coalesce(subcategory,'')), id
             LIMIT 1",
            params![contact_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some((category, subcategory, confidence, method)) = claim {
        db.execute(
            "UPDATE contacts SET category=?1,subcategory=?2,
                 classification_confidence=?3,classification_method=?4,
                 category_id=(SELECT id FROM categories WHERE name=?1),
                 subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id WHERE c.name=?1 AND lower(s.name)=lower(?2))
             WHERE id=?5 AND lower(classification_method) NOT IN ('ai','deterministic')",
            params![category, subcategory, confidence, method, contact_id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// A classification claim about a candidate, contact or source row.
pub(crate) struct ClassificationClaim<'a> {
    pub(crate) candidate_id: Option<&'a str>,
    pub(crate) contact_id: Option<&'a str>,
    pub(crate) source_row_id: Option<&'a str>,
    pub(crate) category: &'a str,
    pub(crate) subcategory: Option<&'a str>,
    pub(crate) confidence: f64,
    pub(crate) method: &'a str,
    pub(crate) evidence: &'a [String],
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn insert_classification_claim(
    db: &Connection,
    claim: ClassificationClaim<'_>,
) -> Result<(), String> {
    let contact_id = claim.contact_id;
    insert_classification_claim_deferred(db, &chrono::Utc::now().to_rfc3339(), claim)?;
    if let Some(contact_id) = contact_id {
        recompute_primary_classification(db, contact_id)?;
    }
    Ok(())
}

pub(crate) fn insert_classification_claim_deferred(
    db: &Connection,
    created_at: &str,
    claim: ClassificationClaim<'_>,
) -> Result<(), String> {
    let ClassificationClaim {
        candidate_id,
        contact_id,
        source_row_id,
        category,
        subcategory,
        confidence,
        method,
        evidence,
    } = claim;
    let unique = format!(
        "{}|{}|{}|{}|{}",
        contact_id.unwrap_or_default(),
        source_row_id.unwrap_or_default(),
        category,
        subcategory.unwrap_or_default(),
        candidate_id.unwrap_or_default()
    );
    let evidence_json = if evidence.is_empty() {
        "[]".to_owned()
    } else {
        serde_json::to_string(evidence).map_err(|e| e.to_string())?
    };
    db.prepare_cached(
        "INSERT OR IGNORE INTO classification_claims
         (id,candidate_id,contact_id,source_row_id,category,subcategory,confidence,method,evidence_json,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )
    .map_err(|e| e.to_string())?
    .execute(params![
            simple_hash(&unique),
            candidate_id,
            contact_id,
            source_row_id,
            category,
            subcategory,
            confidence.clamp(0.0, 1.0),
            method,
            evidence_json,
            created_at,
        ])
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Picks the best source claim for contacts touched by an import. AI and
/// deterministic results are not claims, so they outrank claims and stay as they are.
pub(crate) fn recompute_imported_classifications(
    db: &Connection,
    import_id: &str,
) -> Result<(), String> {
    db.execute(
        "WITH touched AS (
             SELECT DISTINCT cc.contact_id
             FROM classification_claims cc
             JOIN source_rows sr ON sr.id=cc.source_row_id
             WHERE sr.import_id=?1 AND cc.contact_id IS NOT NULL
         ), ranked AS (
             SELECT cc.contact_id,cc.category,cc.subcategory,cc.confidence,cc.method,
                    row_number() OVER (
                      PARTITION BY cc.contact_id
                      ORDER BY CASE WHEN lower(cc.category)='varie' THEN 1 ELSE 0 END,
                        cc.confidence DESC,lower(cc.category),
                        lower(coalesce(cc.subcategory,'')),cc.id
                    ) AS rank
             FROM classification_claims cc
             JOIN touched ON touched.contact_id=cc.contact_id
         ), best AS (
             SELECT contact_id, category, subcategory, confidence, method
             FROM ranked WHERE rank=1
         )
         UPDATE contacts SET
             category=best.category,
             subcategory=best.subcategory,
             classification_confidence=best.confidence,
             classification_method=best.method
         FROM best
         WHERE contacts.id=best.contact_id
           AND lower(contacts.classification_method) NOT IN ('ai','deterministic')",
        params![import_id],
    )
    .map_err(|error| error.to_string())?;
    db.execute(
        "UPDATE contacts SET
           category_id=(SELECT id FROM categories WHERE name=contacts.category),
           subcategory_id=(SELECT s.id FROM subcategories s JOIN categories c ON c.id=s.category_id
                           WHERE c.name=contacts.category AND lower(s.name)=lower(contacts.subcategory))
         WHERE lower(classification_method) NOT IN ('ai','deterministic') AND id IN (
           SELECT DISTINCT cc.contact_id FROM classification_claims cc
           JOIN source_rows sr ON sr.id=cc.source_row_id
           WHERE sr.import_id=?1 AND cc.contact_id IS NOT NULL
         )",
        params![import_id],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn merge_contact_records(
    mut existing: ContactRecord,
    incoming: ContactRecord,
) -> ContactRecord {
    merge_contact_record_into(&mut existing, incoming);
    existing
}

pub(crate) fn merge_import_contact_into(
    existing: &mut ContactRecord,
    contact: &ImportContact,
    extra_category: &str,
) {
    existing.status = merge_status(existing.status, contact.status);
    merge_slice(&mut existing.validation_notes, &contact.validation_notes);
    existing.source_file = merge_text_str(
        std::mem::take(&mut existing.source_file),
        contact.source_file.as_ref(),
    );
    let category = standard_category([contact.category.as_str(), extra_category].into_iter());
    let preserve_inferred_classification =
        classification_authority(&existing.classification_method)
            > classification_authority(&contact.classification_method);
    if !preserve_inferred_classification {
        existing.category = merge_category(std::mem::take(&mut existing.category), category);
        existing.subcategory =
            merge_option_str(existing.subcategory.take(), contact.subcategory.as_deref());
    }
    if existing.contact_kind == "non_determinato" {
        existing.contact_kind = contact.contact_kind.clone();
    }
    if existing.paese == "Italia" && contact.paese == "Svizzera" {
        existing.paese = contact.paese.clone();
    }
    existing.is_pec = existing.is_pec || contact.is_pec;
    if !preserve_inferred_classification {
        existing.comune = merge_option_str(existing.comune.take(), contact.comune.as_deref());
        existing.provincia =
            merge_option_str(existing.provincia.take(), contact.provincia.as_deref());
        existing.regione = merge_option_str(existing.regione.take(), contact.regione.as_deref());
        existing.macroarea =
            merge_option_str(existing.macroarea.take(), contact.macroarea.as_deref()).or_else(
                || {
                    existing
                        .regione
                        .as_deref()
                        .and_then(crate::ai_enrichment::macroarea_from_regione)
                        .map(String::from)
                },
            );
    }
    existing.website = merge_option_str(existing.website.take(), contact.website.as_deref());
    existing.source_url =
        merge_option_str(existing.source_url.take(), contact.source_url.as_deref());
    if classification_authority(&contact.classification_method)
        > classification_authority(&existing.classification_method)
        || (classification_authority(&contact.classification_method)
            == classification_authority(&existing.classification_method)
            && contact.classification_confidence > existing.classification_confidence)
    {
        existing.classification_method = contact.classification_method.clone();
        existing.classification_confidence = contact.classification_confidence;
    }
}

pub(crate) fn merge_contact_record_into(existing: &mut ContactRecord, incoming: ContactRecord) {
    existing.email = incoming.email;
    existing.email_local_part = incoming.email_local_part;
    existing.email_domain = incoming.email_domain;
    existing.email_tld = incoming.email_tld;
    existing.status = merge_status(existing.status, incoming.status);
    merge_vec(&mut existing.validation_notes, incoming.validation_notes);
    existing.source_file = merge_text(
        std::mem::take(&mut existing.source_file),
        incoming.source_file,
    );
    let preserve_inferred_classification =
        classification_authority(&existing.classification_method)
            > classification_authority(&incoming.classification_method);
    if !preserve_inferred_classification {
        existing.category =
            merge_category(std::mem::take(&mut existing.category), incoming.category);
        existing.subcategory = merge_option(existing.subcategory.take(), incoming.subcategory);
    }
    if existing.contact_kind == "non_determinato" {
        existing.contact_kind = incoming.contact_kind;
    }
    if existing.paese == "Italia" && incoming.paese == "Svizzera" {
        existing.paese = incoming.paese;
    }
    existing.is_pec = existing.is_pec || incoming.is_pec;
    if !preserve_inferred_classification {
        existing.comune = merge_option(existing.comune.take(), incoming.comune);
        existing.provincia = merge_option(existing.provincia.take(), incoming.provincia);
        existing.regione = merge_option(existing.regione.take(), incoming.regione);
        existing.macroarea =
            merge_option(existing.macroarea.take(), incoming.macroarea).or_else(|| {
                existing
                    .regione
                    .as_deref()
                    .and_then(crate::ai_enrichment::macroarea_from_regione)
                    .map(String::from)
            });
    }
    existing.website = merge_option(existing.website.take(), incoming.website);
    existing.source_url = merge_option(existing.source_url.take(), incoming.source_url);
    if classification_authority(&incoming.classification_method)
        > classification_authority(&existing.classification_method)
        || (classification_authority(&incoming.classification_method)
            == classification_authority(&existing.classification_method)
            && incoming.classification_confidence > existing.classification_confidence)
    {
        existing.classification_method = incoming.classification_method;
        existing.classification_confidence = incoming.classification_confidence;
    }
}

fn classification_authority(method: &str) -> u8 {
    match method.trim().to_ascii_lowercase().as_str() {
        "ai" => 3,
        "deterministic" => 2,
        _ => 1,
    }
}

pub(crate) fn merge_status(existing: EmailStatus, incoming: EmailStatus) -> EmailStatus {
    match (existing, incoming) {
        (EmailStatus::Blacklisted, _) | (_, EmailStatus::Blacklisted) => EmailStatus::Blacklisted,
        (EmailStatus::InvalidSyntax, _) | (_, EmailStatus::InvalidSyntax) => {
            EmailStatus::InvalidSyntax
        }
        (EmailStatus::DomainMissingMx, _) | (_, EmailStatus::DomainMissingMx) => {
            EmailStatus::DomainMissingMx
        }
        (EmailStatus::DomainUnverified, _) | (_, EmailStatus::DomainUnverified) => {
            EmailStatus::DomainUnverified
        }
        (EmailStatus::Valid, EmailStatus::Valid) => EmailStatus::Valid,
    }
}

pub(crate) fn merge_category(existing: String, incoming: String) -> String {
    if existing == "Varie" && incoming != "Varie" {
        incoming
    } else {
        existing
    }
}

pub(crate) fn merge_option(existing: Option<String>, incoming: Option<String>) -> Option<String> {
    match (existing, incoming) {
        (Some(existing), Some(incoming)) => Some(merge_text(existing, incoming)),
        (Some(existing), None) => Some(existing),
        (None, Some(incoming)) => Some(incoming),
        (None, None) => None,
    }
}

pub(crate) fn merge_option_str(existing: Option<String>, incoming: Option<&str>) -> Option<String> {
    match (existing, incoming) {
        (Some(existing), Some(incoming)) => Some(merge_text_str(existing, incoming)),
        (Some(existing), None) => Some(existing),
        (None, Some(incoming)) => Some(incoming.to_owned()),
        (None, None) => None,
    }
}

pub(crate) fn merge_vec(existing: &mut Vec<String>, incoming: Vec<String>) {
    merge_slice(existing, &incoming);
}

pub(crate) fn merge_slice(existing: &mut Vec<String>, incoming: &[String]) {
    let mut added = false;
    for value in incoming {
        if !value.trim().is_empty() && !existing.iter().any(|item| item.eq_ignore_ascii_case(value))
        {
            existing.push(value.clone());
            added = true;
        }
    }
    if added {
        existing.sort_by_key(|value| value.to_lowercase());
    }
}

const MAX_MERGED_TOKENS: usize = 50;

pub(crate) fn merge_text(existing: String, incoming: String) -> String {
    merge_text_str(existing, &incoming)
}

pub(crate) fn merge_text_str(mut existing: String, incoming: &str) -> String {
    let incoming_trimmed = incoming.trim();
    if existing.is_empty() {
        return incoming_trimmed.to_owned();
    }
    if incoming_trimmed.is_empty() || existing.eq_ignore_ascii_case(incoming_trimmed) {
        return existing;
    }

    if !incoming_trimmed.contains('|') {
        let mut count = 0usize;
        for existing_part in existing.split('|').map(str::trim).filter(|p| !p.is_empty()) {
            count += 1;
            if existing_part.eq_ignore_ascii_case(incoming_trimmed) {
                return existing;
            }
        }
        if count < MAX_MERGED_TOKENS {
            existing.push_str(" | ");
            existing.push_str(incoming_trimmed);
        }
        return existing;
    }

    let mut current_count = existing
        .split('|')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .count();
    for incoming_part in incoming_trimmed
        .split('|')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if current_count >= MAX_MERGED_TOKENS {
            break;
        }
        let already_present = existing
            .split('|')
            .map(str::trim)
            .any(|existing_part| existing_part.eq_ignore_ascii_case(incoming_part));
        if !already_present {
            existing.push_str(" | ");
            existing.push_str(incoming_part);
            current_count += 1;
        }
    }
    existing
}

pub(crate) fn split_merged_values(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn load_contacts(
    db: &Connection,
    filters: &ListFilters,
    limit: usize,
    offset: usize,
) -> Result<ContactList, String> {
    load_contacts_page(db, filters, limit, offset, true)
}

/// Text sort key: case-insensitive, with empty values mapped to a value above any
/// text so an ascending index scan lists them last.
const SORT_EMPTY: &str = "char(1114111)";

/// `column` marks each column with `@`, replaced by the table qualifier.
pub(crate) fn text_sort_sql(column: &str, qualifier: &str) -> String {
    let column = column.replace('@', qualifier);
    format!("(coalesce(nullif({column},''),{SORT_EMPTY}) COLLATE NOCASE)")
}

pub(crate) enum ContactSortKind {
    Text(&'static str),
    Number(&'static str),
}

/// Sortable columns of the contacts list, by the key the frontend sends. Migration
/// 61 builds one partial index per entry from this table (`key`, `lower(email)`,
/// `id`), so a sorted page reads an index instead of sorting the whole list.
/// `@` marks a column of `contacts`: the index writes it bare, the query qualifies it.
pub(crate) const CONTACT_SORT_COLUMNS: [(&str, ContactSortKind); 10] = [
    (
        "organization",
        ContactSortKind::Text("coalesce(nullif(@organization_name,''),@denomination)"),
    ),
    ("category", ContactSortKind::Text("@category")),
    ("subcategory", ContactSortKind::Text("@subcategory")),
    ("comune", ContactSortKind::Text("@comune")),
    ("provincia", ContactSortKind::Text("@provincia")),
    ("regione", ContactSortKind::Text("@regione")),
    ("pec", ContactSortKind::Number("@is_pec")),
    (
        "sources",
        ContactSortKind::Number("@independent_source_count"),
    ),
    ("score", ContactSortKind::Number("@quality_score")),
    // The labels of `formatClassificationMethod` (contactFormat.ts), so the order
    // matches what the table shows. The stored confidence is fixed per method.
    (
        "classification",
        ContactSortKind::Text(
            "CASE @classification_method WHEN 'ai' THEN 'IA' WHEN 'deterministic' THEN 'Deterministico' \
             WHEN 'rule' THEN 'Regola' WHEN 'column' THEN 'Colonna' WHEN 'legacy' THEN 'Importazione' \
             WHEN 'revalidated' THEN 'Rivalidazione DNS' WHEN 'taxonomy_default' THEN 'Sottocategoria generica' \
             ELSE @classification_method END",
        ),
    ),
];

/// How a page of the contacts list is read. Every tie falls back to the address
/// and then the id, in the direction of the sort, so pages never reshuffle.
enum ListPlan {
    /// One ordered scan.
    Ordered(String),
    /// Text, descending: the non-empty range backwards, then the empty values by address.
    TextDescending(String),
}

fn contact_list_plan(filters: &ListFilters) -> Result<ListPlan, String> {
    let Some(sort) = filters.sort.as_ref().filter(|sort| !sort.column.is_empty()) else {
        return Ok(ListPlan::Ordered(
            "lower(contacts.email),contacts.id".to_owned(),
        ));
    };
    let descending = sort.direction.eq_ignore_ascii_case("desc");
    let (direction, tail) = if descending {
        ("DESC", "lower(contacts.email) DESC,contacts.id DESC")
    } else {
        ("ASC", "lower(contacts.email),contacts.id")
    };
    if sort.column == "email" {
        return Ok(ListPlan::Ordered(tail.to_owned()));
    }
    let (_, kind) = CONTACT_SORT_COLUMNS
        .iter()
        .find(|(key, _)| *key == sort.column)
        .ok_or_else(|| format!("Ordinamento non supportato: {}", sort.column))?;
    Ok(match kind {
        ContactSortKind::Number(column) => ListPlan::Ordered(format!(
            "{} {direction},{tail}",
            column.replace('@', "contacts.")
        )),
        ContactSortKind::Text(expression) => {
            let key = text_sort_sql(expression, "contacts.");
            if descending {
                ListPlan::TextDescending(key)
            } else {
                ListPlan::Ordered(format!("{key},{tail}"))
            }
        }
    })
}

pub(crate) fn load_contacts_page(
    db: &Connection,
    filters: &ListFilters,
    limit: usize,
    offset: usize,
    include_total: bool,
) -> Result<ContactList, String> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let plan = contact_list_plan(filters)?;
    let (where_sql, values) = filter_where(filters);
    let total = if include_total {
        db.query_row(
            &format!("SELECT COUNT(*) FROM contacts {where_sql}"),
            rusqlite::params_from_iter(values.iter()),
            |row| Ok(row.get::<_, i64>(0)? as usize),
        )
        .map_err(|err| err.to_string())?
    } else {
        0
    };
    let fetch_limit = if include_total {
        limit
    } else {
        limit.saturating_add(1)
    };
    let fetch = |extra_where: &str,
                 order_sql: &str,
                 page_limit: usize,
                 page_offset: usize|
     -> Result<Vec<ContactView>, String> {
        let mut query_values = values.clone();
        query_values.push(page_limit.to_string());
        query_values.push(page_offset.to_string());
        let sql = format!(
            "SELECT contacts.id,contacts.email,contacts.email_local_part,contacts.email_domain,contacts.email_tld,contacts.status,contacts.validation_notes,
         contacts.source_file,contacts.category,contacts.subcategory,contacts.contact_kind,contacts.paese,contacts.is_pec,contacts.comune,contacts.provincia,contacts.regione,contacts.website,
         contacts.source_url,contacts.category_id,contacts.subcategory_id,contacts.classification_method,contacts.classification_confidence,
         contacts.classification_evidence,contacts.needs_review,contacts.classification_hash,
         contacts.macroarea,contacts.taxonomy_version_id,
         tv.semantic_version,
         contacts.classification_state,contacts.classification_state_reason,
         contacts.raw_email,contacts.email_comparison_key,contacts.normalization_method,contacts.organization_id,contacts.organization_name,
         contacts.denomination,contacts.person_name,contacts.role,contacts.department,contacts.address,contacts.postal_code,contacts.official_code,
         contacts.routing_status,contacts.mailbox_status,
         contacts.suppression_status,contacts.first_seen_at,contacts.last_seen_at,contacts.last_confirmed_at,contacts.source_occurrence_count,
         contacts.independent_source_count,contacts.is_role_mailbox,contacts.is_disposable,contacts.is_placeholder,contacts.is_typo_suspected,
         contacts.quality_score
         FROM contacts
         LEFT JOIN taxonomy_versions tv ON tv.id = contacts.taxonomy_version_id
         {where_sql}{extra_where} ORDER BY {order_sql} LIMIT CAST(? AS INTEGER) OFFSET CAST(? AS INTEGER)"
        );
        let mut stmt = db.prepare(&sql).map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(query_values.iter()),
                row_to_contact_view,
            )
            .map_err(|err| err.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())
    };
    let mut contacts = match &plan {
        ListPlan::Ordered(order) => fetch("", order, fetch_limit, offset)?,
        ListPlan::TextDescending(key) => {
            let non_empty = format!(" AND {key} < {SORT_EMPTY}");
            let mut rows = fetch(
                &non_empty,
                &format!("{key} DESC,lower(contacts.email) DESC,contacts.id DESC"),
                fetch_limit,
                offset,
            )?;
            if rows.len() < fetch_limit {
                // Past the non-empty values. Their count is only needed when this
                // page holds none of them.
                let non_empty_count = if rows.is_empty() {
                    db.query_row(
                        &format!("SELECT COUNT(*) FROM contacts {where_sql}{non_empty}"),
                        rusqlite::params_from_iter(values.iter()),
                        |row| Ok(row.get::<_, i64>(0)? as usize),
                    )
                    .map_err(|err| err.to_string())?
                } else {
                    offset + rows.len()
                };
                rows.extend(fetch(
                    &format!(" AND {key} = {SORT_EMPTY}"),
                    "lower(contacts.email),contacts.id",
                    fetch_limit - rows.len(),
                    offset.saturating_sub(non_empty_count),
                )?);
            }
            rows
        }
    };
    let has_next = if include_total {
        offset.saturating_add(contacts.len()) < total
    } else {
        let extra = contacts.len() > limit;
        if extra {
            contacts.truncate(limit);
        }
        extra
    };
    let total = if include_total {
        total
    } else {
        offset
            .saturating_add(contacts.len())
            .saturating_add(usize::from(has_next))
    };
    Ok(ContactList {
        has_previous: offset > 0,
        has_next,
        contacts,
        total,
        limit,
        offset,
    })
}

const GENERIC_LOCAL_PARTS: [&str; 8] = [
    "info",
    "segreteria",
    "amministrazione",
    "protocollo",
    "posta",
    "urp",
    "contatti",
    "direzione",
];

/// Subcategories that say nothing beyond the category (the taxonomy's catch-alls),
/// plus any `Altro ente ...`. A contact left in one of them scores as unclassified.
const GENERIC_SUBCATEGORIES: [&str; 7] = [
    "",
    "varie",
    "unknown",
    "sconosciuto",
    "associazione generica",
    "ente non determinato",
    "dato misto o da revisionare",
];

/// Contact quality score (0-100), stored in `contacts.quality_score`. It is a
/// function of the contact row alone, with no per-campaign keyword or query
/// bonus, and it ignores how many sources listed the address and how the contact
/// was classified (method or confidence). 30 specificity (a named person and
/// role beat a generic mailbox), 25 data richness (organization, place, website,
/// address, official code), 15 classification specificity (a real category and
/// subcategory, not a catch-all like "Altro ente formativo"), 15 freshness (time
/// since the last confirmation or sighting) and 15 deliverability (routing,
/// mailbox, PEC, placeholder, typo). `row` is the table alias (`contacts`, or
/// `new` inside a trigger). Migration 60 materializes it through triggers, so
/// changing it requires a migration that recreates them and backfills the column.
pub(crate) fn contact_score_sql(row: &str) -> String {
    let local = format!(
        "lower(trim(CASE WHEN trim(coalesce({row}.email_local_part,''))<>'' \
         THEN {row}.email_local_part \
         WHEN instr({row}.email,'@')>1 THEN substr({row}.email,1,instr({row}.email,'@')-1) \
         ELSE {row}.email END))"
    );
    let generic_local = GENERIC_LOCAL_PARTS
        .iter()
        .map(|prefix| format!("(({local})||' ') GLOB '{prefix}[^a-z0-9_]*'"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let filled = |column: &str| format!("trim(coalesce({row}.{column},''))<>''");
    let has_name = filled("person_name");
    let has_role = filled("role");
    let has_office = format!("({} OR {})", filled("role"), filled("department"));
    let personale = format!("{row}.contact_kind='personale'");
    let role_mailbox = format!("{row}.is_role_mailbox=1");
    let specificity = format!(
        "CASE WHEN {has_name} AND {has_role} THEN 1.0 \
         WHEN {has_name} THEN 0.85 \
         WHEN {personale} THEN 0.72 \
         WHEN {has_office} THEN 0.7 \
         WHEN {role_mailbox} AND NOT ({generic_local}) THEN 0.65 \
         WHEN {generic_local} THEN 0.55 \
         ELSE 0.45 END"
    );

    // Person, role and office are already in the specificity term.
    let present = |condition: String| format!("(CASE WHEN {condition} THEN 1.0 ELSE 0.0 END)");
    let richness = format!(
        "(({} + {} + {} + {} + {}) / 5.0)",
        present(format!(
            "{} OR {}",
            filled("organization_name"),
            filled("denomination")
        )),
        present(filled("comune")),
        present(filled("website")),
        present(format!(
            "{} OR {}",
            filled("address"),
            filled("postal_code")
        )),
        present(filled("official_code")),
    );

    let category = format!("lower(trim(coalesce({row}.category,'')))");
    let subcategory = format!("lower(trim(coalesce({row}.subcategory,'')))");
    let generic_subcategory = format!(
        "{subcategory} IN ({}) OR {subcategory} LIKE 'altro ente %'",
        GENERIC_SUBCATEGORIES
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let classification = format!(
        "CASE WHEN {category} IN ('','varie') THEN 0.0 \
         WHEN {generic_subcategory} THEN 0.4 \
         ELSE 1.0 END"
    );

    // Personal and role mailboxes go stale faster than institutional ones.
    let span = format!("CASE WHEN {role_mailbox} OR {personale} THEN 12.0 ELSE 24.0 END");
    let observed = format!("julianday(COALESCE({row}.last_confirmed_at, {row}.last_seen_at))");
    let freshness = format!(
        "CASE WHEN {observed} IS NULL THEN 0.3 \
         ELSE min(1.0,max(0.0, 1.0 - (max(0.0,(julianday('now') - {observed})/30.4375)) / ({span}) )) END"
    );

    let routing = format!("lower(trim(coalesce({row}.routing_status,'')))");
    let mailbox = format!("lower(trim(coalesce({row}.mailbox_status,'')))");
    let routing_factor = format!(
        "(CASE WHEN {routing} IN ('','unknown','not_checked','routing_ok','ok','valid','routable') THEN 1.0 \
         WHEN {routing}='no_routing' THEN 0.2 ELSE 0.7 END)"
    );
    let mailbox_factor = format!(
        "(CASE WHEN {mailbox} IN ('','unknown','accepted','valid','ok') THEN 1.0 \
         WHEN {mailbox}='catch_all' THEN 0.7 ELSE 0.3 END)"
    );
    let deliverability = format!(
        "(CASE WHEN {row}.is_pec=1 OR {row}.is_placeholder=1 OR {row}.is_disposable=1 THEN 0.0 \
         ELSE {routing_factor}*{mailbox_factor}*(CASE WHEN {row}.is_typo_suspected=1 THEN 0.5 ELSE 1.0 END) END)"
    );

    format!(
        "CAST(min(100.0,max(0.0,round(coalesce(\
         30.0*({specificity}) + 25.0*({richness}) + 15.0*({classification}) \
         + 15.0*({freshness}) + 15.0*({deliverability}),\
         0.0)))) AS INTEGER)"
    )
}

/// The columns `contact_score_sql` reads; the update trigger fires on these only.
pub(crate) const CONTACT_SCORE_INPUTS: &str = "email, email_local_part, person_name, role, department, \
    contact_kind, is_role_mailbox, organization_name, denomination, comune, website, address, \
    postal_code, official_code, category, subcategory, \
    last_confirmed_at, last_seen_at, routing_status, mailbox_status, is_pec, is_placeholder, \
    is_disposable, is_typo_suspected";

fn geographic_filter_aliases(column: &str, value: &str) -> Vec<String> {
    let norm_val = crate::gazetteer::norm(value);
    let mut tokens = vec![value.to_owned()];

    if column == "regione" {
        if norm_val.contains("valle") || norm_val == "vda" || norm_val == "aosta" {
            tokens.extend(
                [
                    "Valle d'Aosta",
                    "Valle daosta",
                    "Val d'Aoste",
                    "VDA",
                    "Aosta",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val.contains("trentino") || norm_val.contains("alto adige") {
            tokens.extend(
                [
                    "Trentino Alto Adige",
                    "Trentino-Alto Adige",
                    "Südtirol",
                    "Sudtirol",
                    "Alto Adige",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val.contains("friuli") || norm_val == "fvg" {
            tokens.extend(
                [
                    "Friuli Venezia Giulia",
                    "Friuli-Venezia Giulia",
                    "Friuli",
                    "FVG",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val.contains("emilia") || norm_val.contains("romagna") {
            tokens.extend(
                ["Emilia Romagna", "Emilia-Romagna", "Emilia"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val.contains("abruzzo") || norm_val.contains("abruzzi") {
            tokens.extend(["Abruzzo", "Abruzzi"].iter().map(|s| s.to_string()));
        }
    } else if column == "provincia" {
        if norm_val == "sud sardegna"
            || norm_val == "su"
            || norm_val == "ci"
            || norm_val == "vs"
            || norm_val == "og"
        {
            tokens.extend(
                [
                    "Sud Sardegna",
                    "Carbonia-Iglesias",
                    "Carbonia",
                    "Iglesias",
                    "Medio Campidano",
                    "Ogliastra",
                    "CI",
                    "VS",
                    "OG",
                    "SU",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val == "sassari" || norm_val == "ss" || norm_val == "ot" {
            tokens.extend(
                ["Sassari", "Olbia-Tempio", "Olbia", "Tempio", "OT", "SS"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val.contains("forli") || norm_val.contains("cesena") || norm_val == "fc" {
            tokens.extend(
                [
                    "Forlì-Cesena",
                    "Forli-Cesena",
                    "Forlì Cesena",
                    "Forli' Cesena",
                    "Forli",
                    "Forlì",
                    "FC",
                    "Cesena",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val.contains("massa") || norm_val.contains("carrara") || norm_val == "ms" {
            tokens.extend(
                ["Massa-Carrara", "Massa Carrara", "Massa", "Carrara", "MS"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val.contains("pesaro") || norm_val.contains("urbino") || norm_val == "pu" {
            tokens.extend(
                [
                    "Pesaro e Urbino",
                    "Pesaro-Urbino",
                    "Pesaro Urbino",
                    "Pesaro",
                    "Urbino",
                    "PU",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val.contains("barletta")
            || norm_val.contains("andria")
            || norm_val.contains("trani")
            || norm_val == "bt"
        {
            tokens.extend(
                [
                    "Barletta-Andria-Trani",
                    "Barletta Andria Trani",
                    "Barletta Andr5",
                    "Barletta",
                    "Andria",
                    "Trani",
                    "BT",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val.contains("monza") || norm_val.contains("brianza") || norm_val == "mb" {
            tokens.extend(
                ["Monza e Brianza", "Monza Brianza", "Monza", "Brianza", "MB"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val.contains("verbano")
            || norm_val.contains("cusio")
            || norm_val.contains("ossola")
            || norm_val == "verbania"
            || norm_val == "vb"
        {
            tokens.extend(
                [
                    "Verbano-Cusio-Ossola",
                    "Verbano Cusio Ossola",
                    "Verbania",
                    "VB",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        } else if norm_val.contains("reggio emilia") || norm_val == "re" {
            tokens.extend(
                ["Reggio Emilia", "Reggio nell'Emilia", "RE"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val.contains("reggio calabria") || norm_val == "rc" {
            tokens.extend(
                ["Reggio Calabria", "Reggio di Calabria", "RC"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val == "bolzano" || norm_val == "bozen" || norm_val == "bz" {
            tokens.extend(
                ["Bolzano", "Bozen", "Alto Adige", "BZ"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val == "aosta" || norm_val == "ao" {
            tokens.extend(
                ["Aosta", "AO", "Valle d'Aosta"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
    } else if column == "comune" {
        if norm_val.contains("abetone") || norm_val.contains("cutigliano") {
            tokens.extend(
                ["Abetone Cutigliano", "Abetone", "Cutigliano"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        } else if norm_val == "belvi" {
            tokens.extend(["Belvì", "Belvi", "Belvi'"].iter().map(|s| s.to_string()));
        } else if norm_val == "andorno micca" || norm_val == "adorno micca" {
            tokens.extend(
                ["Andorno Micca", "Adorno Micca"]
                    .iter()
                    .map(|s| s.to_string()),
            );
        }
    }

    tokens
}

pub(crate) fn contact_fts_match_query(raw: &str) -> Option<String> {
    let tokens = raw
        .split_whitespace()
        .filter_map(|token| {
            let cleaned: String = token
                .chars()
                .filter(|character| {
                    character.is_alphanumeric()
                        || matches!(character, '@' | '.' | '-' | '_' | '+' | '\'')
                })
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(format!("\"{cleaned}\"*"))
            }
        })
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

/// SQL predicate for the exportable contact projection: valid, active, not an
/// automated mailbox, not blacklisted, not suppressed. Migration 54 materializes it
/// into `contacts.is_exportable` through triggers, so changing it (or
/// `NON_CONTACT_LOCAL_PREFIXES`) requires a migration that recreates the triggers
/// and backfills the column.
pub(crate) fn exportable_contact_sql(row: &str) -> String {
    let local = format!("(lower(substr({row}.email,1,instr({row}.email,'@')-1))||'!')");
    // Branch on the first letter so most addresses skip the patterns entirely.
    let mut by_initial = std::collections::BTreeMap::<char, Vec<String>>::new();
    for prefix in crate::parsing::NON_CONTACT_LOCAL_PREFIXES {
        by_initial
            .entry(prefix.chars().next().unwrap_or_default())
            .or_default()
            .push(format!("{local} GLOB '{prefix}[^a-z]*'"));
    }
    let automated = by_initial
        .iter()
        .map(|(initial, patterns)| format!("WHEN '{initial}' THEN ({})", patterns.join(" OR ")))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{row}.status='valid'
         AND coalesce({row}.suppression_status,'active')='active'
         AND NOT (CASE lower(substr({row}.email,1,1)) {automated} ELSE 0 END)
         AND NOT EXISTS (SELECT 1 FROM blacklist b
                         WHERE b.normalized_email={row}.email COLLATE NOCASE)
         AND NOT EXISTS (SELECT 1 FROM suppressions s
                         WHERE s.comparison_key=COALESCE({row}.email_comparison_key,lower({row}.email)) COLLATE NOCASE
                           AND s.lifted_at IS NULL)"
    )
}

pub(crate) fn filter_where(filters: &ListFilters) -> (String, Vec<String>) {
    let mut clauses = vec!["contacts.is_exportable=1".to_owned()];
    let mut values = Vec::new();
    let query = filters.query.trim();
    if !query.is_empty() {
        if let Some(fts_query) = contact_fts_match_query(query) {
            clauses.push(
                "contacts.rowid IN (SELECT rowid FROM contacts_fts WHERE contacts_fts MATCH ?)"
                    .to_owned(),
            );
            values.push(fts_query);
        } else {
            clauses.push("(lower(contacts.email||' '||coalesce(contacts.source_file,'')||' '||coalesce(contacts.category,'')||' '||coalesce(contacts.subcategory,'')||' '||coalesce(contacts.contact_kind,'')||' '||coalesce(contacts.paese,'')||' '||coalesce(contacts.macroarea,'')||' '||coalesce(contacts.comune,'')||' '||coalesce(contacts.provincia,'')||' '||coalesce(contacts.regione,'')) LIKE ?)".to_owned());
            values.push(format!("%{}%", query.to_lowercase()));
        }
    }
    for (column, filter) in [
        ("source_file", &filters.source_file),
        ("category", &filters.category),
        ("subcategory", &filters.subcategory),
        ("contact_kind", &filters.contact_kind),
        ("paese", &filters.paese),
        ("macroarea", &filters.macroarea),
        ("regione", &filters.regione),
        ("provincia", &filters.provincia),
        ("comune", &filters.comune),
        ("classification_method", &filters.classification_method),
        (
            "classification_confidence",
            &filters.classification_confidence,
        ),
    ] {
        let is_multi_value = matches!(
            column,
            "category" | "subcategory" | "regione" | "provincia" | "comune" | "macroarea"
        );
        if column == "source_file" {
            if !filter.include.is_empty() {
                clauses.push(format!(
                    "({})",
                    vec![
                        "lower(coalesce(source_file,'')) LIKE '%' || lower(?) || '%'";
                        filter.include.len()
                    ]
                    .join(" OR ")
                ));
                values.extend(filter.include.iter().cloned());
            }
            if !filter.exclude.is_empty() {
                clauses.push(format!(
                    "(source_file IS NULL OR NOT ({}))",
                    vec![
                        "lower(coalesce(source_file,'')) LIKE '%' || lower(?) || '%'";
                        filter.exclude.len()
                    ]
                    .join(" OR ")
                ));
                values.extend(filter.exclude.iter().cloned());
            }
            continue;
        }
        if is_multi_value {
            if matches!(column, "category" | "subcategory") {
                if !filter.include.is_empty() {
                    clauses.push(format!(
                        "lower(coalesce({column},'')) IN ({})",
                        vec!["lower(?)"; filter.include.len()].join(",")
                    ));
                    values.extend(filter.include.iter().cloned());
                }
                if !filter.exclude.is_empty() {
                    clauses.push(format!(
                        "(coalesce({column},'')='' OR lower({column}) NOT IN ({}))",
                        vec!["lower(?)"; filter.exclude.len()].join(",")
                    ));
                    values.extend(filter.exclude.iter().cloned());
                }
                continue;
            }
            let token_match = format!(
                "lower(replace(replace(replace(replace(replace(coalesce({column},''),'’',\"'\"),'‘',\"'\"),'´',\"'\"),'`',\"'\"),'-',' ')) LIKE '%' || lower(replace(replace(replace(replace(replace(?,'’',\"'\"),'‘',\"'\"),'´',\"'\"),'`',\"'\"),'-',' ')) || '%'"
            );
            let mut inc_tokens = Vec::new();
            for val in &filter.include {
                for alias in geographic_filter_aliases(column, val) {
                    inc_tokens.push(alias);
                }
            }
            if !inc_tokens.is_empty() {
                clauses.push(format!(
                    "({})",
                    vec![token_match.clone(); inc_tokens.len()].join(" OR ")
                ));
                values.extend(inc_tokens);
            }
            let mut exc_tokens = Vec::new();
            for val in &filter.exclude {
                for alias in geographic_filter_aliases(column, val) {
                    exc_tokens.push(alias);
                }
            }
            if !exc_tokens.is_empty() {
                clauses.push(format!(
                    "({column} IS NULL OR NOT ({}))",
                    vec![token_match; exc_tokens.len()].join(" OR ")
                ));
                values.extend(exc_tokens);
            }
            continue;
        }
        let expression = if column == "classification_confidence" {
            format!("CAST({column} AS TEXT)")
        } else {
            column.to_owned()
        };
        if !filter.include.is_empty() {
            clauses.push(format!(
                "{expression} IN ({})",
                vec!["?"; filter.include.len()].join(",")
            ));
            values.extend(filter.include.iter().cloned());
        }
        if !filter.exclude.is_empty() {
            clauses.push(format!(
                "({column} IS NULL OR {expression} NOT IN ({}))",
                vec!["?"; filter.exclude.len()].join(",")
            ));
            values.extend(filter.exclude.iter().cloned());
        }
    }
    if !filters.pec.include.is_empty() {
        let inc_pec = filters.pec.include.iter().any(|v| v == "pec");
        let inc_std = filters.pec.include.iter().any(|v| v == "standard");
        if inc_pec && !inc_std {
            clauses.push("contacts.is_pec = 1".to_owned());
        } else if inc_std && !inc_pec {
            clauses.push("contacts.is_pec = 0".to_owned());
        }
    }
    if !filters.pec.exclude.is_empty() {
        let excl_pec = filters.pec.exclude.iter().any(|v| v == "pec");
        let excl_std = filters.pec.exclude.iter().any(|v| v == "standard");
        if excl_pec && !excl_std {
            clauses.push("contacts.is_pec = 0".to_owned());
        } else if excl_std && !excl_pec {
            clauses.push("contacts.is_pec = 1".to_owned());
        }
    }
    if let Some(minimum) = filters.min_score {
        // An integer, so it is safe to inline and needs no bind slot.
        clauses.push(format!("contacts.quality_score >= {minimum}"));
    }
    (
        if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        },
        values,
    )
}

const FACET_COLUMNS: [&str; 11] = [
    "source_file",
    "category",
    "subcategory",
    "contact_kind",
    "paese",
    "macroarea",
    "regione",
    "provincia",
    "comune",
    "classification_method",
    "classification_confidence",
];

/// Filter options for every facet, counted over the exportable contacts in one
/// table scan: a GROUP BY per facet meant eleven scans of the wide contacts table.
pub(crate) fn facet_options(db: &Connection) -> Result<crate::FilterOptions, String> {
    let mut raw = vec![HashMap::<String, usize>::new(); FACET_COLUMNS.len()];
    let mut pec = [0usize; 2];
    let mut statement = db
        .prepare(
            "SELECT source_file,category,subcategory,contact_kind,paese,macroarea,regione,
                    provincia,comune,classification_method,
                    CAST(classification_confidence AS TEXT),is_pec
             FROM contacts WHERE is_exportable=1",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = statement.query([]).map_err(|error| error.to_string())?;
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        for (index, counts) in raw.iter_mut().enumerate() {
            let rusqlite::types::ValueRef::Text(bytes) =
                row.get_ref(index).map_err(|error| error.to_string())?
            else {
                continue;
            };
            let value = String::from_utf8_lossy(bytes);
            if value.trim().is_empty() {
                continue;
            }
            match counts.get_mut(value.as_ref()) {
                Some(count) => *count += 1,
                None => {
                    counts.insert(value.into_owned(), 1);
                }
            }
        }
        let is_pec: i64 = row.get(11).map_err(|error| error.to_string())?;
        pec[usize::from(is_pec == 0)] += 1;
    }
    let mut raw = raw.into_iter();
    let mut next = |column: &str| facet_from_raw(column, raw.next().unwrap_or_default());
    Ok(crate::FilterOptions {
        source_file: next("source_file"),
        category: next("category"),
        subcategory: next("subcategory"),
        contact_kind: next("contact_kind"),
        paese: next("paese"),
        macroarea: next("macroarea"),
        regione: next("regione"),
        provincia: next("provincia"),
        comune: next("comune"),
        classification_method: next("classification_method"),
        classification_confidence: next("classification_confidence"),
        pec: [("pec", pec[0]), ("standard", pec[1])]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(value, count)| FilterOption {
                value: value.to_owned(),
                count,
            })
            .collect(),
    })
}

fn facet_from_raw(column: &str, raw: HashMap<String, usize>) -> Vec<FilterOption> {
    let mut counts = BTreeMap::<String, usize>::new();
    for (raw_value, count) in raw {
        match column {
            "source_file" => {
                let mut seen = std::collections::BTreeSet::new();
                for value in split_merged_values(&raw_value) {
                    let base = source_file_base_name(&value);
                    if seen.insert(base.clone()) {
                        *counts.entry(base).or_default() += count;
                    }
                }
            }
            "regione" | "provincia" | "comune" | "macroarea" => {
                for value in split_merged_values(&raw_value) {
                    let sanitized = match column {
                        "comune" => crate::ai_enrichment::normalize_extracted_comune(&value),
                        "regione" => crate::ai_enrichment::normalize_extracted_regione(&value),
                        "macroarea" => crate::ai_enrichment::normalize_extracted_macroarea(&value),
                        _ => {
                            let normalized =
                                crate::ai_enrichment::normalize_extracted_province(value);
                            (!normalized.is_empty()
                                && !normalized.chars().all(|c| c.is_ascii_digit()))
                            .then_some(normalized)
                        }
                    };
                    if let Some(clean_val) = sanitized {
                        *counts.entry(clean_val).or_default() += count;
                    }
                }
            }
            _ => *counts.entry(raw_value).or_default() += count,
        }
    }
    let mut options = counts
        .into_iter()
        .map(|(value, count)| FilterOption { value, count })
        .collect::<Vec<_>>();
    options.sort_by_cached_key(|option| option.value.to_lowercase());
    options
}

#[cfg(test)]
pub(crate) fn facet_values(db: &Connection, column: &str) -> Result<Vec<FilterOption>, String> {
    let options = facet_options(db)?;
    Ok(match column {
        "source_file" => options.source_file,
        "category" => options.category,
        "subcategory" => options.subcategory,
        "contact_kind" => options.contact_kind,
        "paese" => options.paese,
        "macroarea" => options.macroarea,
        "regione" => options.regione,
        "provincia" => options.provincia,
        "comune" => options.comune,
        "pec" => options.pec,
        "classification_method" => options.classification_method,
        "classification_confidence" => options.classification_confidence,
        _ => return Err("Campo filtro non valido".to_owned()),
    })
}

pub(crate) fn source_file_base_name(value: &str) -> String {
    let name = value
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value.trim());
    name.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(name)
        .trim()
        .to_owned()
}

pub(crate) fn row_to_contact(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContactRecord> {
    let status: String = row.get(5)?;
    let validation_notes: String = row.get(6)?;
    let email: String = row.get(1)?;
    Ok(ContactRecord {
        id: row.get(0)?,
        email: email.clone(),
        email_local_part: row.get(2)?,
        email_domain: row.get(3)?,
        email_tld: row.get(4)?,
        status: EmailStatus::from_db(&status),
        validation_notes: serde_json::from_str(&validation_notes).unwrap_or_default(),
        source_file: row.get(7)?,
        category: row.get(8)?,
        subcategory: row.get(9)?,
        contact_kind: row.get(10)?,
        paese: row.get(11)?,
        is_pec: row.get::<_, i64>(12)? != 0,
        comune: row.get(13)?,
        provincia: row.get(14)?,
        regione: row.get(15)?,
        website: row.get(16)?,
        source_url: row.get(17)?,
        category_id: row.get(18)?,
        subcategory_id: row.get(19)?,
        classification_method: row.get(20)?,
        classification_confidence: row.get(21)?,
        classification_evidence: serde_json::from_str(&row.get::<_, String>(22)?)
            .unwrap_or_default(),
        needs_review: row.get::<_, i64>(23)? != 0,
        classification_hash: row.get(24)?,
        macroarea: row.get(25)?,
        taxonomy_version_id: row.get(26)?,
        taxonomy_version: row.get(27)?,
        classification_state: row.get(28)?,
        classification_state_reason: row.get(29)?,
    })
}

fn row_to_contact_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContactView> {
    Ok(ContactView {
        contact: row_to_contact(row)?,
        raw_email: row.get(30)?,
        email_comparison_key: row.get(31)?,
        normalization_method: row.get(32)?,
        organization_id: row.get(33)?,
        organization_name: row.get(34)?,
        denomination: row.get(35)?,
        person_name: row.get(36)?,
        role: row.get(37)?,
        department: row.get(38)?,
        address: row.get(39)?,
        postal_code: row.get(40)?,
        official_code: row.get(41)?,
        routing_status: row.get(42)?,
        mailbox_status: row.get(43)?,
        suppression_status: row.get(44)?,
        first_seen_at: row.get(45)?,
        last_seen_at: row.get(46)?,
        last_confirmed_at: row.get(47)?,
        source_occurrence_count: row.get(48)?,
        independent_source_count: row.get(49)?,
        is_role_mailbox: row.get::<_, i64>(50)? != 0,
        is_disposable: row.get::<_, i64>(51)? != 0,
        is_placeholder: row.get::<_, i64>(52)? != 0,
        is_typo_suspected: row.get::<_, i64>(53)? != 0,
        quality_score: row.get(54)?,
    })
}

pub(crate) fn new_contact_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn normalize_category(value: &str) -> String {
    value.trim().to_lowercase()
}

pub(crate) fn standard_category<'a>(categories: impl Iterator<Item = &'a str>) -> String {
    let mut selected = "Varie";
    for category in categories {
        let normalized = normalize_category(category);
        if normalized.is_empty() {
            continue;
        }
        if matches!(
            normalized.as_str(),
            "istruzione"
                | "pubblica amministrazione"
                | "enti religiosi"
                | "cultura"
                | "turismo e promozione territoriale"
                | "associazioni"
                | "varie"
        ) {
            if normalized == "varie" {
                continue;
            }
            return match normalized.as_str() {
                "istruzione" => "Istruzione",
                "pubblica amministrazione" => "Pubblica amministrazione",
                "enti religiosi" => "Enti religiosi",
                "cultura" => "Cultura",
                "turismo e promozione territoriale" => "Turismo e promozione territoriale",
                "associazioni" => "Associazioni",
                _ => "Varie",
            }
            .to_owned();
        }
        if normalized.contains("comun") {
            selected = "Pubblica amministrazione";
        }
        if normalized.contains("scuol")
            || normalized.contains("docent")
            || normalized.contains("insegn")
        {
            return "Istruzione".to_owned();
        }
        if normalized.contains("parroc")
            || normalized.contains("orator")
            || normalized.contains("chiesa")
        {
            return "Enti religiosi".to_owned();
        }
        if normalized.contains("frat")
            || normalized.contains("domenican")
            || normalized.contains("agostinian")
            || normalized.contains("salesian")
            || normalized.contains("dioces")
            || normalized.contains("decanat")
            || normalized.contains("sacerdot")
            || normalized.contains("preti")
        {
            return "Enti religiosi".to_owned();
        }
        if normalized.contains("bibliotec")
            || normalized.contains("teatr")
            || normalized.contains("cinema")
            || normalized.contains("muse")
        {
            return "Cultura".to_owned();
        }
        if normalized.contains("turism")
            || normalized.contains("proloco")
            || normalized.contains("pro loco")
            || normalized.contains("apt")
        {
            return "Turismo e promozione territoriale".to_owned();
        }
        if normalized.contains("associa")
            || normalized.contains("proloco")
            || normalized.contains("pro loco")
            || normalized.contains("alpin")
            || normalized.contains("rotary")
            || normalized.contains("lions")
            || normalized.contains("cai")
            || normalized.contains("noi")
        {
            selected = "Associazioni";
        }
    }
    selected.to_owned()
}
