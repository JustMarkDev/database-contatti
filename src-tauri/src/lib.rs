#[cfg(not(any(target_os = "windows", target_os = "macos")))]
compile_error!("Database Contatti supports only Windows and macOS");

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\p{L}\p{N}._%+\-']+@[\p{L}\p{N}.\-]+\.[\p{L}]{2,}")
        .expect("valid Unicode email regex")
});

static ENTITY_AT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:&#0*64;|&commat;|%40)").expect("valid entity at regex"));
static ENTITY_DOT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:&#0*46;|&period;|%2e)").expect("valid entity dot regex"));

static OBFUSCATED_AT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*(?:\[\s*(?:at|chiocciola|arobase)\s*\]|\(\s*(?:at|chiocciola|arobase)\s*\)|\{\s*(?:at|chiocciola|arobase)\s*\}|<\s*(?:at|chiocciola|arobase)\s*>|_(?:at|chiocciola)_|-(?:at|chiocciola)-)\s*")
        .expect("valid obfuscated at regex")
});
static OBFUSCATED_DOT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*(?:\[\s*(?:dot|punto)\s*\]|\(\s*(?:dot|punto)\s*\)|\{\s*(?:dot|punto)\s*\}|<\s*(?:dot|punto)\s*>|_(?:dot|punto)_|-(?:dot|punto)-)\s*")
        .expect("valid obfuscated dot regex")
});

static WORD_SEPARATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b([A-Z0-9._%+\-']+)[ \t]+(?:at|chiocciola)[ \t]+([A-Z0-9\-]+(?:[ \t]*\.[ \t]*|[ \t]+(?:dot|punto)[ \t]+)[A-Z0-9\-]+(?:(?:[ \t]*\.[ \t]*|[ \t]+(?:dot|punto)[ \t]+)[A-Z0-9\-]+)*)",
    )
    .expect("valid word separator regex")
});
static WORD_DOT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[ \t]*(?:\.|[ \t](?:dot|punto)[ \t])[ \t]*").expect("valid word dot regex")
});

static SPACED_AT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)([A-Z0-9._%+\-']+)[ \t]*@[ \t]*([A-Z0-9\-]+(?:\.[A-Z0-9\-]+)*\.[A-Z]{2,})")
        .expect("valid spaced at regex")
});

static OBFUSCATION_HINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)(?:
            &\#0*(?:64|46);|&(?:commat|period);|%(?:40|2e)|
            [\[\(\{<]\s*(?:at|chiocciola|arobase|dot|punto)\s*[\]\)\}>]|
            _(?:at|chiocciola|dot|punto)_|-(?:at|chiocciola|dot|punto)-|
            \b[A-Z0-9._%+\-']+[\x20\t]+(?:at|chiocciola)[\x20\t]+|
            [\x20\t]+@|@[\x20\t]+
        )",
    )
    .expect("valid obfuscation hint regex")
});
static APP_SESSION_ID: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());
const AI_LEASE_SECONDS: i64 = 45;

#[derive(Debug, Clone)]
struct ImportFile {
    name: String,
    relative_path: String,
    bytes: Vec<u8>,
    /// Set for files found on disk: bytes stay unread until the file's turn.
    path: Option<std::path::PathBuf>,
}

impl ImportFile {
    fn load(mut self) -> Result<Self, String> {
        if let Some(path) = self.path.take() {
            self.bytes = std::fs::read(&path)
                .map_err(|error| format!("Impossibile leggere {}: {error}", path.display()))?;
        }
        Ok(self)
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct FieldFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

/// Column sort of the contacts list. `column` is one of `contact_order_sql`'s keys.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ListSort {
    column: String,
    direction: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ListFilters {
    query: String,
    source_file: FieldFilter,
    category: FieldFilter,
    subcategory: FieldFilter,
    contact_kind: FieldFilter,
    paese: FieldFilter,
    macroarea: FieldFilter,
    comune: FieldFilter,
    provincia: FieldFilter,
    regione: FieldFilter,
    pec: FieldFilter,
    classification_method: FieldFilter,
    classification_confidence: FieldFilter,
    min_score: Option<i64>,
    /// Order of the list only; exports keep their own order.
    sort: Option<ListSort>,
}

#[derive(Debug, Serialize)]
struct ImportContact {
    raw_email: String,

    normalized_email: String,

    comparison_key: String,

    normalization_method: String,
    email_local_part: String,
    email_domain: String,
    email_tld: String,
    status: EmailStatus,
    validation_notes: Vec<String>,
    source_file: std::sync::Arc<str>,
    category: String,
    subcategory: Option<String>,
    contact_kind: String,
    paese: String,
    is_pec: bool,
    comune: Option<String>,
    provincia: Option<String>,
    regione: Option<String>,
    macroarea: Option<String>,
    website: Option<String>,
    source_url: Option<String>,
    source_sheet: Option<String>,
    source_row: usize,
    raw_context: std::sync::Arc<str>,
    classification_method: String,
    classification_confidence: f64,
    classification_evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
struct ContactRecord {
    id: String,
    email: String,
    email_local_part: String,
    email_domain: String,
    email_tld: String,
    status: EmailStatus,
    validation_notes: Vec<String>,
    source_file: String,
    category: String,
    subcategory: Option<String>,
    contact_kind: String,
    paese: String,
    is_pec: bool,
    comune: Option<String>,
    provincia: Option<String>,
    regione: Option<String>,
    macroarea: Option<String>,
    website: Option<String>,
    source_url: Option<String>,
    category_id: Option<i64>,
    subcategory_id: Option<i64>,
    classification_method: String,
    classification_confidence: f64,
    classification_evidence: Vec<String>,
    needs_review: bool,
    classification_hash: Option<String>,
    taxonomy_version_id: Option<i64>,
    taxonomy_version: Option<String>,
    classification_state: String,
    classification_state_reason: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct ContactView {
    #[serde(flatten)]
    contact: ContactRecord,
    raw_email: Option<String>,
    email_comparison_key: Option<String>,
    normalization_method: String,
    organization_id: Option<String>,
    organization_name: Option<String>,
    denomination: Option<String>,
    person_name: Option<String>,
    role: Option<String>,
    department: Option<String>,
    address: Option<String>,
    postal_code: Option<String>,
    official_code: Option<String>,
    routing_status: Option<String>,
    mailbox_status: Option<String>,
    suppression_status: String,
    first_seen_at: Option<String>,
    last_seen_at: Option<String>,
    last_confirmed_at: Option<String>,
    source_occurrence_count: i64,
    independent_source_count: i64,
    is_role_mailbox: bool,
    is_disposable: bool,
    is_placeholder: bool,
    is_typo_suspected: bool,
    quality_score: Option<i64>,
}

impl std::ops::Deref for ContactView {
    type Target = ContactRecord;

    fn deref(&self) -> &Self::Target {
        &self.contact
    }
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EmailStatus {
    Valid,
    InvalidSyntax,
    DomainMissingMx,
    DomainUnverified,
    Blacklisted,
}

impl EmailStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::InvalidSyntax => "invalid_syntax",
            Self::DomainMissingMx => "domain_missing_mx",
            Self::DomainUnverified => "domain_unverified",
            Self::Blacklisted => "blacklisted",
        }
    }

    fn from_db(value: &str) -> Self {
        match value {
            "valid" => Self::Valid,
            "invalid_syntax" => Self::InvalidSyntax,
            "domain_missing_mx" => Self::DomainMissingMx,
            "blacklisted" => Self::Blacklisted,
            _ => Self::DomainUnverified,
        }
    }
}

#[derive(Default)]
struct EmailStatusCounts {
    valid: usize,
    invalid_syntax: usize,
    domain_missing_mx: usize,
    domain_unverified: usize,
    blacklisted: usize,
}

fn email_status_counts(contacts: &[ImportContact]) -> EmailStatusCounts {
    let mut counts = EmailStatusCounts::default();
    for contact in contacts {
        match contact.status {
            EmailStatus::Valid => counts.valid += 1,
            EmailStatus::InvalidSyntax => counts.invalid_syntax += 1,
            EmailStatus::DomainMissingMx => counts.domain_missing_mx += 1,
            EmailStatus::DomainUnverified => counts.domain_unverified += 1,
            EmailStatus::Blacklisted => counts.blacklisted += 1,
        }
    }
    counts
}

#[derive(Debug, Serialize)]
struct ImportReport {
    imported: usize,
    candidate_occurrences: usize,
    unique_normalized_addresses: usize,
    new_canonical_contacts: usize,
    existing_contacts_updated: usize,
    source_occurrences_added: usize,
    syntax_invalid_candidates: usize,
    no_routing_candidates: usize,
    temporary_dns_failures: usize,
    suppressed_blacklisted_candidates: usize,
    rows_without_address: usize,
    malformed_rows: usize,
    failed_sheets: usize,
    files: Vec<FileReport>,
}

#[derive(Debug, Serialize)]
struct FileReport {
    name: String,
    parsed_rows: usize,
    contacts_found: usize,
    unique_normalized_addresses: usize,
    new_canonical_contacts: usize,
    existing_contacts_updated: usize,
    source_occurrences_added: usize,
    syntax_invalid_candidates: usize,
    no_routing_candidates: usize,
    temporary_dns_failures: usize,
    suppressed_blacklisted_candidates: usize,
    rows_without_address: usize,
    malformed_rows: usize,
    failed_sheets: usize,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ContactList {
    contacts: Vec<ContactView>,
    total: usize,
    limit: usize,
    offset: usize,
    has_previous: bool,
    has_next: bool,
}

#[derive(Debug, Serialize)]
struct SearchRunSummary {
    run_id: String,
    lead_count: i64,
    page_count: i64,
    pdf_found: i64,
    /// Parts of an import that failed (a table that would not load); the
    /// rest of the import still went through. Shown to the user as a warning.
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SearchLeadView {
    email: String,
    email_local_part: String,
    email_domain: String,
    email_tld: String,
    email_comparison_key: String,
    source_url: Option<String>,
    retrieved_at: String,
    is_pec: bool,
    person_name: Option<String>,
    role: Option<String>,
    category: Option<String>,
    subcategory: Option<String>,
    organization: Option<String>,
    comune: Option<String>,
    provincia: Option<String>,
    regione: Option<String>,
    official_code: Option<String>,
    department: Option<String>,
    website: Option<String>,
    macroarea: Option<String>,
    id: String,
    run_id: String,
    run_label: Option<String>,
    address: Option<String>,
    postal_code: Option<String>,
    paese: Option<String>,
    method: Option<String>,
    /// Runs that found this address.
    source_count: i64,
    in_database: bool,
    /// `nuovo`, `completa` (promotion fills empty contact fields) or `nel_database`.
    stato: String,
}

#[derive(Debug, Serialize)]
struct SearchLeadList {
    leads: Vec<SearchLeadView>,
    total: i64,
    limit: usize,
    offset: usize,
    has_previous: bool,
    has_next: bool,
}

#[derive(Debug, Serialize)]
struct DashboardData {
    filtered: ContactList,
    database_total: usize,
    filters: FilterOptions,
    blacklist_count: usize,
    pending_review_count: usize,
}

#[derive(Debug, Serialize, Clone)]
struct ImportProgress {
    phase: String,
    file: Option<String>,
    processed_files: usize,
    total_files: usize,
    imported: usize,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default, rename_all = "camelCase")]
struct AiClassificationRequest {
    thinking: bool,
    context_size: usize,
    timeout_seconds: u64,
    force: bool,
    mode: String,
    #[serde(flatten)]
    selection: ai_selection::ProviderSelection,
}

impl Default for AiClassificationRequest {
    fn default() -> Self {
        Self {
            thinking: true,
            context_size: 4096,
            timeout_seconds: 600,
            force: false,
            mode: "fill".to_owned(),
            selection: ai_selection::ProviderSelection::default(),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
struct AiProgress {
    phase: String,
    processed: usize,
    total: usize,
    updated: usize,
    skipped: usize,
    error: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct AiEnrichmentReport {
    processed: usize,
    updated: usize,
    skipped: usize,
    errors: Vec<String>,
    misclassified: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
struct AiRollbackReport {
    reverted: usize,
    contacts_reverted: usize,
    runs_rolled_back: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct GroundedSourceReference {
    pub source_id: String,
    pub field: String,
    pub quote: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TaxonomyCandidate {
    pub category_key: String,
    #[serde(default)]
    pub subcategory_key: Option<String>,

    #[serde(default)]
    pub category_grade: EvidenceGrade,
    #[serde(default)]
    pub subcategory_grade: Option<EvidenceGrade>,
    #[serde(default)]
    pub category_evidence: Vec<GroundedSourceReference>,
    #[serde(default)]
    pub subcategory_evidence: Vec<GroundedSourceReference>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub enum EvidenceGrade {
    #[serde(rename = "direct")]
    Direct,
    #[serde(rename = "corroborated")]
    Corroborated,
    #[serde(rename = "weak")]
    #[default]
    Weak,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum TaxonomyOutcome {
    #[serde(rename = "proposal")]
    Proposal,
    #[serde(rename = "conflict")]
    Conflict,
    #[serde(rename = "taxonomy_gap")]
    TaxonomyGap,
    #[serde(rename = "insufficient")]
    Insufficient,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaxonomyGap {
    pub level: String,
    #[serde(default)]
    pub nearest_category_key: Option<String>,
    #[serde(default)]
    pub nearest_subcategory_key: Option<String>,
    pub proposed_display_name: String,
    pub proposed_definition: String,
    #[serde(default)]
    pub parent_category_key: Option<String>,
    #[serde(default)]
    pub evidence: Vec<GroundedSourceReference>,
}

impl<'de> Deserialize<'de> for TaxonomyGap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TaxonomyGapVisitor;
        impl<'de> serde::de::Visitor<'de> for TaxonomyGapVisitor {
            type Value = TaxonomyGap;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or a taxonomy gap object")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let text = v.trim();
                let is_sub =
                    text.to_lowercase().contains("subcat") || text.to_lowercase().contains("sotto");
                Ok(TaxonomyGap {
                    level: if is_sub {
                        "subcategory".to_string()
                    } else {
                        "category".to_string()
                    },
                    nearest_category_key: None,
                    nearest_subcategory_key: None,
                    proposed_display_name: text.to_string(),
                    proposed_definition: text.to_string(),
                    parent_category_key: None,
                    evidence: Vec::new(),
                })
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut level = None;
                let mut nearest_cat = None;
                let mut nearest_sub = None;
                let mut display_name = None;
                let mut definition = None;
                let mut parent_cat = None;
                let mut evidence = Vec::new();

                while let Some(key) = map.next_key::<String>()? {
                    let k_lower = key.to_ascii_lowercase();
                    match k_lower.as_str() {
                        "level" => level = map.next_value()?,
                        "nearest_category_key" => nearest_cat = map.next_value()?,
                        "nearest_subcategory_key" => nearest_sub = map.next_value()?,
                        "parent_category_key" => parent_cat = map.next_value()?,
                        "proposed_display_name"
                        | "suggested_category"
                        | "missing_subcategory"
                        | "suggested_subcategory"
                        | "name" => {
                            display_name = map.next_value()?;
                        }
                        "category" | "category_key" => {
                            let cat_val: String = map.next_value()?;
                            if display_name.is_none() {
                                display_name = Some(cat_val);
                            }
                        }
                        "proposed_definition" | "description" | "reason" => {
                            definition = map.next_value()?
                        }
                        "evidence" => evidence = map.next_value()?,
                        _ => {
                            let _: serde_json::Value = map.next_value()?;
                        }
                    }
                }

                let disp = display_name
                    .or_else(|| definition.clone())
                    .unwrap_or_else(|| "Taxonomy Gap".to_string());
                let def = definition.unwrap_or_else(|| disp.clone());
                let lev = level.unwrap_or_else(|| {
                    if parent_cat.is_some() || disp.to_lowercase().contains("sub") {
                        "subcategory".to_string()
                    } else {
                        "category".to_string()
                    }
                });

                Ok(TaxonomyGap {
                    level: lev,
                    nearest_category_key: nearest_cat,
                    nearest_subcategory_key: nearest_sub,
                    proposed_display_name: disp,
                    proposed_definition: def,
                    parent_category_key: parent_cat,
                    evidence,
                })
            }
        }

        deserializer.deserialize_any(TaxonomyGapVisitor)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TaxonomyResult {
    pub outcome: TaxonomyOutcome,
    #[serde(default)]
    pub candidates: Vec<TaxonomyCandidate>,
    #[serde(default)]
    pub gap: Option<TaxonomyGap>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(not(test), allow(dead_code))]
struct AiContactSuggestion {
    tipo_email: Option<String>,
    categoria: Option<String>,
    sottocategoria: Option<String>,
    tipo_contatto: Option<String>,
    paese: Option<String>,
    comune: Option<String>,
    provincia: Option<String>,
    regione: Option<String>,
    confidence: Option<f64>,
    evidence: Option<Vec<String>>,
    reason_code: Option<String>,
    conflict_with_existing: Option<bool>,
}

#[derive(Debug, Serialize)]
struct FilterOption {
    value: String,
    count: usize,
}

#[derive(Debug, Serialize, Default)]
struct FilterOptions {
    source_file: Vec<FilterOption>,
    category: Vec<FilterOption>,
    subcategory: Vec<FilterOption>,
    contact_kind: Vec<FilterOption>,
    paese: Vec<FilterOption>,
    macroarea: Vec<FilterOption>,
    regione: Vec<FilterOption>,
    provincia: Vec<FilterOption>,
    comune: Vec<FilterOption>,
    pec: Vec<FilterOption>,
    classification_method: Vec<FilterOption>,
    classification_confidence: Vec<FilterOption>,
}

#[derive(Debug, Serialize)]
struct BlacklistImportReport {
    inserted: usize,
    duplicates: usize,
    invalid: usize,
    removed_contacts: usize,
    /// `path: error` for files that could not be read or saved; the others are kept.
    failed_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BlacklistEntry {
    email: String,
    source_file: String,
    imported_at: String,
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct BlacklistList {
    entries: Vec<BlacklistEntry>,
    total: usize,
    limit: usize,
    offset: usize,
    has_previous: bool,
    has_next: bool,
}

#[derive(Debug, Serialize)]
struct ExportReport {
    path: String,
    rows_written: usize,
}

const MAX_PAGE_SIZE: usize = 100;
#[cfg(test)]
static FACET_LOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Serialize)]
struct ClassificationReviewList {
    reviews: Vec<ClassificationReview>,
    total: usize,
}

#[derive(Debug, Serialize)]
struct ClassificationReview {
    id: String,
    contact_id: String,
    email: String,
    current_category: String,
    current_subcategory: Option<String>,
    proposed_category: String,
    proposed_subcategory: Option<String>,
    confidence: f64,
    evidence: Vec<String>,
    model: String,
    profile: String,
    status: String,
    outcome: String,
    candidates: Vec<serde_json::Value>,
    summary: Option<String>,
    taxonomy_change: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct AiRunStatus {
    id: String,
    model: String,
    profile: String,
    status: String,
    total: usize,
    processed: usize,
    updated: usize,
    reviews_created: usize,
    skipped: usize,
    errors: usize,
    interruption_reason: Option<String>,
    started_at: String,
    provider: String,
    parent_run_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AiFieldChange {
    id: String,
    run_id: String,
    contact_id: String,
    review_id: Option<String>,
    email: String,
    field_name: String,
    old_value: Option<String>,
    new_value: Option<String>,
    change_kind: String,
    status: String,
    confidence: f64,
    evidence: Vec<String>,
    created_at: String,
    resolved_at: Option<String>,
    rolled_back: bool,
}

#[derive(Debug, Serialize)]
struct AiFieldChangeList {
    changes: Vec<AiFieldChange>,
    total: usize,
}

pub mod ai_batch;
pub mod ai_cascade;
pub mod ai_commands;
pub mod ai_enrichment;
pub mod ai_opencode;
mod ai_provider;
mod ai_registry;
pub mod ai_runs;
pub mod ai_selection;
pub(crate) mod ai_typesafe;
pub mod benchmark;
pub mod blacklist;
pub mod candidates;
mod classification_benchmarks;
mod classification_core;
pub mod contacts;
// shortcut: the client is unused until the crawler switches to it (plan phase 2), drop then.
#[allow(dead_code)]
mod contextdev;
pub mod database;
pub mod database_backup;
pub mod deterministic_rules;
mod discovery;
pub mod gazetteer;
pub mod imports;
mod lead_ai;
mod lead_candidates;
mod lead_crawl;
mod lead_sources;
pub mod mailbox_verification;
mod mcp;
pub mod parsing;
pub mod schema;
pub mod taxonomy;

pub(crate) use ai_enrichment::{
    load_ai_candidate, load_ai_candidate_ids, normalize_extracted_province,
};
pub(crate) use ai_runs::{count_classification_reviews, process_ai_run};
pub(crate) use contacts::{
    filter_where, load_contacts, load_contacts_page, new_contact_id, simple_hash, standard_category,
};
pub(crate) use database::{claim_ai_run, normalize_orphaned_ai_runs, open_db, renew_ai_lease};
pub use mcp::run_mcp;
pub(crate) use parsing::{
    DELIMITED_EXTENSIONS, SPREADSHEET_EXTENSIONS, display_name, is_blacklist_file, normalize_key,
    parse_file,
};
pub use schema::{TAXONOMY, has_table};

#[cfg(test)]
mod benchmark_tests;
#[cfg(test)]
mod cascade_live_benchmark;
#[cfg(test)]
mod tests_corpus;
#[cfg(test)]
mod tests_features;
#[cfg(test)]
mod tests_performance;
#[cfg(test)]
mod tests_schema;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if let Err(error) = open_db(&handle) {
                    eprintln!("[db] Pre-warm database fallito: {error}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            imports::import_contact_path,
            imports::import_contact_paths,
            imports::control_import,
            imports::list_contacts,
            candidates::list_candidates,
            imports::export_contacts_csv,
            imports::export_contact_emails_csv,
            discovery::start_search_run,
            discovery::list_search_leads,
            discovery::get_lead_filter_options,
            discovery::list_search_runs,
            discovery::delete_search_run,
            discovery::export_search_leads_txt,
            discovery::promote_search_leads,
            lead_sources::list_lead_sources,
            lead_sources::import_lead_source,
            lead_crawl::list_seed_lists,
            lead_crawl::delete_seed_list,
            lead_crawl::add_contact_seeds,
            lead_crawl::add_seed_urls,
            lead_crawl::explore_seeds,
            lead_crawl::explore_directory,
            lead_crawl::find_seed_sites,
            lead_crawl::plan_lead_request,
            lead_crawl::cancel_lead_crawl,
            lead_crawl::list_lead_taxonomy,
            lead_candidates::discover_by_search,
            contextdev::get_contextdev_api_key,
            contextdev::set_contextdev_api_key,
            imports::get_filter_options,
            imports::refresh_dashboard,
            imports::delete_contact,
            imports::delete_all_invalid,
            imports::clear_database,
            database_backup::export_database_backup,
            database_backup::import_database_backup,
            ai_commands::start_ai_classification,
            ai_commands::start_deterministic_classification,
            ai_commands::start_geographic_enrichment,
            ai_runs::resume_ai_classification,
            ai_runs::get_ai_run_status,
            ai_runs::list_ai_runs,
            ai_runs::delete_ai_run,
            ai_runs::get_resumable_ai_run,
            ai_runs::pause_ai_classification,
            ai_runs::cancel_ai_classification,
            ai_runs::reset_ai_classification,
            ai_runs::rollback_ai_run,
            ai_runs::rollback_ai_runs,
            ai_runs::list_ai_field_changes,
            ai_runs::list_ai_run_batches,
            ai_runs::list_ai_run_failures,
            ai_commands::retry_failed_contacts,
            ai_commands::start_deterministic_recheck,
            classification_benchmarks::run_classification_benchmark,
            classification_benchmarks::list_classification_benchmarks,
            classification_benchmarks::pause_classification_benchmark,
            classification_benchmarks::resume_classification_benchmark,
            classification_benchmarks::cancel_classification_benchmark,
            ai_opencode::detect_opencode,
            ai_typesafe::detect_typesafe,
            ai_typesafe::get_typesafe_api_key,
            ai_typesafe::set_typesafe_api_key,
            deterministic_rules::list_deterministic_catalogs,
            deterministic_rules::activate_deterministic_catalog,
            ai_runs::list_classification_reviews,
            ai_runs::approve_classification_review,
            ai_runs::reject_classification_review,
            blacklist::import_blacklist_paths,
            blacklist::list_blacklist,
            imports::export_blacklist_csv,
            imports::export_blacklist_emails_csv,
            blacklist::add_blacklist_entry,
            blacklist::remove_blacklist_entry,
            blacklist::clear_blacklist,
            blacklist::get_blacklist_stats,
            mailbox_verification::get_mailbox_verification_overview,
            mailbox_verification::save_mailbox_verification_settings,
            mailbox_verification::start_mailbox_verification,
            mailbox_verification::control_mailbox_verification,
            mailbox_verification::get_mailbox_verification_status,
            mailbox_verification::revalidate_mailbox_dns,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                match open_db(app).and_then(|db| {
                    ai_runs::pause_owned_ai_runs_on_shutdown(&db, APP_SESSION_ID.as_str())
                }) {
                    Ok(paused) if paused > 0 => {
                        println!("[ai] {paused} classificazione messa in pausa alla chiusura");
                    }
                    Ok(_) => {}
                    Err(error) => eprintln!(
                        "[ai] Impossibile mettere in pausa la classificazione alla chiusura: {error}"
                    ),
                }
                if let Ok(db) = open_db(app) {
                    let _ = mailbox_verification::pause_running_jobs(&db);
                }
            }
        });
}

#[cfg(test)]
mod ai_request_tests {
    use super::AiClassificationRequest;

    #[test]
    fn deserializes_camel_case_payload() {
        let request: AiClassificationRequest = serde_json::from_value(serde_json::json!({
            "thinking": false,
            "contextSize": 2048,
            "timeoutSeconds": 120,
            "force": true,
            "mode": "audit",
            "provider": "cascade"
        }))
        .expect("camelCase payload deserializes");
        assert!(!request.thinking);
        assert_eq!(request.context_size, 2048);
        assert_eq!(request.timeout_seconds, 120);
        assert!(request.force);
        assert_eq!(request.mode, "audit");
    }

    #[test]
    fn applies_defaults_when_optional_fields_missing() {
        let request: AiClassificationRequest = serde_json::from_value(serde_json::json!({
            "mode": "fill"
        }))
        .expect("minimal payload deserializes");
        assert_eq!(request.context_size, 4096);
        assert_eq!(request.timeout_seconds, 600);
    }
}
