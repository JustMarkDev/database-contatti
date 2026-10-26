export type Contact = {
  id: string;
  email: string;
  email_local_part: string;
  email_domain: string;
  email_tld: string;
  status: string;
  validation_notes: string[];
  source_file: string;
  category: string;
  subcategory: string | null;
  contact_kind: "istituzionale" | "personale" | "non_determinato";
  paese: "Italia" | "Svizzera";
  is_pec: boolean;
  comune: string | null;
  provincia: string | null;
  regione: string | null;
  macroarea: string | null;
  website: string | null;
  source_url: string | null;
  category_id: number | null;
  subcategory_id: number | null;
  classification_method: string;
  classification_confidence: number;
  classification_evidence: string[];
  needs_review: boolean;
  classification_hash: string | null;
  taxonomy_version_id: number | null;
  taxonomy_version: string | null;
  classification_state:
    | "unprocessed"
    | "classified"
    | "not_applicable"
    | "insufficient_evidence"
    | "conflicting"
    | "taxonomy_gap";
  classification_state_reason: string | null;
  raw_email: string | null;
  email_comparison_key: string | null;
  normalization_method: string;
  organization_id: string | null;
  organization_name: string | null;
  denomination: string | null;
  person_name: string | null;
  role: string | null;
  department: string | null;
  address: string | null;
  postal_code: string | null;
  official_code: string | null;
  routing_status: string | null;
  mailbox_status: string | null;
  suppression_status: string;
  first_seen_at: string | null;
  last_seen_at: string | null;
  last_confirmed_at: string | null;
  source_occurrence_count: number;
  independent_source_count: number;
  is_role_mailbox: boolean;
  is_disposable: boolean;
  is_placeholder: boolean;
  is_typo_suspected: boolean;
  /** 0-100, kept up to date by the database; see the README. */
  quality_score: number | null;
};
export type ImportReport = {
  imported: number;
  candidate_occurrences: number;
  unique_normalized_addresses: number;
  new_canonical_contacts: number;
  existing_contacts_updated: number;
  source_occurrences_added: number;
  syntax_invalid_candidates: number;
  no_routing_candidates: number;
  temporary_dns_failures: number;
  suppressed_blacklisted_candidates: number;
  rows_without_address: number;
  malformed_rows: number;
  failed_sheets: number;
  files: Array<{
    name: string;
    parsed_rows: number;
    contacts_found: number;
    unique_normalized_addresses: number;
    new_canonical_contacts: number;
    existing_contacts_updated: number;
    source_occurrences_added: number;
    syntax_invalid_candidates: number;
    no_routing_candidates: number;
    temporary_dns_failures: number;
    suppressed_blacklisted_candidates: number;
    rows_without_address: number;
    malformed_rows: number;
    failed_sheets: number;
    warnings: string[];
  }>;
};
export type ContactList = {
  contacts: Contact[];
  total: number;
  limit: number;
  offset: number;
  has_previous: boolean;
  has_next: boolean;
};
export type SearchRunSummary = {
  run_id: string;
  lead_count: number;
  page_count: number;
  pdf_found: number;
  /** Parts of an import that failed (tables that would not load) while the rest went through. */
  errors: string[];
};
export type PromoteSearchReport = {
  added: number;
  updated: number;
  skipped_blacklist: number;
};
export type SearchLeadView = {
  email: string;
  email_local_part: string;
  email_domain: string;
  email_tld: string;
  email_comparison_key: string;
  source_url: string | null;
  retrieved_at: string | null;
  is_pec: boolean;
  person_name: string | null;
  role: string | null;
  category: string | null;
  subcategory: string | null;
  organization: string | null;
  comune: string | null;
  provincia: string | null;
  regione: string | null;
  official_code: string | null;
  department: string | null;
  website: string | null;
  macroarea: string | null;
  id: string;
  run_id: string;
  run_label: string | null;
  address: string | null;
  postal_code: string | null;
  paese: string | null;
  /** `open_data`, `sito`, `tinyfish`, `archivio`, with `+ia` when the model filled fields. */
  method: string | null;
  /** Runs that found this address. */
  source_count: number;
  in_database: boolean;
  /** `nuovo`, `completa` (promotion fills empty contact fields) or `nel_database`. */
  stato: "nuovo" | "completa" | "nel_database";
};
export type LeadDirectory = {
  id: string;
  name: string;
  publisher: string;
  landing_url: string;
  /** Taxonomy the directory's leads and seeds inherit. */
  category: string;
  subcategory: string;
  note: string;
};
export type LeadFilters = {
  query: string;
  /** Run kinds: `web`, `directory`, `crawl`, `discovery` (`source`: older open-data imports). */
  kind: FieldFilter;
  run: FieldFilter;
  category: FieldFilter;
  subcategory: FieldFilter;
  regione: FieldFilter;
  provincia: FieldFilter;
  method: FieldFilter;
  pec: FieldFilter;
  stato: FieldFilter;
};
export type LeadFilterOptions = {
  run: FilterOption[];
  category: FilterOption[];
  subcategory: FilterOption[];
  regione: FilterOption[];
  provincia: FilterOption[];
  method: FilterOption[];
  pec: FilterOption[];
  stato: FilterOption[];
};
export type SearchRunView = {
  id: string;
  label: string;
  kind: "web" | "source" | "directory" | "crawl" | "discovery";
  status: "running" | "completed" | "cancelled" | "failed";
  lead_count: number;
  page_count: number;
  error: string | null;
  created_at: string;
};
export type SeedListView = {
  list_name: string;
  total: number;
  pending: number;
  crawled: number;
  failed: number;
  without_site: number;
  /** Not confidently classified or not confirmed: stored, never read. */
  to_verify: number;
};
/** What is known and read about one askable subcategory (catch-alls are never listed). */
export type SubcategoryCoverage = {
  category: string;
  subcategory: string;
  /** Distinct sites known, those waiting for a person to check them excluded. */
  sites: number;
  pending: number;
  crawled: number;
  failed: number;
  to_verify: number;
  /** Addresses found on the sites read. */
  leads: number;
};
export type ContextDevSettings = {
  /** Requests in flight the plan allows (Free 2, Developer 10, Pro 100). */
  concurrency: number;
  maps_enabled: boolean;
  /** Credits kept aside: runs refuse to start below it. */
  reserve: number;
  /** Credits one run or discovery may spend; 0 for no cap. */
  max_credits_per_run: number;
};
/** `GET /org/usage` with the reason runs would refuse to start. */
export type ContextDevUsage = {
  usage: {
    credits_remaining: number;
    next_refill: { at: string; credits: number } | null;
    auto_topup: { enabled: boolean; monthly_budget_credits: number | null };
  };
  problem: string | null;
};
export type ExploreEstimate = { sites: number; max_credits: number };
export type ChannelYield = {
  channel: string;
  discovery_credits: number;
  reading_credits: number;
  seeds: number;
  new_emails: number;
};
export type DiscoveryReport = {
  found: number;
  known: number;
  rejected: number;
  accepted: number;
  to_verify: number;
  credits: number;
};
export type CrawlProgress = {
  run_id: string;
  label: string;
  done: number;
  total: number;
  leads: number;
  host: string;
  finished: boolean;
  /** context.dev credits this run has spent so far. */
  credits_used: number;
  credits_remaining: number | null;
};
export type LeadPlan = {
  category: string | null;
  subcategory: string | null;
  regione: string | null;
  source_ids: string[];
  web_query: string;
};
export type SearchLeadList = {
  leads: SearchLeadView[];
  total: number;
  limit: number;
  offset: number;
  has_previous: boolean;
  has_next: boolean;
};
export type FieldFilter = { include: string[]; exclude: string[] };
/** Column sort of the contacts list; `column` is one of the keys in ContactsPage. */
export type ContactSort = { column: string; direction: "asc" | "desc" };
export type Filters = {
  query: string;
  source_file: FieldFilter;
  category: FieldFilter;
  subcategory: FieldFilter;
  contact_kind: FieldFilter;
  paese: FieldFilter;
  macroarea: FieldFilter;
  regione: FieldFilter;
  provincia: FieldFilter;
  pec: FieldFilter;
  classification_method: FieldFilter;
  classification_confidence: FieldFilter;
  /** Minimum contact quality score (0-100). */
  min_score: number | null;
  /** Orders the list only; exports keep their own order. */
  sort: ContactSort | null;
};
export type FilterOption = { value: string; count: number };
export type FilterOptions = {
  source_file: FilterOption[];
  category: FilterOption[];
  subcategory: FilterOption[];
  contact_kind: FilterOption[];
  paese: FilterOption[];
  macroarea: FilterOption[];
  regione: FilterOption[];
  provincia: FilterOption[];
  pec: FilterOption[];
  classification_method: FilterOption[];
  classification_confidence: FilterOption[];
};
export type ImportProgress = {
  phase: string;
  file: string | null;
  processed_files: number;
  total_files: number;
  imported: number;
};
export type AiProgress = {
  phase: string;
  processed: number;
  total: number;
  updated: number;
  skipped: number;
  error: string | null;
  message: string | null;
};
export type AiReport = {
  processed: number;
  updated: number;
  skipped: number;
  errors: string[];
  misclassified: string[];
};
export type AiStartRequest = {
  thinking?: boolean;
  contextSize?: number;
  timeoutSeconds?: number;
  force?: boolean;
  mode?: "fill" | "audit" | "geography" | "full" | "follow_up" | "deterministic_audit";
  provider?: "opencode" | "cascade";
  model?: string;
  effort?: string | null;
  escalationProvider?: "opencode";
  escalationModel?: string;
  escalationEffort?: string | null;
  jevModel?: string;
  confidenceThreshold?: number;
  gapNoulThreshold?: number;
  subcategoryFloor?: number;
  cheapSkipSubAbove?: number;
};
export type AiRun = {
  id: string;
  model: string;
  profile: string;
  mode: "fill" | "audit" | "geography" | "full" | "deterministic_only";
  status:
    | "running"
    | "pausing"
    | "paused"
    | "completed"
    | "completed_with_errors"
    | "cancelled"
    | "failed";
  total: number;
  processed: number;
  updated: number;
  reviews_created: number;
  skipped: number;
  errors: number;
  interruption_reason: string | null;
  started_at?: string;
  provider: string;
  parent_run_id: string | null;
};
export type AiFieldChange = {
  id: string;
  run_id: string;
  contact_id: string;
  review_id: string | null;
  email: string;
  field_name: string;
  old_value: string | null;
  new_value: string | null;
  change_kind: "aggiunto" | "modificato" | "in_revisione" | "saltato";
  status: "applied" | "pending_review" | "rejected" | "skipped";
  confidence: number;
  evidence: string[];
  created_at: string;
  resolved_at: string | null;
  rolled_back?: boolean;
};
export type AiRollbackReport = {
  reverted: number;
  contacts_reverted: number;
  runs_rolled_back: number;
};
export type AiBatchLog = {
  id: string;
  run_id: string;
  taxonomy_version: string | null;
  provider: string;
  model: string;
  status: "dispatched" | "completed" | "failed";
  contact_count: number;
  dispatched_at: string;
  completed_at: string | null;
  system_prompt: string;
  user_prompt: string;
  raw_response: string | null;
  error: string | null;
  attempt: number;
  origin_batch_id: string | null;
  usage: Record<string, unknown>;
  input_bytes: number;
};
export type OpenCodeModelInfo = {
  id: string;
  variants: string[];
};
export type OpenCodeStatus = {
  available: boolean;
  installed: boolean;
  authenticated: boolean;
  models: OpenCodeModelInfo[];
  selected_model: string;
  selected_effort: string | null;
  version: string | null;
  error: string | null;
};
export type TypeSafeStatus = {
  available: boolean;
  configured: boolean;
  model: string;
  error: string | null;
};
export type TypeSafeKeyView = {
  configured: boolean;
  from_env: boolean;
};
export type DeterministicRuleSummary = {
  id: string;
  kind: string;
  evidence_type: string;
  specificity_level: string;
  priority: number;
  output: string;
  overridden_rules: string[];
  match_count: number;
  overridden_count: number;
  conflict_count: number;
};
export type DeterministicCatalog = {
  id: string;
  semantic_version: string;
  status: "pending" | "active" | "retired";
  match_count: number;
  conflict_count: number;
  unresolved_semantic_keys: string[];
  examples: string[];
  rules: DeterministicRuleSummary[];
  active_run: boolean;
  recheck_count: number;
};
export type AiRunFailure = {
  contact_id: string;
  email: string;
  error: string;
  failure_kind: string;
  attempt_count: number;
};
export type BenchmarkReport = {
  cases: number;
  category_precision: number;
  category_coverage: number;
  subcategory_precision: number;
  subcategory_coverage: number;
  automatic_fill_errors: number;
  proposal_coverage: number;
  useful_review_rate: number;
  conflict_quality: number;
  taxonomy_gap_quality: number;
  insufficient_rate: number;
  escalation_rate?: number;
  latency_ms: number;
  prompt_context_chars: number;
};
export type BenchmarkRun = {
  id: string;
  provider: string;
  model: string;
  taxonomy_version: string;
  status: "running" | "pausing" | "paused" | "completed" | "failed" | "cancelled";
  report: BenchmarkReport | null;
  started_at: string;
  finished_at: string | null;
  total: number;
  processed: number;
};
export type Review = {
  id: string;
  contact_id: string;
  email: string;
  current_category: string;
  current_subcategory: string | null;
  proposed_category: string;
  proposed_subcategory: string | null;
  confidence: number;
  evidence?: string[];
  model: string;
  profile: string;
  status: string;
  outcome?: "proposal" | "conflict" | "taxonomy_gap" | "insufficient";

  candidates?: Record<string, unknown>[];
  summary: string | null;
  taxonomy_change: Record<string, unknown> | null;
};
export type BlacklistReport = {
  inserted: number;
  duplicates: number;
  invalid: number;
  removed_contacts: number;
  failed_files: string[];
};
export type BlacklistEntry = {
  email: string;
  source_file: string;
  imported_at: string;
  note: string | null;
};
export type BlacklistList = {
  entries: BlacklistEntry[];
  total: number;
  limit: number;
  offset: number;
  has_previous: boolean;
  has_next: boolean;
};
export type CandidateFilters = {
  query: string;
  status: string;
  disposition: string;
};
export type Candidate = {
  id: string;
  source_row_id: string | null;
  raw_email: string | null;
  canonical_email: string | null;
  comparison_key: string | null;
  normalization_method: string | null;
  extraction_method: string | null;
  contact_id: string | null;
  status: string;
  syntax_status: string | null;
  routing_status: string | null;
  mailbox_status: string | null;
  candidate_status: string;
  disposition: string;
  disposition_reason: string | null;
  source_file: string;
  source_row: number | null;
  source_sheet: string | null;
  quality_score: number | null;
  score_factors: Record<string, unknown> | string[] | null;
  category: string | null;
  subcategory: string | null;
  source_url: string | null;
  organization_name: string | null;
  denomination: string | null;
  person_name: string | null;
  role: string | null;
  department: string | null;
  address: string | null;
  postal_code: string | null;
  official_code: string | null;
  outreach_eligible: boolean;
  occurrence_index: number | null;
  created_at: string | null;
  last_seen_at: string | null;
};
export type CandidateList = {
  candidates: Candidate[];
  total: number;
  limit: number;
  offset: number;
  has_previous: boolean;
  has_next: boolean;
};
export type ExportReport = { path: string; rows_written: number };
export type DatabaseBackupReport = {
  path: string;
  size_bytes: number;
  contact_count: number;
  blacklist_count: number;
};
export type DatabaseRestoreReport = {
  contact_count: number;
  blacklist_count: number;
  candidates_count: number;
};
export type DashboardData = {
  filtered: ContactList;
  database_total: number;
  filters: FilterOptions;
  blacklist_count: number;
  pending_review_count: number;
};
export type ReviewList = {
  reviews: Review[];
  total: number;
};
export type Page = "contacts" | "ai" | "blacklist" | "search" | "settings";
