use crate::open_db;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
pub(crate) struct CandidateFilters {
    pub query: String,
    pub status: String,
    pub disposition: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Candidate {
    pub id: String,
    pub source_row_id: Option<String>,
    pub raw_email: Option<String>,
    pub canonical_email: Option<String>,
    pub comparison_key: Option<String>,
    pub normalization_method: Option<String>,
    pub extraction_method: Option<String>,
    pub contact_id: Option<String>,
    pub status: String,
    pub syntax_status: Option<String>,
    pub routing_status: Option<String>,
    pub mailbox_status: Option<String>,
    pub candidate_status: String,
    pub disposition: String,
    pub disposition_reason: Option<String>,
    pub source_file: String,
    pub source_row: Option<i64>,
    pub source_sheet: Option<String>,
    pub quality_score: Option<f64>,
    pub score_factors: serde_json::Value,
    pub category: Option<String>,
    pub subcategory: Option<String>,
    pub source_url: Option<String>,
    pub organization_name: Option<String>,
    pub denomination: Option<String>,
    pub person_name: Option<String>,
    pub role: Option<String>,
    pub department: Option<String>,
    pub address: Option<String>,
    pub postal_code: Option<String>,
    pub official_code: Option<String>,
    pub outreach_eligible: bool,
    pub occurrence_index: Option<i64>,
    pub created_at: Option<String>,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CandidateList {
    pub candidates: Vec<Candidate>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_previous: bool,
    pub has_next: bool,
}

fn candidate_view_sql() -> &'static str {
    "WITH candidate_view AS (
       SELECT min(ec.id) AS id,
              min(ec.source_row_id) AS source_row_id,
              coalesce(min(ec.raw_email), min(ec.canonical_email)) AS raw_email,
              min(ec.canonical_email) AS canonical_email,
              ec.comparison_key,
              min(ec.normalization_method) AS normalization_method,
              min(ec.extraction_method) AS extraction_method,
              min(ec.contact_id) AS contact_id,
              ec.status,
              min(ec.syntax_status) AS syntax_status,
              min(ec.routing_status) AS routing_status,
              min(ec.mailbox_status) AS mailbox_status,
              ec.candidate_status,
              ec.current_disposition AS disposition,
              min(ec.disposition_reason) AS disposition_reason,
              min(i.relative_path) AS source_file,
              min(sr.row_number) AS source_row,
              min(sr.source_sheet) AS source_sheet,
              max(ec.quality_score) AS quality_score,
              min(ec.score_factors_json) AS score_factors_json,
              min(ec.category) AS category,
              min(ec.subcategory) AS subcategory,
              min(ec.source_url) AS source_url,
              min(ec.organization_name) AS organization_name,
              min(ec.denomination) AS denomination,
              min(ec.person_name) AS person_name,
              min(ec.role) AS role,
              min(ec.department) AS department,
              min(ec.address) AS address,
              min(ec.postal_code) AS postal_code,
              min(ec.official_code) AS official_code,
              max(ec.outreach_eligible) AS outreach_eligible,
              min(ec.occurrence_index) AS occurrence_index,
              min(ec.first_seen_at) AS created_at,
              max(ec.last_seen_at) AS last_seen_at
       FROM email_candidates ec
       LEFT JOIN source_rows sr ON sr.id=ec.source_row_id
       LEFT JOIN imports i ON i.id=sr.import_id
       WHERE ec.current_disposition <> 'linked'
       GROUP BY coalesce(nullif(ec.comparison_key,''), nullif(ec.canonical_email,''), ec.raw_email), ec.current_disposition
       UNION ALL
       SELECT 'row-error:'||sr.id,sr.id,NULL,NULL,NULL,NULL,NULL,NULL,'parse_error',
              NULL,NULL,NULL,'parse_error','parse_error',
              coalesce(sr.error_message,'Riga non leggibile'),i.relative_path,
              sr.row_number,sr.source_sheet,NULL,'{}',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,
              NULL,NULL,NULL,0,NULL,sr.created_at,sr.created_at
       FROM source_rows sr
       JOIN imports i ON i.id=sr.import_id
       WHERE sr.parse_status IN ('malformed','failed')
     )"
}

fn where_clause(filters: &CandidateFilters) -> (String, Vec<String>) {
    let mut clauses = Vec::new();
    let mut values = Vec::new();
    if !filters.query.trim().is_empty() {
        clauses.push("lower(coalesce(raw_email,'')||' '||coalesce(canonical_email,'')||' '||coalesce(source_file,'')||' '||coalesce(disposition_reason,'')) LIKE ?".to_owned());
        values.push(format!("%{}%", filters.query.trim().to_lowercase()));
    }
    if !filters.status.trim().is_empty() {
        clauses.push("status=?".to_owned());
        values.push(filters.status.trim().to_owned());
    }
    if !filters.disposition.trim().is_empty() {
        clauses.push("disposition=?".to_owned());
        values.push(filters.disposition.trim().to_owned());
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

pub(crate) fn load_candidates(
    db: &Connection,
    filters: &CandidateFilters,
    limit: usize,
    offset: usize,
) -> Result<CandidateList, String> {
    let limit = limit.clamp(1, 500);
    let is_unfiltered = filters.query.trim().is_empty()
        && filters.status.trim().is_empty()
        && filters.disposition.trim().is_empty();

    let total: usize = if is_unfiltered {
        let cand_count: i64 = db
            .query_row(
                "SELECT COUNT(DISTINCT coalesce(nullif(comparison_key,''), nullif(canonical_email,''), raw_email)) FROM email_candidates WHERE current_disposition <> 'linked'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let row_error_count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM source_rows WHERE parse_status IN ('malformed','failed')",
                [],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        (cand_count + row_error_count) as usize
    } else {
        let (where_sql, values) = where_clause(filters);
        db.query_row(
            &format!(
                "{} SELECT COUNT(*) FROM candidate_view {}",
                candidate_view_sql(),
                where_sql,
            ),
            rusqlite::params_from_iter(values.iter()),
            |row| Ok(row.get::<_, i64>(0)? as usize),
        )
        .map_err(|e| e.to_string())?
    };

    let (where_sql, values) = where_clause(filters);
    let mut query_values = values;
    query_values.push(limit.to_string());
    query_values.push(offset.to_string());
    let sql = format!(
        "{} SELECT id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
         extraction_method,contact_id,status,syntax_status,routing_status,mailbox_status,
         candidate_status,disposition,disposition_reason,source_file,source_row,source_sheet,
         quality_score,score_factors_json,category,subcategory,source_url,organization_name,
         denomination,person_name,role,department,address,postal_code,official_code,
         outreach_eligible,occurrence_index,
         created_at,last_seen_at FROM candidate_view {}
         ORDER BY comparison_key, id
         LIMIT CAST(? AS INTEGER) OFFSET CAST(? AS INTEGER)",
        candidate_view_sql(),
        where_sql,
    );
    let mut stmt = db.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(query_values.iter()), |row| {
            let factors: String = row.get(19)?;
            Ok(Candidate {
                id: row.get(0)?,
                source_row_id: row.get(1)?,
                raw_email: row.get(2)?,
                canonical_email: row.get(3)?,
                comparison_key: row.get(4)?,
                normalization_method: row.get(5)?,
                extraction_method: row.get(6)?,
                contact_id: row.get(7)?,
                status: row.get(8)?,
                syntax_status: row.get(9)?,
                routing_status: row.get(10)?,
                mailbox_status: row.get(11)?,
                candidate_status: row.get(12)?,
                disposition: row.get(13)?,
                disposition_reason: row.get(14)?,
                source_file: row
                    .get::<_, Option<String>>(15)?
                    .unwrap_or_else(|| "—".to_owned()),
                source_row: row.get(16)?,
                source_sheet: row.get(17)?,
                quality_score: row.get(18)?,
                score_factors: serde_json::from_str(&factors).unwrap_or(serde_json::json!({})),
                category: row.get(20)?,
                subcategory: row.get(21)?,
                source_url: row.get(22)?,
                organization_name: row.get(23)?,
                denomination: row.get(24)?,
                person_name: row.get(25)?,
                role: row.get(26)?,
                department: row.get(27)?,
                address: row.get(28)?,
                postal_code: row.get(29)?,
                official_code: row.get(30)?,
                outreach_eligible: row.get::<_, i64>(31)? != 0,
                occurrence_index: row.get(32)?,
                created_at: row.get(33)?,
                last_seen_at: row.get(34)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let candidates = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let page_len = candidates.len();
    Ok(CandidateList {
        candidates,
        total,
        limit,
        offset,
        has_previous: offset > 0,
        has_next: offset + page_len < total,
    })
}

#[tauri::command]
pub(crate) async fn list_candidates(
    app: tauri::AppHandle,
    filters: CandidateFilters,
    limit: usize,
    offset: Option<usize>,
) -> Result<CandidateList, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        load_candidates(&db, &filters, limit, offset.unwrap_or(0))
    })
    .await
    .map_err(|e| format!("Caricamento candidati interrotto: {e}"))?
}
