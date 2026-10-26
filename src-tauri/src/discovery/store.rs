use super::*;

pub(crate) fn collect_leads(
    pages: &[FetchedPage],
    now: &str,
) -> (BTreeMap<String, LeadAccumulator>, BTreeSet<String>) {
    let mut leads: BTreeMap<String, LeadAccumulator> = BTreeMap::new();
    let mut pdfs: BTreeSet<String> = BTreeSet::new();

    for page in pages {
        let parsed = parse_page(&page.url, &page.html);
        for pdf in parsed.pdf_urls {
            pdfs.insert(pdf);
        }

        for hit in &parsed.emails {
            let key = email_comparison_key(&hit.email);
            if !leads.contains_key(&key) {
                leads.insert(
                    key.clone(),
                    LeadAccumulator {
                        email: hit.email.clone(),
                        comparison_key: key.clone(),
                        source_url: page.url.clone(),
                        pages: BTreeSet::new(),
                        retrieved_at: now.to_owned(),
                        is_pec: is_pec_email(&hit.email),
                        role: context_role(&hit.context),
                        person_name: context_person_name(&hit.context),
                        context: LeadContext::default(),
                        method: page.via.to_owned(),
                    },
                );
            }
            if let Some(entry) = leads.get_mut(&key) {
                entry.pages.insert(page.url.clone());
                if entry.role.is_none() {
                    entry.role = context_role(&hit.context);
                }
                if entry.person_name.is_none() {
                    entry.person_name = context_person_name(&hit.context);
                }
            }
        }
    }

    (leads, pdfs)
}

/// Insert one run and all of its leads in a single transaction (offline tests).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn persist_search_run(
    db: &Connection,
    query_text: &str,
    seeds: &[String],
    pages: &[FetchedPage],
    now: &str,
    max_pdfs: i64,
    errors: &[String],
) -> Result<SearchRunSummary, String> {
    let (accumulators, pdfs) = collect_leads(pages, now);
    let pdf_found = if max_pdfs <= 0 {
        0
    } else {
        (pdfs.len() as i64).min(max_pdfs)
    };
    insert_run(
        db,
        &RunRecord {
            kind: "web",
            query_text,
            source_id: None,
            seed_count: seeds.len() as i64,
            now,
        },
        pages.len() as i64,
        pdf_found,
        errors,
        accumulators.into_values(),
    )
}

/// Insert the leads with their run in a single transaction (offline tests).
#[cfg(test)]
pub(crate) fn insert_run(
    db: &Connection,
    run: &RunRecord,
    page_count: i64,
    pdf_found: i64,
    errors: &[String],
    leads: impl IntoIterator<Item = LeadAccumulator>,
) -> Result<SearchRunSummary, String> {
    let owned = db
        .is_autocommit()
        .then(|| db.unchecked_transaction())
        .transpose()
        .map_err(|error| error.to_string())?;
    let run_id = create_run(db, run, "running")?;
    let lead_count = append_leads(db, &run_id, leads)?;
    finish_run(db, &run_id, "completed", page_count, pdf_found, errors)?;
    if let Some(transaction) = owned {
        transaction.commit().map_err(|error| error.to_string())?;
    }
    Ok(SearchRunSummary {
        run_id,
        lead_count: lead_count as i64,
        page_count,
        pdf_found,
        errors: errors.to_vec(),
    })
}

/// Insert the `search_runs` row. A crawl starts `running` and appends leads
/// host by host, so a cancelled or crashed crawl keeps what it found.
pub(crate) fn create_run(db: &Connection, run: &RunRecord, status: &str) -> Result<String, String> {
    let run_id = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO search_runs(
            id,query_text,status,
            seed_count,page_count,lead_count,pdf_found,error,created_at,source_id,kind
         ) VALUES(?1,?2,?3,?4,0,0,0,NULL,?5,?6,?7)",
        params![
            run_id,
            run.query_text,
            status,
            run.seed_count,
            run.now,
            run.source_id,
            run.kind,
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(run_id)
}

/// Insert leads into an existing run. An address already in the run
/// is skipped, so appending the same host twice cannot duplicate it.
pub(crate) fn append_leads(
    db: &Connection,
    run_id: &str,
    leads: impl IntoIterator<Item = LeadAccumulator>,
) -> Result<usize, String> {
    let mut leads: Vec<LeadAccumulator> = leads.into_iter().collect();
    leads.sort_by(|left, right| left.comparison_key.cmp(&right.comparison_key));

    // Own transaction when called alone; inside `insert_run` the caller's
    // transaction already covers the batch.
    let owned = db
        .is_autocommit()
        .then(|| db.unchecked_transaction())
        .transpose()
        .map_err(|error| error.to_string())?;
    let mut inserted = 0usize;
    {
        let mut statement = db
            .prepare(
                "INSERT INTO search_leads(
                    id,run_id,email,email_comparison_key,source_url,retrieved_at,
                    is_pec,person_name,role,
                    category,subcategory,organization,comune,provincia,regione,website,
                    official_code,department,address,postal_code,paese,method
                 ) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,
                          ?21,?22
                   WHERE NOT EXISTS (
                       SELECT 1 FROM search_leads WHERE run_id=?2 AND email_comparison_key=?4
                   )",
            )
            .map_err(|error| error.to_string())?;
        for lead in &leads {
            let context = &lead.context;
            inserted += statement
                .execute(params![
                    uuid::Uuid::new_v4().to_string(),
                    run_id,
                    lead.email,
                    lead.comparison_key,
                    lead.source_url,
                    lead.retrieved_at,
                    if lead.is_pec { 1_i64 } else { 0_i64 },
                    lead.person_name,
                    lead.role,
                    context.category,
                    context.subcategory,
                    context.organization,
                    context.comune,
                    context.provincia,
                    context.regione,
                    context.website,
                    context.official_code,
                    context.department,
                    context.address,
                    context.postal_code,
                    context.paese,
                    lead.method,
                ])
                .map_err(|error| error.to_string())?;
        }
    }
    db.execute(
        "UPDATE search_runs SET lead_count=(SELECT COUNT(*) FROM search_leads WHERE run_id=?1)
         WHERE id=?1",
        params![run_id],
    )
    .map_err(|error| error.to_string())?;
    if let Some(transaction) = owned {
        transaction.commit().map_err(|error| error.to_string())?;
    }
    Ok(inserted)
}

/// Close a run with its final status, counters and non-fatal errors.
pub(crate) fn finish_run(
    db: &Connection,
    run_id: &str,
    status: &str,
    page_count: i64,
    pdf_found: i64,
    errors: &[String],
) -> Result<(), String> {
    let error_text = (!errors.is_empty()).then(|| errors.join("; "));
    db.execute(
        "UPDATE search_runs SET status=?2, page_count=?3, pdf_found=?4, error=?5,
            lead_count=(SELECT COUNT(*) FROM search_leads WHERE run_id=?1)
         WHERE id=?1",
        params![run_id, status, page_count, pdf_found, error_text],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Runs still `running` when the app starts were cut off (crash, quit while
/// reading): close them as failed. Their leads and unread sites are kept.
pub(crate) fn close_orphaned_runs(db: &Connection) -> Result<usize, String> {
    db.execute(
        "UPDATE search_runs SET status='failed',
            error=coalesce(error||'; ','')||'Ricerca interrotta dalla chiusura dell''app',
            lead_count=(SELECT COUNT(*) FROM search_leads WHERE run_id=search_runs.id)
         WHERE status='running'",
        [],
    )
    .map_err(|error| error.to_string())
}

/// Filters of the Cerca lead table: the contacts filters that make sense for
/// leads, plus the run, the extraction method and whether the address is
/// already in the contacts database.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(crate) struct LeadFilters {
    pub(crate) query: String,
    /// Run kinds: `source` (open data) or `web`, `directory`, `crawl`. The page
    /// keeps the two families in separate tabs.
    pub(crate) kind: FieldFilter,
    pub(crate) run: FieldFilter,
    pub(crate) category: FieldFilter,
    pub(crate) subcategory: FieldFilter,
    pub(crate) regione: FieldFilter,
    pub(crate) provincia: FieldFilter,
    pub(crate) method: FieldFilter,
    /// `pec` or `standard`, like the contacts PEC filter.
    pub(crate) pec: FieldFilter,
    /// `nuovo`, `completa` (in contacts with empty fields the lead fills) or
    /// `nel_database`.
    pub(crate) stato: FieldFilter,
}

/// `nuovo` (not in contacts), `completa` (in contacts, and promotion changes
/// it: the lead fills an empty field, or an official registry replaces the
/// ente, code or place) or `nel_database` (nothing to change).
pub(super) const STATO_SQL: &str = "CASE
    WHEN NOT EXISTS(SELECT 1 FROM contacts c WHERE c.email_comparison_key = l.email_comparison_key COLLATE NOCASE)
        THEN 'nuovo'
    WHEN EXISTS(SELECT 1 FROM contacts c WHERE c.email_comparison_key = l.email_comparison_key COLLATE NOCASE AND (
        (trim(coalesce(c.organization_name,''))='' AND trim(coalesce(l.organization,''))<>'')
        OR (trim(coalesce(c.department,''))='' AND trim(coalesce(l.department,''))<>'')
        OR (trim(coalesce(c.person_name,''))='' AND trim(coalesce(l.person_name,''))<>'')
        OR (trim(coalesce(c.role,''))='' AND trim(coalesce(l.role,''))<>'')
        OR (trim(coalesce(c.official_code,''))='' AND trim(coalesce(l.official_code,''))<>'')
        OR (trim(coalesce(c.website,''))='' AND trim(coalesce(l.website,''))<>'')
        OR (trim(coalesce(c.comune,''))='' AND trim(coalesce(c.provincia,''))=''
            AND trim(coalesce(c.regione,''))='' AND trim(coalesce(l.regione,''))<>'')
        OR ((SELECT r.source_id FROM search_runs r WHERE r.id=l.run_id) IN ('miur-statali','miur-paritarie','indicepa','iccu-biblioteche') AND (
            (trim(coalesce(l.organization,''))<>'' AND l.organization IS NOT c.organization_name)
            OR (trim(coalesce(l.official_code,''))<>'' AND l.official_code IS NOT c.official_code)
            OR (trim(coalesce(l.comune,''))<>'' AND trim(coalesce(l.regione,''))<>''
                AND (l.comune IS NOT c.comune OR l.provincia IS NOT c.provincia OR l.regione IS NOT c.regione))
        ))
    )) THEN 'completa'
    ELSE 'nel_database' END";

/// Public registries whose ente, code and place replace the contact's values
/// on promotion; every other source only fills empty fields. Keep in sync
/// with the list inside `STATO_SQL`.
pub(super) const OFFICIAL_SOURCES: [&str; 4] = [
    "miur-statali",
    "miur-paritarie",
    "indicepa",
    "iccu-biblioteche",
];

pub(super) fn push_in(
    clauses: &mut Vec<String>,
    values: &mut Vec<Value>,
    expression: &str,
    filter: &FieldFilter,
) {
    if !filter.include.is_empty() {
        clauses.push(format!(
            "{expression} IN ({})",
            vec!["?"; filter.include.len()].join(",")
        ));
        values.extend(filter.include.iter().cloned().map(Value::Text));
    }
    if !filter.exclude.is_empty() {
        clauses.push(format!(
            "({expression} IS NULL OR {expression} NOT IN ({}))",
            vec!["?"; filter.exclude.len()].join(",")
        ));
        values.extend(filter.exclude.iter().cloned().map(Value::Text));
    }
}

/// `pec`/`standard` or `nuovo`/`nel_database`: one side kept, one side dropped.
pub(super) fn push_flag(
    clauses: &mut Vec<String>,
    filter: &FieldFilter,
    yes: &str,
    when_yes: &str,
    when_no: &str,
) {
    let wants = |values: &[String], value: &str| values.iter().any(|item| item == value);
    let keep = |on: bool| if on { when_yes } else { when_no };
    let include_yes = wants(&filter.include, yes);
    let include_no = !filter.include.is_empty() && filter.include.iter().any(|item| item != yes);
    if include_yes != include_no {
        clauses.push(keep(include_yes).to_owned());
    }
    let exclude_yes = wants(&filter.exclude, yes);
    let exclude_no = !filter.exclude.is_empty() && filter.exclude.iter().any(|item| item != yes);
    if exclude_yes != exclude_no {
        clauses.push(keep(!exclude_yes).to_owned());
    }
}

pub(crate) fn lead_filter_where(filters: &LeadFilters) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    let query = filters.query.trim().to_lowercase();
    if !query.is_empty() {
        clauses.push(
            "lower(l.email||' '||coalesce(l.organization,'')||' '||coalesce(l.department,'')||' '||
                   coalesce(l.person_name,'')||' '||coalesce(l.role,'')||' '||coalesce(l.category,'')||' '||
                   coalesce(l.subcategory,'')||' '||coalesce(l.comune,'')||' '||coalesce(l.provincia,'')||' '||
                   coalesce(l.regione,'')||' '||coalesce(l.website,'')) LIKE ?"
                .to_owned(),
        );
        values.push(Value::Text(format!("%{query}%")));
    }
    push_in(
        &mut clauses,
        &mut values,
        "(SELECT coalesce(r.kind,'web') FROM search_runs r WHERE r.id=l.run_id)",
        &filters.kind,
    );
    push_in(&mut clauses, &mut values, "l.run_id", &filters.run);
    push_in(&mut clauses, &mut values, "l.category", &filters.category);
    push_in(
        &mut clauses,
        &mut values,
        "l.subcategory",
        &filters.subcategory,
    );
    push_in(&mut clauses, &mut values, "l.regione", &filters.regione);
    push_in(&mut clauses, &mut values, "l.provincia", &filters.provincia);
    push_in(&mut clauses, &mut values, "l.method", &filters.method);
    push_flag(
        &mut clauses,
        &filters.pec,
        "pec",
        "l.is_pec=1",
        "l.is_pec=0",
    );
    push_in(
        &mut clauses,
        &mut values,
        &format!("({STATO_SQL})"),
        &filters.stato,
    );
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_sql, values)
}

/// One row per address across every matching run: the most recent lead wins
/// and `source_count` tells how many runs found it, like the contacts `fonti`.
pub(crate) fn list_sandbox_leads(
    db: &Connection,
    filters: &LeadFilters,
    limit: usize,
    offset: usize,
) -> Result<SearchLeadList, String> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let (where_sql, values) = lead_filter_where(filters);
    let total: i64 = db
        .query_row(
            &format!(
                "SELECT COUNT(DISTINCT l.email_comparison_key) FROM search_leads l {where_sql}"
            ),
            params_from_iter(values.iter()),
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;

    let sql = format!(
        "WITH f AS (SELECT l.rowid AS rid, l.email_comparison_key AS k FROM search_leads l {where_sql}),
              picked AS (SELECT max(rid) AS rid FROM f GROUP BY k)
         SELECT l.email,l.source_url,l.retrieved_at,l.is_pec,l.person_name,l.role,
                l.category,l.subcategory,l.organization,l.comune,l.provincia,l.regione,
                l.official_code,l.department,l.email_comparison_key,l.website,{macroarea},
                l.id,l.run_id,r.query_text,l.address,l.postal_code,l.paese,l.method,
                (SELECT COUNT(DISTINCT x.run_id) FROM search_leads x
                  WHERE x.email_comparison_key=l.email_comparison_key),
                ({STATO_SQL})
         FROM picked JOIN search_leads l ON l.rowid=picked.rid
         LEFT JOIN search_runs r ON r.id=l.run_id
         ORDER BY lower(l.email) ASC LIMIT ? OFFSET ?",
        macroarea = crate::schema::macroarea_sql().replace("regione", "l.regione"),
    );
    let mut values = values;
    values.push(Value::Integer(limit as i64));
    values.push(Value::Integer(offset as i64));
    let mut statement = db.prepare(&sql).map_err(|error| error.to_string())?;
    let leads = statement
        .query_map(params_from_iter(values.iter()), |row| {
            let email: String = row.get(0)?;
            let (email_local_part, email_domain, email_tld) = crate::parsing::email_parts(&email);
            Ok(SearchLeadView {
                email,
                email_local_part,
                email_domain,
                email_tld,
                email_comparison_key: row.get(14)?,
                website: row.get(15)?,
                macroarea: row.get(16)?,
                source_url: row.get(1)?,
                retrieved_at: row.get(2)?,
                is_pec: row.get::<_, i64>(3)? != 0,
                person_name: row.get(4)?,
                role: row.get(5)?,
                category: row.get(6)?,
                subcategory: row.get(7)?,
                organization: row.get(8)?,
                comune: row.get(9)?,
                provincia: row.get(10)?,
                regione: row.get(11)?,
                official_code: row.get(12)?,
                department: row.get(13)?,
                id: row.get(17)?,
                run_id: row.get(18)?,
                run_label: row.get(19)?,
                address: row.get(20)?,
                postal_code: row.get(21)?,
                paese: row.get(22)?,
                method: row.get(23)?,
                source_count: row.get(24)?,
                in_database: row.get::<_, String>(25)? != "nuovo",
                stato: row.get(25)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;

    let has_previous = offset > 0;
    let has_next = (offset as i64 + leads.len() as i64) < total;
    Ok(SearchLeadList {
        leads,
        total,
        limit,
        offset,
        has_previous,
        has_next,
    })
}

/// Leads of a single run, for the offline tests.
#[cfg(test)]
pub(crate) fn list_search_leads_page(
    db: &Connection,
    run_id: &str,
    limit: usize,
    offset: usize,
) -> Result<SearchLeadList, String> {
    let filters = LeadFilters {
        run: FieldFilter {
            include: vec![run_id.to_owned()],
            exclude: Vec::new(),
        },
        ..LeadFilters::default()
    };
    list_sandbox_leads(db, &filters, limit, offset)
}

#[derive(Debug, Serialize)]
pub(crate) struct LeadFilterOptions {
    pub(crate) run: Vec<crate::FilterOption>,
    pub(crate) category: Vec<crate::FilterOption>,
    pub(crate) subcategory: Vec<crate::FilterOption>,
    pub(crate) regione: Vec<crate::FilterOption>,
    pub(crate) provincia: Vec<crate::FilterOption>,
    pub(crate) method: Vec<crate::FilterOption>,
    pub(crate) pec: Vec<crate::FilterOption>,
    pub(crate) stato: Vec<crate::FilterOption>,
}

/// Distinct addresses per value of each facet, over the runs of the given kinds
/// (every run when empty).
pub(crate) fn lead_filter_options(
    db: &Connection,
    kinds: &[String],
) -> Result<LeadFilterOptions, String> {
    let (kind_sql, kind_values) = lead_filter_where(&LeadFilters {
        kind: FieldFilter {
            include: kinds.to_vec(),
            exclude: Vec::new(),
        },
        ..LeadFilters::default()
    });
    let scope = if kind_sql.is_empty() {
        "WHERE".to_owned()
    } else {
        format!("{kind_sql} AND")
    };
    let facet = |expression: &str| -> Result<Vec<crate::FilterOption>, String> {
        let mut statement = db
            .prepare(&format!(
                "SELECT {expression} AS value, COUNT(DISTINCT l.email_comparison_key)
                 FROM search_leads l {scope} {expression} IS NOT NULL AND {expression}<>''
                 GROUP BY value ORDER BY 2 DESC, value"
            ))
            .map_err(|error| error.to_string())?;

        statement
            .query_map(params_from_iter(kind_values.iter()), |row| {
                Ok(crate::FilterOption {
                    value: row.get(0)?,
                    count: row.get::<_, i64>(1)? as usize,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    };
    Ok(LeadFilterOptions {
        run: facet("l.run_id")?,
        category: facet("l.category")?,
        subcategory: facet("l.subcategory")?,
        regione: facet("l.regione")?,
        provincia: facet("l.provincia")?,
        method: facet("l.method")?,
        pec: facet("CASE WHEN l.is_pec=1 THEN 'pec' ELSE 'standard' END")?,
        stato: facet(&format!("({STATO_SQL})"))?,
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchRunView {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) status: String,
    pub(crate) lead_count: i64,
    pub(crate) page_count: i64,
    pub(crate) error: Option<String>,
    pub(crate) created_at: String,
}

pub(crate) fn list_runs(db: &Connection) -> Result<Vec<SearchRunView>, String> {
    let mut statement = db
        .prepare(
            "SELECT id, coalesce(query_text,''), coalesce(kind,'web'), status, lead_count,
                    page_count, error, created_at
             FROM search_runs ORDER BY created_at DESC, rowid DESC LIMIT 500",
        )
        .map_err(|error| error.to_string())?;

    statement
        .query_map([], |row| {
            Ok(SearchRunView {
                id: row.get(0)?,
                label: row.get(1)?,
                kind: row.get(2)?,
                status: row.get(3)?,
                lead_count: row.get(4)?,
                page_count: row.get(5)?,
                error: row.get(6)?,
                created_at: row.get(7)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

pub(crate) fn export_leads_txt_to_path(
    db: &Connection,
    filters: &LeadFilters,
    target: &Path,
) -> Result<usize, String> {
    let (where_sql, values) = lead_filter_where(filters);
    let peo = if where_sql.is_empty() {
        "WHERE l.is_pec=0"
    } else {
        " AND l.is_pec=0"
    };
    publish_export(target, |output| {
        let mut statement = db
            .prepare(&format!(
                "SELECT min(l.email) FROM search_leads l {where_sql}{peo}
                 GROUP BY lower(l.email) ORDER BY lower(l.email)"
            ))
            .map_err(|error| error.to_string())?;
        let mut rows = statement
            .query(params_from_iter(values.iter()))
            .map_err(|error| error.to_string())?;
        let mut count = 0usize;
        while let Some(row) = rows.next().map_err(|error| error.to_string())? {
            let email: String = row.get(0).map_err(|error| error.to_string())?;
            output
                .write_all(email.as_bytes())
                .map_err(|error| error.to_string())?;
            output.write_all(b"\n").map_err(|error| error.to_string())?;
            count += 1;
        }
        Ok(count)
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct PromoteSearchReport {
    pub(crate) added: usize,
    pub(crate) updated: usize,
    pub(crate) skipped_blacklist: usize,
    /// New contacts whose domain has no DNS result yet: hidden from Contatti
    /// until the mailbox verification checks it.
    pub(crate) unverified: usize,
}

/// Copy the filtered sandbox leads into `contacts`, merging on the comparison
/// key. Every run that found an address contributes its fields (the contact
/// merge keeps known values), PEC included with `is_pec`. Blacklisted
/// addresses are skipped. The leads that were copied (or skipped) leave the
/// sandbox; the whole promotion is one transaction.
pub(crate) fn promote_leads_inner(
    db: &Connection,
    filters: &LeadFilters,
) -> Result<PromoteSearchReport, String> {
    let blacklist = crate::blacklist::load_blacklist_set_from_db(db)?;
    let (where_sql, values) = lead_filter_where(filters);
    // The `stato` filter reads `contacts`, so the leaves are fixed before the
    // contacts change.
    let mut leaving = db
        .prepare(&format!("SELECT l.id FROM search_leads l {where_sql}"))
        .map_err(|error| error.to_string())?;
    let leaving_ids = leaving
        .query_map(params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(leaving);
    // Re-importing a source adds the same leads again: identical ones collapse
    // so each address is saved once per distinct content.
    let mut statement = db
        .prepare(&format!(
            "SELECT l.email, l.email_comparison_key, l.source_url, l.person_name, l.role,
                    l.category, l.subcategory, l.organization, l.comune, l.provincia, l.regione,
                    l.website, l.official_code, l.department, l.is_pec, l.address, l.postal_code,
                    l.paese, l.run_id, l.method,
                    (SELECT r.source_id FROM search_runs r WHERE r.id=l.run_id) AS source_id
             FROM search_leads l {where_sql}
             GROUP BY l.email, l.email_comparison_key, l.source_url, l.person_name, l.role,
                      l.category, l.subcategory, l.organization, l.comune, l.provincia, l.regione,
                      l.website, l.official_code, l.department, l.is_pec, l.paese, l.method,
                      source_id
             ORDER BY lower(l.email), min(l.rowid)"
        ))
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok(PromotedLead {
                email: row.get(0)?,
                comparison_key: row.get(1)?,
                source_url: row.get(2)?,
                person_name: row.get(3)?,
                role: row.get(4)?,
                category: row.get(5)?,
                subcategory: row.get(6)?,
                organization: row.get(7)?,
                comune: row.get(8)?,
                provincia: row.get(9)?,
                regione: row.get(10)?,
                website: row.get(11)?,
                official_code: row.get(12)?,
                department: row.get(13)?,
                is_pec: row.get::<_, i64>(14)? != 0,
                paese: row.get(17)?,
                run_id: row.get(18)?,
                method: row.get(19)?,
                source_id: row.get(20)?,
            })
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);

    let now = chrono::Utc::now().to_rfc3339();
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut skipped_blacklist = 0usize;
    let mut unverified = 0usize;
    let mut counted = HashSet::new();
    let transaction = db
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    for lead in rows {
        let key = lead.comparison_key.to_ascii_lowercase();
        let first = counted.insert(key.clone());
        if blacklist.contains(&lead.comparison_key) || blacklist.contains(&key) {
            if first {
                skipped_blacklist += 1;
            }
            continue;
        }
        // The contact before this lead: [subcategory, ente, code, comune,
        // provincia, regione, status]. The import merge would join different
        // values with " | "; promotion never does, except for the website.
        let existing: Option<[Option<String>; 7]> = transaction
            .query_row(
                "SELECT subcategory, organization_name, official_code, comune, provincia, regione,
                        status
                 FROM contacts WHERE email_comparison_key=?1",
                params![lead.comparison_key],
                |row| {
                    Ok([
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ])
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let blank = |index: usize| {
            existing.as_ref().is_none_or(|values| {
                values[index]
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
            })
        };
        let fill_place = blank(3) && blank(4) && blank(5);
        let (local, domain, tld) = crate::parsing::email_parts(&lead.email);
        // `merge_status` lets an unverified state win over a known one, so a
        // known contact keeps its own. A new one takes the last DNS result of
        // its domain, even an expired one: Contatti lists valid contacts only.
        let status = match existing.as_ref().and_then(|values| values[6].as_deref()) {
            Some(known) => crate::EmailStatus::from_db(known),
            None => match transaction
                .query_row(
                    "SELECT routing_status FROM domain_verification_cache WHERE domain=?1",
                    params![
                        lead.email
                            .rsplit_once('@')
                            .map_or(String::new(), |(_, host)| host.to_ascii_lowercase())
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| error.to_string())?
                .as_deref()
            {
                Some("routing_ok") => crate::EmailStatus::Valid,
                Some("no_routing") => crate::EmailStatus::DomainMissingMx,
                _ => crate::EmailStatus::DomainUnverified,
            },
        };
        let paese = lead.paese.clone().unwrap_or_else(|| {
            match lead.regione.as_deref() {
                Some(value)
                    if value.eq_ignore_ascii_case("Svizzera") || value.contains("Ticino") =>
                {
                    "Svizzera"
                }
                _ => "Italia",
            }
            .to_owned()
        });
        let contact = crate::ImportContact {
            raw_email: lead.email.clone(),
            normalized_email: lead.email.clone(),
            comparison_key: lead.comparison_key.clone(),
            normalization_method: "canonical".to_owned(),
            email_local_part: local,
            email_domain: domain,
            email_tld: tld,
            status,
            validation_notes: Vec::new(),
            source_file: format!("cerca-lead/{}", lead.run_id).into(),
            category: lead
                .category
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "Varie".to_owned()),
            subcategory: lead.subcategory.clone().filter(|_| blank(0)),
            contact_kind: "non_determinato".to_owned(),
            paese,
            is_pec: lead.is_pec,
            comune: lead.comune.clone().filter(|_| fill_place),
            provincia: lead.provincia.clone().filter(|_| fill_place),
            regione: lead.regione.clone().filter(|_| fill_place),
            macroarea: None,
            website: lead.website.clone(),
            source_url: lead.source_url.clone(),
            source_sheet: None,
            source_row: 0,
            raw_context: "".into(),
            classification_method: "discovery".to_owned(),
            classification_confidence: 0.0,
            classification_evidence: vec![format!(
                "source:cerca-lead:{}",
                lead.method.as_deref().unwrap_or("web")
            )],
        };
        let official = lead
            .source_id
            .as_deref()
            .is_some_and(|id| OFFICIAL_SOURCES.contains(&id));
        let before = existing
            .as_ref()
            .filter(|_| official)
            .map(|values| values[1..6].to_vec());
        let (result, _) =
            crate::contacts::save_contact(&transaction, &contact, "", None, None, &now)?;
        transaction
            .execute(
                // Empty fields of an existing contact take the lead's values;
                // known values are never overwritten here. Address and CAP only
                // serve to resolve the place and stay in the sandbox.
                "UPDATE contacts SET
                    person_name=COALESCE(NULLIF(trim(person_name),''), ?1),
                    role=COALESCE(NULLIF(trim(role),''), ?2),
                    department=COALESCE(NULLIF(trim(department),''), ?3),
                    organization_name=COALESCE(NULLIF(trim(organization_name),''), ?4),
                    official_code=COALESCE(NULLIF(trim(official_code),''), ?5),
                    website=COALESCE(NULLIF(trim(website),''), ?8),
                    is_pec=MAX(is_pec, ?6)
                 WHERE id=?7",
                params![
                    lead.person_name,
                    lead.role,
                    lead.department,
                    lead.organization,
                    lead.official_code,
                    lead.is_pec as i64,
                    result.contact_id,
                    lead.website,
                ],
            )
            .map_err(|error| error.to_string())?;
        // Official registries replace ente, code and place; the replaced
        // values stay readable in the contact's evidence.
        if let Some(before) = before {
            let after = [
                lead.organization.clone(),
                lead.official_code.clone(),
                lead.comune.clone(),
                lead.provincia.clone(),
                lead.regione.clone(),
            ];
            let has_place = after[2].is_some() && after[4].is_some();
            let present = |value: &Option<String>| {
                value
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            let changes: Vec<(&str, Option<String>)> =
                ["ente", "codice", "comune", "provincia", "regione"]
                    .into_iter()
                    .zip(before.iter().zip(&after))
                    .enumerate()
                    .filter(|(index, _)| *index < 2 || has_place)
                    .filter_map(|(_, (label, (old, new)))| {
                        let new = present(new)?;
                        let old = present(old);
                        (old.as_deref() != Some(new.as_str())).then_some((label, old))
                    })
                    .collect();
            let replaced: Vec<String> = changes
                .iter()
                .filter_map(|(label, old)| old.as_ref().map(|old| format!("{label}: {old}")))
                .collect();
            let evidence = (!replaced.is_empty()).then(|| {
                format!(
                    "sostituito da {}: {}",
                    lead.source_id.as_deref().unwrap_or_default(),
                    replaced.join("; ")
                )
            });
            if !changes.is_empty() {
                transaction
                    .execute(
                        "UPDATE contacts SET
                            organization_name=COALESCE(NULLIF(trim(?1),''), organization_name),
                            official_code=COALESCE(NULLIF(trim(?2),''), official_code),
                            comune=CASE WHEN ?6 THEN ?3 ELSE comune END,
                            provincia=CASE WHEN ?6 THEN ?4 ELSE provincia END,
                            regione=CASE WHEN ?6 THEN ?5 ELSE regione END,
                            classification_evidence=CASE WHEN ?7 IS NULL THEN classification_evidence
                                ELSE json_insert(coalesce(NULLIF(classification_evidence,''),'[]'), '$[#]', ?7) END
                         WHERE id=?8",
                        params![
                            after[0],
                            after[1],
                            after[2],
                            after[3],
                            after[4],
                            has_place,
                            evidence,
                            result.contact_id,
                        ],
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        transaction
            .execute(
                "UPDATE contacts SET comune=?1, provincia=?2, regione=?3
                 WHERE id=?4 AND ?3 IS NOT NULL
                   AND trim(coalesce(comune,''))='' AND trim(coalesce(provincia,''))=''
                   AND trim(coalesce(regione,''))=''",
                params![lead.comune, lead.provincia, lead.regione, result.contact_id],
            )
            .map_err(|error| error.to_string())?;
        if first {
            if result.is_new {
                added += 1;
                unverified += (status == crate::EmailStatus::DomainUnverified) as usize;
            } else {
                updated += 1;
            }
        }
    }
    let mut remove = transaction
        .prepare("DELETE FROM search_leads WHERE id=?1")
        .map_err(|error| error.to_string())?;
    for id in &leaving_ids {
        remove
            .execute(params![id])
            .map_err(|error| error.to_string())?;
    }
    drop(remove);
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(PromoteSearchReport {
        added,
        updated,
        skipped_blacklist,
        unverified,
    })
}

pub(super) struct PromotedLead {
    email: String,
    comparison_key: String,
    source_url: Option<String>,
    person_name: Option<String>,
    role: Option<String>,
    category: Option<String>,
    subcategory: Option<String>,
    organization: Option<String>,
    comune: Option<String>,
    provincia: Option<String>,
    regione: Option<String>,
    website: Option<String>,
    official_code: Option<String>,
    department: Option<String>,
    is_pec: bool,
    paese: Option<String>,
    run_id: String,
    method: Option<String>,
    source_id: Option<String>,
}

/// Remove a run with its leads (cascade). Seeds and contacts stay.
pub(crate) fn delete_run(db: &Connection, run_id: &str) -> Result<(), String> {
    db.execute("DELETE FROM search_runs WHERE id=?1", params![run_id])
        .map_err(|error| error.to_string())?;
    Ok(())
}
