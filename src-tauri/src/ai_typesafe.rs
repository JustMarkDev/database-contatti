use crate::ai_provider::ProviderError;
use crate::ai_runs::{GeographicPlaceClaim, GeographicResult, UnifiedResult};
use crate::{
    EvidenceGrade, GroundedSourceReference, TaxonomyCandidate, TaxonomyOutcome, TaxonomyResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const TYPESAFE_PROVIDER_ID: &str = "typesafe";
pub const TYPESAFE_SETTINGS_KEY: &str = "typesafe_api_key";
const TYPESAFE_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// Contacts per System One request. Taxonomy lives once in shared state.
/// Matches the runner batch (32) so each batch is one pass-1 call, not 24 + 8.
pub const JEV_CONTACT_BATCH: usize = 32;

/// Gates for “spend only where it matters”: cheap category-only vs paid subcategory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpendPolicy {
    pub apply_confidence: f64,
    pub subcategory_floor: f64,
    pub cheap_skip_sub_above: f64,
    pub gap_noul_threshold: f64,
}

impl Default for SpendPolicy {
    fn default() -> Self {
        Self {
            apply_confidence: crate::ai_selection::DEFAULT_CASCADE_CONFIDENCE,
            subcategory_floor: crate::ai_selection::DEFAULT_SUBCATEGORY_FLOOR,
            cheap_skip_sub_above: crate::ai_selection::DEFAULT_CHEAP_SKIP_SUB_ABOVE,
            gap_noul_threshold: crate::ai_selection::DEFAULT_GAP_NOUL_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldProfile {
    /// Pass 1: email identity + denom + geo.
    Slim,
    /// Paid subcategory / rich retry: add address, site, phone, name.
    Full,
}
const MAX_FIELD_CHARS: usize = 120;

#[derive(Debug, Serialize)]
pub struct TypeSafeStatus {
    pub available: bool,
    pub configured: bool,
    pub model: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TypeSafeClient {
    api_key: String,
    model: String,
    http: reqwest::Client,
    warmed: Arc<AtomicBool>,
    last_usage: Arc<Mutex<Option<Value>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChoiceAnswer {
    pub choice: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    #[allow(dead_code)]
    pub probabilities: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NoulAnswer {
    pub noul: f64,
}

#[derive(Debug, Clone)]
pub struct CascadeJudgment {
    pub disposition: ChoiceAnswer,
    pub category: ChoiceAnswer,
    pub subcategory: ChoiceAnswer,
    pub needs_taxonomy_gap: NoulAnswer,
    pub country: Option<ChoiceAnswer>,
    pub municipality: Option<ChoiceAnswer>,
    pub province: Option<ChoiceAnswer>,
    pub region: Option<ChoiceAnswer>,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CascadeAction {
    Apply,
    Escalate,
    Insufficient,
    Conflict,
}

pub fn resolve_api_key(db_key: Option<String>) -> Option<String> {
    std::env::var("TYPESAFE_API_KEY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            db_key
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
}

impl TypeSafeClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            // One pooled connection so concurrent calls multiplex on HTTP/2 instead of
            // each paying a fresh TLS handshake (~400ms from EU to TypeSafe's US edge).
            .pool_max_idle_per_host(1)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .http2_adaptive_window(true)
            .http2_keep_alive_interval(Duration::from_secs(20))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            api_key: api_key.into(),
            model: model.into(),
            http,
            warmed: Arc::new(AtomicBool::new(false)),
            last_usage: Arc::new(Mutex::new(None)),
        }
    }

    #[allow(dead_code)] // retained for diagnostics / future UI metering
    pub fn take_last_usage(&self) -> Option<Value> {
        self.last_usage.lock().ok()?.take()
    }

    pub async fn ensure_warm(&self) {
        if self.warmed.load(Ordering::Relaxed) {
            return;
        }
        let mut questions = Map::new();
        questions.insert(
            "ready".to_owned(),
            json!({
                "type": "noul",
                "instructions": "Is the service ready?",
                "criteria": { "true": "ready", "false": "not ready" }
            }),
        );
        let _ = self.system_one(json!("warmup"), questions).await;
        self.warmed.store(true, Ordering::Relaxed);
    }

    pub async fn system_one(
        &self,
        state: Value,
        questions: Map<String, Value>,
    ) -> Result<Value, ProviderError> {
        // TypeSafe docs: 429/529 need exponential backoff. Keep short local
        // retries here so a brief Jev overload does not escalate to OpenCode.
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            match self.system_one_once(state.clone(), questions.clone()).await {
                Ok(value) => return Ok(value),
                Err(error) if error.is_auto_retryable_limit() && attempt < MAX_ATTEMPTS => {
                    let wait = std::time::Duration::from_millis(
                        1_000_u64.saturating_mul(1_u64 << (attempt - 1).min(4)),
                    );
                    tokio::time::sleep(wait).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn system_one_once(
        &self,
        state: Value,
        questions: Map<String, Value>,
    ) -> Result<Value, ProviderError> {
        let body = json!({
            "state": state,
            "model": self.model,
            "questions": questions,
        });
        let started = std::time::Instant::now();
        let response = self
            .http
            .post(TYPESAFE_ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                // Timeouts and transport blips are usually Jev overload. Treat them
                // as wait-and-retry so the cascade does not fall through to Muse Spark.
                if error.is_timeout() {
                    ProviderError::rate_limited(format!(
                        "TypeSafe: timeout o servizio sovraccarico: {error}"
                    ))
                } else {
                    ProviderError::rate_limited(format!(
                        "TypeSafe: errore di rete o servizio sovraccarico: {error}"
                    ))
                }
            })?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            ProviderError::unavailable(format!("TypeSafe: risposta non leggibile: {error}"))
        })?;
        let rtt_ms = started.elapsed().as_secs_f64() * 1_000.0;
        if let Some(error) = status_error(status.as_u16(), &text) {
            return Err(error);
        }
        let mut value: Value = serde_json::from_str(&text)
            .map_err(|error| ProviderError::terminal(format!("TypeSafe JSON: {error}")))?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "_client_rtt_ms".to_owned(),
                Value::from((rtt_ms * 100.0).round() / 100.0),
            );
        }
        record_usage(&self.last_usage, &value);
        Ok(value)
    }
}

/// Credit exhaustion (402/403) pauses for a manual Riprendi. Rate/overload
/// (429/529) is retryable so the run waits instead of dumping work on OpenCode.
fn status_error(status: u16, text: &str) -> Option<ProviderError> {
    match status {
        200..=299 => None,
        402 | 403 => Some(ProviderError::quota(format!(
            "TypeSafe credito esaurito o accesso negato ({status}): {text}"
        ))),
        429 | 503 | 529 => Some(ProviderError::rate_limited(format!(
            "TypeSafe: limite di frequenza o servizio sovraccarico ({status}): {text}"
        ))),
        401 => Some(ProviderError::terminal(
            "Chiave API TypeSafe non valida o mancante",
        )),
        500..=599 if looks_like_overload(text) => Some(ProviderError::rate_limited(format!(
            "TypeSafe: limite di frequenza o servizio sovraccarico ({status}): {text}"
        ))),
        _ => Some(ProviderError::terminal(format!(
            "TypeSafe HTTP {status}: {text}"
        ))),
    }
}

fn looks_like_overload(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "overloaded",
        "server load",
        "high load",
        "rate limit",
        "too many requests",
        "try again later",
        "temporarily unavailable",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn record_usage(slot: &Arc<Mutex<Option<Value>>>, response: &Value) {
    let Some(usage) = response.get("usage") else {
        return;
    };
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    match guard.as_mut() {
        Some(Value::Object(existing)) => {
            let prev_in = existing
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let prev_out = existing
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let prev_calls = existing.get("calls").and_then(Value::as_u64).unwrap_or(0);
            existing.insert("input_tokens".to_owned(), json!(prev_in + input));
            existing.insert("output_tokens".to_owned(), json!(prev_out + output));
            existing.insert("calls".to_owned(), json!(prev_calls + 1));
        }
        _ => {
            *guard = Some(json!({
                "input_tokens": input,
                "output_tokens": output,
                "calls": 1,
            }));
        }
    }
}

/// Flatten import rows into the few fields Jev needs. Drops paths, duplicates, and junk columns.
pub fn compact_jev_fields(source_context: &str) -> Map<String, Value> {
    compact_jev_fields_with_profile(source_context, FieldProfile::Slim)
}

pub fn compact_jev_fields_with_profile(
    source_context: &str,
    profile: FieldProfile,
) -> Map<String, Value> {
    const SLIM: &[&str] = &[
        "Email",
        "PEC",
        "Local",
        "Denominazione",
        "Comune",
        "Provincia",
        "Regione",
        "Domain",
    ];
    const FULL: &[&str] = &[
        "Email",
        "PEC",
        "Local",
        "Denominazione",
        "Nome",
        "Indirizzo",
        "CAP",
        "Comune",
        "Provincia",
        "Regione",
        "Sito Web",
        "Telefono",
        "Domain",
    ];
    let keep = match profile {
        FieldProfile::Slim => SLIM,
        FieldProfile::Full => FULL,
    };
    let Ok(records) = serde_json::from_str::<Vec<Value>>(source_context) else {
        let mut fields = Map::new();
        let trimmed = source_context.trim();
        if !trimmed.is_empty() {
            fields.insert(
                "text".to_owned(),
                Value::String(truncate_chars(trimmed, MAX_FIELD_CHARS)),
            );
        }
        return fields;
    };

    let mut ordered = records;
    ordered.sort_by_key(|record| {
        let id = record
            .get("source_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if id == "canonical_record" { 0 } else { 1 }
    });

    let mut fields = Map::new();
    for record in ordered {
        let Some(map) = record.get("fields").and_then(Value::as_object) else {
            continue;
        };
        for (key, value) in map {
            let Some(text) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) else {
                continue;
            };
            let canonical = canonicalize_compact_key(key);
            if !keep
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&canonical))
            {
                continue;
            }
            if fields.contains_key(&canonical) {
                continue;
            }
            fields.insert(
                canonical,
                Value::String(truncate_chars(text, MAX_FIELD_CHARS)),
            );
        }
    }
    if let Some(email) = fields
        .get("Email")
        .or_else(|| fields.get("PEC"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        && let Some((local, domain)) = email.split_once('@')
    {
        let local = local.trim();
        let domain = domain.trim();
        if !local.is_empty() {
            fields
                .entry("Local".to_owned())
                .or_insert_with(|| Value::String(local.to_owned()));
        }
        if !domain.is_empty() {
            fields
                .entry("Domain".to_owned())
                .or_insert_with(|| Value::String(domain.to_ascii_lowercase()));
        }
    }
    if let Some(Value::String(denomination)) = fields.get_mut("Denominazione") {
        let cleaned = clean_denomination(denomination);
        if !cleaned.is_empty() {
            *denomination = truncate_chars(&cleaned, MAX_FIELD_CHARS);
        }
    }
    fields
}

fn clean_denomination(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn canonicalize_compact_key(key: &str) -> String {
    let lower = key.to_ascii_lowercase();
    if lower.contains("pec") && !lower.contains("spec") {
        return "PEC".to_owned();
    }
    if lower.contains("mail") || lower == "email" {
        return "Email".to_owned();
    }
    if lower.contains("denomin") || lower.contains("ragione") || lower == "ente" {
        return "Denominazione".to_owned();
    }
    if lower.contains("indirizz") || lower.contains("address") || lower.contains("via") {
        return "Indirizzo".to_owned();
    }
    if lower == "cap" || lower.contains("postal") {
        return "CAP".to_owned();
    }
    if lower.contains("comune") || lower.contains("municip") || lower.contains("citt") {
        return "Comune".to_owned();
    }
    if lower.contains("provincia") || lower.contains("province") {
        return "Provincia".to_owned();
    }
    if lower.contains("regione") || lower.contains("region") || lower.contains("canton") {
        return "Regione".to_owned();
    }
    if lower.contains("sito") || lower.contains("website") || lower.contains("url") {
        return "Sito Web".to_owned();
    }
    if lower.contains("telefon") || lower.contains("phone") {
        return "Telefono".to_owned();
    }
    if lower == "nome" || lower == "name" {
        return "Nome".to_owned();
    }
    if lower == "domain" || lower == "dominio" {
        return "Domain".to_owned();
    }
    if lower == "local" || lower == "localpart" || lower == "local-part" {
        return "Local".to_owned();
    }
    key.trim().to_owned()
}

fn truncate_chars(value: &str, max: usize) -> String {
    let mut out: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        out.push('…');
    }
    out
}

/// Shared taxonomy state. Pass `subcategory_categories = Some(&[])` to omit every
/// subcategory tree (pass 1). Pass `Some(&[cat])` to include only those trees.
pub fn taxonomy_state_blob_filtered(
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
    subcategory_categories: Option<&[String]>,
    include_all_subcategories: bool,
) -> Value {
    let mut categories = Map::new();
    let mut subcategories = Map::new();
    for (category, subs) in taxonomy {
        let label = display_names
            .get(&("category".to_owned(), None, category.clone()))
            .cloned()
            .unwrap_or_else(|| category.clone());
        categories.insert(category.clone(), Value::String(label));
        let include_subs = include_all_subcategories
            || subcategory_categories
                .is_some_and(|wanted| wanted.iter().any(|key| key == category));
        if !include_subs || subs.is_empty() {
            continue;
        }
        let mut sub_map = Map::new();
        for subcategory in subs {
            let label = display_names
                .get(&(
                    "subcategory".to_owned(),
                    Some(category.clone()),
                    subcategory.clone(),
                ))
                .cloned()
                .unwrap_or_else(|| subcategory.clone());
            sub_map.insert(subcategory.clone(), Value::String(label));
        }
        subcategories.insert(category.clone(), Value::Object(sub_map));
    }
    json!({
        "categories": categories,
        "subcategories": subcategories,
    })
}

fn category_labeled_criteria(
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
) -> Map<String, Value> {
    let mut criteria = Map::new();
    for (category, _) in taxonomy {
        let label = display_names
            .get(&("category".to_owned(), None, category.clone()))
            .cloned()
            .unwrap_or_else(|| category.clone());
        criteria.insert(category.clone(), Value::String(label));
    }
    criteria.insert(
        "none_fit".to_owned(),
        Value::String("Nessuna categoria attiva descrive questo ente".to_owned()),
    );
    criteria
}

/// Pass-1: category + ambiguous geography only. Disposition/gap are synthesized in code
/// or asked in a rare follow-up, which is what makes the hot path cheap.
pub fn build_batched_pass1_questions(
    batch_len: usize,
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
    places: &[PlaceCandidates],
) -> Map<String, Value> {
    debug_assert_eq!(batch_len, places.len());
    let category_criteria = category_labeled_criteria(taxonomy, display_names);
    let mut questions = Map::new();
    for index in 0..batch_len {
        let contact_path = format!("contacts[{index}]");
        questions.insert(
            format!("category__{index}"),
            json!({
                "type": "choice",
                "instructions": format!(
                    "Which taxonomy category best fits `{contact_path}.fields`? Use email domain and denomination as primary evidence. Prefer the most specific sector over generic public administration. Choose none_fit only when none cover the entity."
                ),
                "criteria": category_criteria.clone()
            }),
        );
        questions.insert(format!("country__{index}"), country_question(index));
        if let Some(place) = places.get(index) {
            for (key, question) in build_geography_questions(&ambiguous_places_for_jev(place)) {
                questions.insert(format!("{key}__{index}"), question);
            }
        }
    }
    questions
}

pub fn build_batched_gap_questions(indices: &[usize]) -> Map<String, Value> {
    let mut questions = Map::new();
    for index in indices {
        questions.insert(
            format!("needs_taxonomy_gap__{index}"),
            json!({
                "type": "noul",
                "instructions": format!(
                    "Does `contacts[{index}].fields` need a new taxonomy entry missing from `taxonomy.categories`?"
                ),
                "criteria": {
                    "true": "A concrete additive label would help and is not covered",
                    "false": "Active taxonomy or a catch-all already covers it, or evidence is weak"
                }
            }),
        );
    }
    questions
}

/// Keep only place levels that need a model Choice (2+ candidates). Unique hits resolve locally.
pub fn ambiguous_places_for_jev(places: &PlaceCandidates) -> PlaceCandidates {
    PlaceCandidates {
        municipalities: if places.municipalities.len() > 1 {
            places.municipalities.clone()
        } else {
            Vec::new()
        },
        admin_areas: if places.admin_areas.len() > 1 {
            places.admin_areas.clone()
        } else {
            Vec::new()
        },
        regions: if places.regions.len() > 1 {
            places.regions.clone()
        } else {
            Vec::new()
        },
        admin_level: places.admin_level,
    }
}

pub fn apply_unique_place_answers(judgment: &mut CascadeJudgment, places: &PlaceCandidates) {
    if judgment.municipality.is_none() && places.municipalities.len() == 1 {
        judgment.municipality = Some(unique_place_answer(&places.municipalities[0]));
    }
    if judgment.province.is_none() && places.admin_areas.len() == 1 {
        judgment.province = Some(unique_place_answer(&places.admin_areas[0]));
    }
    if judgment.region.is_none() && places.regions.len() == 1 {
        judgment.region = Some(unique_place_answer(&places.regions[0]));
    }
}

/// Free geo when the gazetteer leaves exactly one candidate per level.
pub fn unique_geo_values(
    places: &PlaceCandidates,
) -> (Option<String>, Option<String>, Option<String>) {
    (
        (places.municipalities.len() == 1).then(|| places.municipalities[0].clone()),
        (places.admin_areas.len() == 1).then(|| places.admin_areas[0].clone()),
        (places.regions.len() == 1).then(|| places.regions[0].clone()),
    )
}

/// Minimal evidence blob for unique gazetteer fill outside a full source_context.
pub fn place_evidence_context(
    email: &str,
    denomination: Option<&str>,
    source_url: Option<&str>,
) -> String {
    let mut fields = Map::new();
    let email = email.trim();
    if !email.is_empty() {
        fields.insert("Email".to_owned(), Value::String(email.to_owned()));
    }
    if let Some(denomination) = denomination
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        fields.insert(
            "Denominazione".to_owned(),
            Value::String(denomination.to_owned()),
        );
    }
    if let Some(source_url) = source_url.map(str::trim).filter(|value| !value.is_empty()) {
        fields.insert("Sito Web".to_owned(), Value::String(source_url.to_owned()));
    }
    serde_json::to_string(&vec![json!({ "fields": fields })]).unwrap_or_else(|_| "[]".to_owned())
}

fn unique_place_answer(place: &str) -> ChoiceAnswer {
    ChoiceAnswer {
        choice: place.to_owned(),
        confidence: 1.0,
        probabilities: HashMap::new(),
    }
}

pub fn synthesize_policy_answers(judgment: &mut CascadeJudgment, policy: SpendPolicy) {
    if judgment.disposition.choice.is_empty() {
        judgment.disposition = inferred_disposition(judgment, policy);
    }
}

fn inferred_disposition(judgment: &CascadeJudgment, policy: SpendPolicy) -> ChoiceAnswer {
    let apply_bar = effective_apply_confidence(judgment, false, policy);
    if judgment.category.choice == "none_fit" {
        if judgment.needs_taxonomy_gap.noul >= policy.gap_noul_threshold {
            return ChoiceAnswer {
                choice: "escalate_generative".to_owned(),
                confidence: judgment.needs_taxonomy_gap.noul,
                probabilities: HashMap::new(),
            };
        }
        return ChoiceAnswer {
            choice: "insufficient".to_owned(),
            confidence: (1.0 - judgment.needs_taxonomy_gap.noul).clamp(0.0, 1.0),
            probabilities: HashMap::new(),
        };
    }
    if judgment.category.confidence < apply_bar {
        return ChoiceAnswer {
            choice: "insufficient".to_owned(),
            confidence: (1.0 - judgment.category.confidence).clamp(0.0, 1.0),
            probabilities: HashMap::new(),
        };
    }
    ChoiceAnswer {
        choice: "apply".to_owned(),
        confidence: judgment.category.confidence,
        probabilities: HashMap::new(),
    }
}

/// Gap answers only matter when no closed category fits: a weak but real category
/// stays Insufficient regardless, so asking about a gap there only burns credits.
pub fn category_needs_gap_followup(judgment: &CascadeJudgment, _policy: SpendPolicy) -> bool {
    judgment.category.choice == "none_fit"
}

/// Institutional / org-shaped contacts are worth a paid subcategory pass.
pub fn is_high_value_contact(email: &str, source_context: &str) -> bool {
    let email = email.trim().to_ascii_lowercase();
    let Some((_, domain)) = email.split_once('@') else {
        return false;
    };
    if is_consumer_mail_domain(domain) {
        return false;
    }
    if domain.ends_with(".gov.it")
        || domain.ends_with(".edu.it")
        || domain == "istruzione.it"
        || domain.ends_with(".istruzione.it")
        || domain.contains("comune.")
        || domain.starts_with("pec.")
        || domain.contains(".pec.")
        || domain.ends_with(".gouv.fr")
    {
        return true;
    }
    let fields = compact_jev_fields(source_context);
    if fields.contains_key("PEC") {
        return true;
    }
    let denom = fields
        .get("Denominazione")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let blob = format!(
        "{} {} {}",
        domain,
        denom,
        fields
            .get("Local")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase()
    );
    const MARKERS: &[&str] = &[
        "comune",
        "provincia",
        "regione",
        "scuola",
        "istituto",
        "universit",
        "ospedale",
        "asl",
        "parrocchia",
        "diocesi",
        "proloco",
        "pro loco",
        "biblioteca",
        "museo",
        "associazion",
        "fondazion",
        "ministero",
        "prefettur",
        "carabinieri",
        "polizia",
        "croce rossa",
        "misericordia",
        "conservatorio",
        "cai",
        "rotary",
        "lions",
    ];
    MARKERS.iter().any(|marker| blob.contains(marker))
}

pub(crate) fn is_consumer_mail_domain(domain: &str) -> bool {
    const CONSUMER: &[&str] = &[
        "gmail.com",
        "googlemail.com",
        "yahoo.com",
        "yahoo.it",
        "hotmail.com",
        "hotmail.it",
        "outlook.com",
        "outlook.it",
        "live.com",
        "live.it",
        "libero.it",
        "virgilio.it",
        "alice.it",
        "tin.it",
        "icloud.com",
        "me.com",
        "proton.me",
        "protonmail.com",
        "aol.com",
        "msn.com",
    ];
    CONSUMER.contains(&domain)
}

pub fn category_needs_subcategory_followup(
    judgment: &CascadeJudgment,
    taxonomy: &[(String, Vec<String>)],
    policy: SpendPolicy,
    high_value: bool,
) -> bool {
    if judgment.category.choice == "none_fit"
        || !category_needs_subcategory_pass(&judgment.category.choice, taxonomy)
    {
        return false;
    }
    let cat = judgment.category.confidence;
    // High-value: always buy subcategory once the category is strong enough to write.
    // Cheap-skip only applies to low-value contacts (which never buy a second call anyway).
    if high_value {
        return cat >= policy.subcategory_floor;
    }
    let _ = policy.cheap_skip_sub_above;
    false
}

/// Apply bar: high-value with a strong subcategory may write at the subcategory floor.
pub fn effective_apply_confidence(
    judgment: &CascadeJudgment,
    high_value: bool,
    policy: SpendPolicy,
) -> f64 {
    if high_value
        && judgment.subcategory.choice != "none_fit"
        && judgment.subcategory.confidence
            >= subcategory_confidence_threshold(policy.subcategory_floor)
    {
        policy.subcategory_floor
    } else {
        policy.apply_confidence
    }
}

pub fn build_batched_subcategory_questions(
    slots: &[(usize, String)],
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
) -> Map<String, Value> {
    let mut questions = Map::new();
    for (index, category) in slots {
        let Some((_, subcategories)) = taxonomy.iter().find(|(key, _)| key == category) else {
            continue;
        };
        if subcategories.is_empty() {
            continue;
        }
        let category_label = display_names
            .get(&("category".to_owned(), None, category.clone()))
            .cloned()
            .unwrap_or_else(|| category.clone());
        let mut criteria = Map::new();
        for subcategory in subcategories {
            let label = display_names
                .get(&(
                    "subcategory".to_owned(),
                    Some(category.clone()),
                    subcategory.clone(),
                ))
                .cloned()
                .unwrap_or_else(|| subcategory.clone());
            criteria.insert(subcategory.clone(), Value::String(label));
        }
        criteria.insert(
            "none_fit".to_owned(),
            Value::String(format!(
                "Category {category_label} fits but no listed subcategory describes this entity"
            )),
        );
        questions.insert(
            format!("subcategory__{index}"),
            json!({
                "type": "choice",
                "instructions": format!(
                    "Assuming `contacts[{index}]` belongs to `{category}` ({category_label}), which subcategory best fits? Meanings are in `taxonomy.subcategories.{category}`. Choose none_fit when only the category is clear."
                ),
                "criteria": criteria
            }),
        );
    }
    questions
}

/// Map batched answers for one contact index onto the single-contact answer keys.
pub fn project_contact_answers(batch_answers: &Value, index: usize) -> Value {
    let Some(object) = batch_answers.as_object() else {
        return json!({});
    };
    let suffix = format!("__{index}");
    let mut projected = Map::new();
    for (key, value) in object {
        if let Some(stem) = key.strip_suffix(&suffix) {
            projected.insert(stem.to_owned(), value.clone());
        }
    }
    Value::Object(projected)
}

pub fn parse_projected_cascade_judgment(
    projected_answers: Value,
    raw_response: Value,
) -> Result<CascadeJudgment, ProviderError> {
    let wrapper = json!({ "answers": projected_answers });
    let mut judgment = parse_cascade_judgment(&wrapper)?;
    judgment.raw = raw_response;
    Ok(judgment)
}

pub fn category_needs_subcategory_pass(category: &str, taxonomy: &[(String, Vec<String>)]) -> bool {
    taxonomy
        .iter()
        .any(|(key, subs)| key == category && !subs.is_empty())
}

/// Legacy single-contact question set (speculative subcategories). Kept for size
/// regression tests against the batched pass-1 builder.
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_cascade_questions(
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
) -> Map<String, Value> {
    let mut category_criteria = Map::new();
    for (category, _) in taxonomy {
        let label = display_names
            .get(&("category".to_owned(), None, category.clone()))
            .cloned()
            .unwrap_or_else(|| category.clone());
        category_criteria.insert(category.clone(), Value::String(label));
    }
    category_criteria.insert(
        "none_fit".to_owned(),
        Value::String("Nessuna categoria attiva descrive questo ente".to_owned()),
    );

    let mut questions = Map::new();
    questions.insert(
        "disposition".to_owned(),
        json!({
            "type": "choice",
            "instructions": "Given the contact source records and active taxonomy, how should software act? Prefer apply whenever one active category fits, even if subcategory is unclear. Choose escalate_generative ONLY when the entity needs a new taxonomy label that is not in the active closed set. Prefer insufficient when evidence is too thin. Prefer conflict when two incompatible closed-set labels are both strongly supported.",
            "criteria": {
                "apply": "An active category fits and software can write those closed-set labels",
                "escalate_generative": "Needs a freeform taxonomy gap proposal: a new category or subcategory label that the active closed set cannot express",
                "insufficient": "Evidence is too thin to pick any category",
                "conflict": "Two or more incompatible closed-set classifications are both strongly supported"
            }
        }),
    );
    questions.insert(
        "category".to_owned(),
        json!({
            "type": "choice",
            "instructions": "Which active taxonomy category best fits this contact? Prefer the most specific sector over generic public administration when evidence supports it. Choose none_fit only when no active category covers the entity.",
            "criteria": category_criteria
        }),
    );

    // Speculative hierarchical subcategories: one Choice per category. Extra questions
    // add almost no latency; code keeps only the branch for the chosen category.
    for (category, subcategories) in taxonomy {
        if subcategories.is_empty() {
            continue;
        }
        let category_label = display_names
            .get(&("category".to_owned(), None, category.clone()))
            .cloned()
            .unwrap_or_else(|| category.clone());
        let mut subcategory_criteria = Map::new();
        for subcategory in subcategories {
            let label = display_names
                .get(&(
                    "subcategory".to_owned(),
                    Some(category.clone()),
                    subcategory.clone(),
                ))
                .cloned()
                .unwrap_or_else(|| subcategory.clone());
            subcategory_criteria.insert(subcategory.clone(), Value::String(label));
        }
        subcategory_criteria.insert(
            "none_fit".to_owned(),
            Value::String(format!(
                "Category {category_label} fits but no listed subcategory describes this entity"
            )),
        );
        questions.insert(
            format!("subcategory__{category}"),
            json!({
                "type": "choice",
                "instructions": format!(
                    "Assuming the contact belongs to `{category}` ({category_label}), which subcategory best fits? Choose none_fit when only the category is clear."
                ),
                "criteria": subcategory_criteria
            }),
        );
    }

    questions.insert(
        "needs_taxonomy_gap".to_owned(),
        json!({
            "type": "noul",
            "instructions": "Does the evidence support adding a new taxonomy entry that is missing from the active list?",
            "criteria": {
                "true": "A concrete additive category or subcategory would be useful and is not already covered by an active label such as Volontariato, Altro ente culturale, or Associazione generica",
                "false": "The active taxonomy already covers this contact, a nearby catch-all fits, or evidence is too weak for a gap"
            }
        }),
    );
    questions
}

fn country_question(index: usize) -> Value {
    json!({
        "type": "choice",
        "instructions": format!(
            "Which country is `contacts[{index}].fields` in? Use address, phone prefix, canton/province and domain as evidence. Keep the stored `country` unless the evidence clearly contradicts it."
        ),
        "criteria": {
            "Italia": "Italian comune, province, region, phone +39 or .it domain",
            "Svizzera": "Swiss municipality, canton, phone +41 or .ch domain",
            "unknown": "No country is reliably supported by the evidence"
        }
    })
}

pub fn build_geography_questions(place_candidates: &PlaceCandidates) -> Map<String, Value> {
    let mut questions = Map::new();
    if !place_candidates.municipalities.is_empty() {
        questions.insert(
            "municipality".to_owned(),
            place_choice(
                "Which gazetteer municipality is the primary administrative seat? Choose only a place supported by the evidence. Prefer the seat over secondary branches.",
                &place_candidates.municipalities,
            ),
        );
    }
    if !place_candidates.admin_areas.is_empty() {
        let (question_id, instructions) = match place_candidates.admin_level {
            "canton" => (
                "canton",
                "Which gazetteer canton is indicated by the evidence?",
            ),
            _ => (
                "province",
                "Which gazetteer province is indicated by the evidence?",
            ),
        };
        questions.insert(
            question_id.to_owned(),
            place_choice(instructions, &place_candidates.admin_areas),
        );
    }
    if !place_candidates.regions.is_empty() {
        questions.insert(
            "region".to_owned(),
            place_choice(
                "Which gazetteer region is indicated by the evidence?",
                &place_candidates.regions,
            ),
        );
    }
    questions
}

fn place_choice(instructions: &str, candidates: &[String]) -> Value {
    let mut criteria = Map::new();
    for candidate in candidates {
        criteria.insert(
            candidate.clone(),
            Value::String(format!("Gazetteer place: {candidate}")),
        );
    }
    criteria.insert(
        "unknown".to_owned(),
        Value::String("No gazetteer place is reliably supported by the evidence".to_owned()),
    );
    json!({
        "type": "choice",
        "instructions": instructions,
        "criteria": criteria
    })
}

#[derive(Debug, Clone, Default)]
pub struct PlaceCandidates {
    pub municipalities: Vec<String>,
    pub admin_areas: Vec<String>,
    pub regions: Vec<String>,
    pub admin_level: &'static str,
}

const MAX_PLACE_OPTIONS: usize = 8;
/// Overwriting a stored country is riskier than filling an empty place, so the bar is higher.
const COUNTRY_MIN_CONFIDENCE: f64 = 0.9;

pub fn extract_place_candidates(source_context: &str, country: &str) -> PlaceCandidates {
    let swiss = matches!(
        country.trim().to_ascii_lowercase().as_str(),
        "svizzera" | "switzerland" | "ch"
    );
    let admin_level = if swiss { "canton" } else { "province" };
    let mut municipalities = Vec::new();
    let mut admin_areas = Vec::new();
    let mut regions = Vec::new();

    let Ok(records) = serde_json::from_str::<Vec<Value>>(source_context) else {
        return PlaceCandidates {
            admin_level,
            ..PlaceCandidates::default()
        };
    };

    let mut corpus = String::new();
    // Town names are scanned without Provincia/Regione text: "Provincia: Bari"
    // must not offer the city of Bari for a contact in Monopoli.
    let mut town_corpus = String::new();
    for record in &records {
        let Some(fields) = record.get("fields").and_then(Value::as_object) else {
            continue;
        };
        for (key, value) in fields {
            let Some(text) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            push_place_text(&mut corpus, text);
            let key_l = key.to_lowercase();
            let admin_field = ["provincia", "province", "canton", "regione", "region"]
                .iter()
                .any(|marker| key_l.contains(marker));
            if !admin_field {
                push_place_text(&mut town_corpus, text);
            }
            if key_l.contains("comune")
                || key_l.contains("municip")
                || key_l.contains("citt")
                || key_l.contains("city")
            {
                if let Some(canonical) = crate::ai_enrichment::normalize_extracted_comune(text) {
                    push_unique(&mut municipalities, &canonical);
                }
            } else if key_l.contains("provincia") || key_l.contains("province") {
                if !swiss {
                    let canonical =
                        crate::ai_enrichment::normalize_extracted_province(text.to_owned());
                    if !canonical.is_empty() {
                        push_unique(&mut admin_areas, &canonical);
                    }
                }
            } else if key_l.contains("canton") {
                if let Some(canonical) =
                    lookup_index_phrase(text, &crate::gazetteer::GAZETTEER_INDEX.cantons)
                {
                    push_unique(&mut admin_areas, canonical);
                }
            } else if (key_l.contains("regione") || key_l.contains("region"))
                && let Some(canonical) = crate::ai_enrichment::normalize_extracted_regione(text)
            {
                push_unique(&mut regions, &canonical);
            }
        }
    }

    let mentions = scan_gazetteer_mentions(&corpus);
    // An explicit Comune field is authoritative; free-text mentions only help
    // when the source has none.
    if municipalities.is_empty() {
        for name in scan_gazetteer_mentions(&town_corpus).municipalities {
            push_unique(&mut municipalities, &name);
        }
    }
    if swiss {
        for name in mentions.cantons {
            push_unique(&mut admin_areas, &name);
        }
    } else {
        for name in mentions.provinces {
            push_unique(&mut admin_areas, &name);
        }
        for name in mentions.regions {
            push_unique(&mut regions, &name);
        }
    }

    PlaceCandidates {
        municipalities: rank_and_cap(municipalities, MAX_PLACE_OPTIONS),
        admin_areas: rank_and_cap(admin_areas, MAX_PLACE_OPTIONS),
        regions: if swiss {
            Vec::new()
        } else {
            rank_and_cap(regions, MAX_PLACE_OPTIONS)
        },
        admin_level,
    }
}

/// Mail-provider and technical DNS labels are never places: `.com` is not Como,
/// `virgilio.it` is not Borgo Virgilio, `anci.it` is not Anzi.
const NON_PLACE_DOMAIN_LABELS: &[&str] = &[
    "aol",
    "alice",
    "aruba",
    "arubapec",
    "cert",
    "email",
    "fastwebnet",
    "gmail",
    "googlemail",
    "hotmail",
    "icloud",
    "infinito",
    "inwind",
    "iol",
    "katamail",
    "legalmail",
    "libero",
    "live",
    "mail",
    "me",
    "msn",
    "outlook",
    "pec",
    "pecimprese",
    "posta",
    "postacert",
    "proton",
    "protonmail",
    "teletu",
    "tim",
    "tin",
    "tiscali",
    "virgilio",
    "vodafone",
    "web",
    "www",
    "yahoo",
];

/// Official town names that are everyday words in addresses, paths and local parts
/// (`lettere@`, `poli-e-biblioteche`, `fondi-e-raccolte`, "Istituto dello …").
/// ponytail: hand-picked from the 2026-09 audit; extend when a new false town shows up.
const COMMON_WORD_TOWNS: &[&str] = &[
    "ala", "anzi", "arena", "asso", "bono", "cento", "dello", "fondi", "lettere", "nove", "pero",
    "poli", "postal", "sale", "segni", "trenta", "virgilio", "vita", "zone",
];

fn domain_place_labels(domain: &str) -> impl Iterator<Item = &str> {
    let labels = domain.split('.').collect::<Vec<_>>();
    let keep = labels.len().saturating_sub(1);
    labels
        .into_iter()
        .take(keep)
        .filter(|label| !NON_PLACE_DOMAIN_LABELS.contains(&label.to_ascii_lowercase().as_str()))
}

/// Appends a field value as place evidence. Emails contribute their local part and
/// non-provider domain labels, never the TLD. URLs contribute the same host labels
/// plus their path: listing pages are often per town (`comune.chieri.to.it/…`).
fn push_place_text(corpus: &mut String, text: &str) {
    let trimmed = text.trim();
    if let Some((local, domain)) = trimmed
        .split_once('@')
        .filter(|(_, domain)| !trimmed.contains(char::is_whitespace) && domain.contains('.'))
    {
        corpus.push(' ');
        corpus.push_str(local);
        for label in domain_place_labels(&domain.to_ascii_lowercase()) {
            corpus.push(' ');
            corpus.push_str(label);
        }
        return;
    }
    let lower = trimmed.to_ascii_lowercase();
    let url = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .or_else(|| lower.starts_with("www.").then_some(lower.as_str()));
    if let Some(rest) = url {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = host.split(':').next().unwrap_or_default();
        for label in domain_place_labels(host) {
            corpus.push(' ');
            corpus.push_str(label);
        }
        corpus.push(' ');
        corpus.push_str(path.split(['?', '#']).next().unwrap_or_default());
        return;
    }
    corpus.push(' ');
    corpus.push_str(trimmed);
}

#[derive(Default)]
struct GazetteerMentions {
    municipalities: Vec<String>,
    provinces: Vec<String>,
    regions: Vec<String>,
    cantons: Vec<String>,
}

fn scan_gazetteer_mentions(text: &str) -> GazetteerMentions {
    let normed = crate::gazetteer::norm(text);
    if normed.is_empty() {
        return GazetteerMentions::default();
    }
    let tokens = normed.split_whitespace().collect::<Vec<_>>();
    let mut mentions = GazetteerMentions::default();
    let mut matched_muni = std::collections::HashMap::<&str, usize>::new();
    let mut matched_prov = std::collections::HashMap::<&str, usize>::new();
    let mut matched_reg = std::collections::HashMap::<&str, usize>::new();
    let mut matched_canton = std::collections::HashMap::<&str, usize>::new();

    for start in 0..tokens.len() {
        let max_end = tokens.len().min(start + 6);
        for end in (start + 1..=max_end).rev() {
            let phrase = tokens[start..end].join(" ");
            // Siglas are two letters; GeoNames stubs (`com`, `per`, `del`) are not in
            // the official-name map. Explicit fields resolve through their own path.
            if phrase.chars().count() < 3 || COMMON_WORD_TOWNS.contains(&phrase.as_str()) {
                continue;
            }
            let span = phrase.len();
            if let Some(canonical) = crate::gazetteer::GAZETTEER_INDEX
                .municipality_names
                .get(&phrase)
            {
                matched_muni
                    .entry(*canonical)
                    .and_modify(|best| *best = (*best).max(span))
                    .or_insert(span);
                break;
            }
            if let Some(canonical) = crate::gazetteer::GAZETTEER_INDEX.provinces.get(&phrase) {
                matched_prov
                    .entry(*canonical)
                    .and_modify(|best| *best = (*best).max(span))
                    .or_insert(span);
                break;
            }
            if let Some(canonical) = crate::gazetteer::GAZETTEER_INDEX.regions.get(&phrase) {
                matched_reg
                    .entry(*canonical)
                    .and_modify(|best| *best = (*best).max(span))
                    .or_insert(span);
                break;
            }
            if let Some(canonical) = crate::gazetteer::GAZETTEER_INDEX.cantons.get(&phrase) {
                matched_canton
                    .entry(*canonical)
                    .and_modify(|best| *best = (*best).max(span))
                    .or_insert(span);
                break;
            }
        }
    }

    mentions.municipalities = rank_mentions(matched_muni);
    mentions.provinces = rank_mentions(matched_prov);
    mentions.regions = rank_mentions(matched_reg);
    mentions.cantons = rank_mentions(matched_canton);
    mentions
}

fn lookup_index_phrase<'a>(
    value: &str,
    index: &'a HashMap<String, &'static str>,
) -> Option<&'a str> {
    index.get(&crate::gazetteer::norm(value)).copied()
}

fn rank_mentions(matched: std::collections::HashMap<&str, usize>) -> Vec<String> {
    let mut ranked = matched.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    ranked
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect()
}

fn rank_and_cap(values: Vec<String>, limit: usize) -> Vec<String> {
    let mut ranked = values;
    ranked.sort_by(|left, right| {
        right
            .chars()
            .count()
            .cmp(&left.chars().count())
            .then_with(|| left.cmp(right))
    });
    ranked.truncate(limit);
    ranked
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    if !values
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(value))
    {
        values.push(value.to_owned());
    }
}

pub fn parse_cascade_judgment(response: &Value) -> Result<CascadeJudgment, ProviderError> {
    let answers = response
        .get("answers")
        .ok_or_else(|| ProviderError::terminal("typesafe.answers.missing"))?;
    let category: ChoiceAnswer = deserialize_answer(answers, "category")?;
    let subcategory = match hierarchical_subcategory(answers, &category.choice)? {
        Some(answer) => answer,
        None => optional_answer(answers, "subcategory")?.unwrap_or(ChoiceAnswer {
            choice: "none_fit".to_owned(),
            confidence: 0.0,
            probabilities: HashMap::new(),
        }),
    };
    Ok(CascadeJudgment {
        disposition: optional_answer(answers, "disposition")?.unwrap_or(ChoiceAnswer {
            choice: String::new(),
            confidence: 0.0,
            probabilities: HashMap::new(),
        }),
        category,
        subcategory,
        needs_taxonomy_gap: optional_answer(answers, "needs_taxonomy_gap")?
            .unwrap_or(NoulAnswer { noul: 0.0 }),
        country: optional_answer(answers, "country")?,
        municipality: optional_answer(answers, "municipality")?,
        province: optional_answer(answers, "province")?.or(optional_answer(answers, "canton")?),
        region: optional_answer(answers, "region")?,
        raw: response.clone(),
    })
}

fn hierarchical_subcategory(
    answers: &Value,
    category_key: &str,
) -> Result<Option<ChoiceAnswer>, ProviderError> {
    if category_key == "none_fit" || category_key.is_empty() {
        return Ok(None);
    }
    optional_answer(answers, &format!("subcategory__{category_key}"))
}

fn deserialize_answer<T: for<'de> Deserialize<'de>>(
    answers: &Value,
    key: &str,
) -> Result<T, ProviderError> {
    let value = answers
        .get(key)
        .ok_or_else(|| ProviderError::terminal(format!("typesafe.answer.missing:{key}")))?;
    serde_json::from_value(value.clone())
        .map_err(|error| ProviderError::terminal(format!("typesafe.answer.parse:{key}:{error}")))
}

fn optional_answer<T: for<'de> Deserialize<'de>>(
    answers: &Value,
    key: &str,
) -> Result<Option<T>, ProviderError> {
    match answers.get(key) {
        None => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                ProviderError::terminal(format!("typesafe.answer.parse:{key}:{error}"))
            }),
    }
}

pub fn optional_subcategory_answer(answers: &Value) -> Result<Option<ChoiceAnswer>, ProviderError> {
    optional_answer(answers, "subcategory")
}

pub fn optional_gap_answer(answers: &Value) -> Result<Option<NoulAnswer>, ProviderError> {
    optional_answer(answers, "needs_taxonomy_gap")
}

pub fn subcategory_confidence_threshold(confidence_threshold: f64) -> f64 {
    // Subcategory Choices are narrower; allow a slightly lower gate than category.
    (confidence_threshold - 0.05).clamp(0.0, confidence_threshold)
}

pub fn decide_cascade_action(
    judgment: &CascadeJudgment,
    taxonomy: &[(String, Vec<String>)],
    policy: SpendPolicy,
    high_value: bool,
) -> CascadeAction {
    // Disposition is a 4-way policy Choice. Mild probability spread there is normal and
    // should not veto a usable closed-set category read.
    let apply_bar = effective_apply_confidence(judgment, high_value, policy);
    let subcategory_threshold = subcategory_confidence_threshold(apply_bar);

    if strong_closed_set_apply(judgment, taxonomy, apply_bar, subcategory_threshold)
        && judgment.needs_taxonomy_gap.noul < policy.gap_noul_threshold
    {
        // Prefer writing clear closed-set labels even when disposition leans escalate.
        if matches!(
            judgment.disposition.choice.as_str(),
            "apply" | "escalate_generative"
        ) {
            return CascadeAction::Apply;
        }
    }

    match judgment.disposition.choice.as_str() {
        "insufficient" => CascadeAction::Insufficient,
        "conflict" => CascadeAction::Conflict,
        // Jev owns closed-set classification. Muse runs only for real taxonomy gaps
        // (freeform labels outside the active catalog). Low confidence without a gap
        // stays on Jev as category-only Apply, or Insufficient when nothing fits.
        "escalate_generative" | "apply" => {
            if judgment.needs_taxonomy_gap.noul >= policy.gap_noul_threshold {
                return CascadeAction::Escalate;
            }
            if judgment.category.choice == "none_fit" {
                return CascadeAction::Insufficient;
            }
            if judgment.category.confidence < apply_bar {
                return CascadeAction::Insufficient;
            }
            let category = judgment.category.choice.as_str();
            if !taxonomy.iter().any(|(key, _)| key == category) {
                return CascadeAction::Insufficient;
            }
            CascadeAction::Apply
        }
        _ => CascadeAction::Escalate,
    }
}

/// Jev's top category below the apply bar still confirms the category already on
/// the contact: agreement is corroboration, so the contact stays `classified`.
pub const AGREEMENT_CONFIDENCE: f64 = 0.6;

pub fn confirms_current_category(
    judgment: &CascadeJudgment,
    current_category: Option<&str>,
    display_names: &HashMap<(String, Option<String>, String), String>,
) -> bool {
    let Some(current) = current_category
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let key = judgment.category.choice.as_str();
    if key == "none_fit" || judgment.category.confidence < AGREEMENT_CONFIDENCE {
        return false;
    }
    let label = display_names
        .get(&("category".to_owned(), None, key.to_owned()))
        .map(String::as_str)
        .unwrap_or(key);
    let slug = crate::normalize_key;
    slug(label) == slug(current) || slug(key) == slug(current)
}

/// Drop a subcategory that is too weak to write, so Apply stays category-only.
pub fn clear_weak_subcategory(judgment: &mut CascadeJudgment, confidence_threshold: f64) {
    let threshold = subcategory_confidence_threshold(confidence_threshold);
    if judgment.subcategory.choice != "none_fit" && judgment.subcategory.confidence < threshold {
        judgment.subcategory = ChoiceAnswer {
            choice: "none_fit".to_owned(),
            confidence: 0.0,
            probabilities: HashMap::new(),
        };
    }
}

fn strong_closed_set_apply(
    judgment: &CascadeJudgment,
    taxonomy: &[(String, Vec<String>)],
    category_threshold: f64,
    subcategory_threshold: f64,
) -> bool {
    if judgment.category.choice == "none_fit"
        || judgment.category.confidence < category_threshold
        || judgment.subcategory.choice == "none_fit"
        || judgment.subcategory.confidence < subcategory_threshold
    {
        return false;
    }
    taxonomy.iter().any(|(category, subcategories)| {
        category == &judgment.category.choice
            && subcategories
                .iter()
                .any(|subcategory| subcategory == &judgment.subcategory.choice)
    })
}

/// Drop a subcategory that does not belong under the chosen category so Apply can keep category-only.
pub fn sanitize_subcategory_for_category(
    judgment: &mut CascadeJudgment,
    taxonomy: &[(String, Vec<String>)],
) {
    let category = judgment.category.choice.as_str();
    let Some((_, subcategories)) = taxonomy.iter().find(|(key, _)| key == category) else {
        judgment.subcategory = ChoiceAnswer {
            choice: "none_fit".to_owned(),
            confidence: 0.0,
            probabilities: HashMap::new(),
        };
        return;
    };
    if judgment.subcategory.choice != "none_fit"
        && !subcategories
            .iter()
            .any(|key| key == &judgment.subcategory.choice)
    {
        judgment.subcategory = ChoiceAnswer {
            choice: "none_fit".to_owned(),
            confidence: 0.0,
            probabilities: HashMap::new(),
        };
    }
}

pub fn judgment_to_unified(
    judgment: &CascadeJudgment,
    action: &CascadeAction,
    source_context: &str,
    place_candidates: &PlaceCandidates,
) -> UnifiedResult {
    let evidence = first_evidence_quote(source_context);
    let taxonomy = match action {
        CascadeAction::Apply => {
            let subcategory = (judgment.subcategory.choice != "none_fit")
                .then(|| judgment.subcategory.choice.clone());
            TaxonomyResult {
                outcome: TaxonomyOutcome::Proposal,
                candidates: vec![TaxonomyCandidate {
                    category_key: judgment.category.choice.clone(),
                    subcategory_key: subcategory.clone(),
                    category_grade: EvidenceGrade::Direct,
                    subcategory_grade: subcategory.as_ref().map(|_| EvidenceGrade::Direct),
                    category_evidence: evidence.clone(),
                    subcategory_evidence: if subcategory.is_some() {
                        evidence.clone()
                    } else {
                        Vec::new()
                    },
                }],
                gap: None,
                summary: Some("Classificazione Jev ad alta confidenza".to_owned()),
            }
        }
        CascadeAction::Conflict => TaxonomyResult {
            outcome: TaxonomyOutcome::Conflict,
            candidates: Vec::new(),
            gap: None,
            summary: Some("Conflitto segnalato da Jev".to_owned()),
        },
        CascadeAction::Insufficient => TaxonomyResult {
            outcome: TaxonomyOutcome::Insufficient,
            candidates: Vec::new(),
            gap: None,
            summary: Some("Evidenza insufficiente secondo Jev".to_owned()),
        },
        CascadeAction::Escalate => TaxonomyResult {
            outcome: TaxonomyOutcome::Insufficient,
            candidates: Vec::new(),
            gap: None,
            summary: Some("Escalation generativa richiesta".to_owned()),
        },
    };
    UnifiedResult {
        taxonomy,
        geography: geography_from_judgment(judgment, &evidence, place_candidates),
    }
}

fn geography_from_judgment(
    judgment: &CascadeJudgment,
    evidence: &[GroundedSourceReference],
    place_candidates: &PlaceCandidates,
) -> GeographicResult {
    let mut claims = Vec::new();
    for (answer, level) in [
        (judgment.municipality.as_ref(), "municipality"),
        (judgment.province.as_ref(), place_candidates.admin_level),
        (judgment.region.as_ref(), "region"),
    ] {
        if let Some(answer) = answer
            && answer.choice != "unknown"
            && answer.confidence >= 0.7
        {
            claims.push(GeographicPlaceClaim {
                observed_place: answer.choice.clone(),
                level_hint: Some(level.to_owned()),
                evidence: evidence.to_vec(),
            });
        }
    }
    // A country answer is a proposal on its own, even without place claims.
    let country = judgment
        .country
        .as_ref()
        .filter(|answer| answer.confidence >= COUNTRY_MIN_CONFIDENCE)
        .and_then(|answer| {
            matches!(answer.choice.as_str(), "Italia" | "Svizzera").then(|| answer.choice.clone())
        });
    GeographicResult {
        outcome: if claims.is_empty() && country.is_none() {
            "insufficient".to_owned()
        } else {
            "proposal".to_owned()
        },
        country,
        claims,
        summary: Some("Geografia Jev su gazetteer".to_owned()),
    }
}

fn first_evidence_quote(source_context: &str) -> Vec<GroundedSourceReference> {
    let Ok(records) = serde_json::from_str::<Vec<Value>>(source_context) else {
        return Vec::new();
    };
    for record in records {
        let source_id = record
            .get("source_id")
            .and_then(Value::as_str)
            .unwrap_or("canonical_record")
            .to_owned();
        if let Some(fields) = record.get("fields").and_then(Value::as_object) {
            for (field, value) in fields {
                if let Some(quote) = value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    return vec![GroundedSourceReference {
                        source_id,
                        field: field.clone(),
                        quote: quote.chars().take(180).collect(),
                    }];
                }
            }
        }
    }
    Vec::new()
}

#[tauri::command]
pub async fn detect_typesafe(app: tauri::AppHandle) -> Result<TypeSafeStatus, String> {
    let db = crate::open_db(&app)?;
    let configured = resolve_api_key(crate::ai_registry::load_setting(
        &db,
        TYPESAFE_SETTINGS_KEY,
    )?)
    .is_some();
    Ok(TypeSafeStatus {
        available: configured,
        configured,
        model: crate::ai_selection::TYPESAFE_DEFAULT_MODEL.to_owned(),
        error: if configured {
            None
        } else {
            Some("Imposta la chiave API TypeSafe oppure la variabile TYPESAFE_API_KEY".to_owned())
        },
    })
}

#[derive(Debug, serde::Serialize)]
pub struct TypeSafeKeyView {
    pub configured: bool,
    pub from_env: bool,
}

#[tauri::command]
pub async fn get_typesafe_api_key(app: tauri::AppHandle) -> Result<TypeSafeKeyView, String> {
    crate::database::blocking(move || typesafe_key_view(&app)).await
}

fn typesafe_key_view(app: &tauri::AppHandle) -> Result<TypeSafeKeyView, String> {
    let from_env = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .is_some();
    if from_env {
        return Ok(TypeSafeKeyView {
            configured: true,
            from_env: true,
        });
    }
    let db = crate::open_db(app)?;
    let stored = crate::ai_registry::load_setting(&db, TYPESAFE_SETTINGS_KEY)?;
    Ok(TypeSafeKeyView {
        configured: stored
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()),
        from_env: false,
    })
}

#[tauri::command]
pub async fn set_typesafe_api_key(
    app: tauri::AppHandle,
    api_key: String,
) -> Result<TypeSafeKeyView, String> {
    crate::database::blocking(move || {
        let trimmed = api_key.trim().to_owned();
        let db = crate::open_db(&app)?;
        if trimmed.is_empty() {
            db.execute(
                "DELETE FROM app_settings WHERE key=?1",
                rusqlite::params![TYPESAFE_SETTINGS_KEY],
            )
            .map_err(|error| error.to_string())?;
        } else {
            crate::ai_registry::save_setting(&db, TYPESAFE_SETTINGS_KEY, &trimmed)?;
        }
        drop(db);
        typesafe_key_view(&app)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn judgment(
        disposition: &str,
        disposition_conf: f64,
        category: &str,
        category_conf: f64,
        subcategory: &str,
        subcategory_conf: f64,
        gap: f64,
    ) -> CascadeJudgment {
        CascadeJudgment {
            disposition: ChoiceAnswer {
                choice: disposition.to_owned(),
                confidence: disposition_conf,
                probabilities: HashMap::new(),
            },
            category: ChoiceAnswer {
                choice: category.to_owned(),
                confidence: category_conf,
                probabilities: HashMap::new(),
            },
            subcategory: ChoiceAnswer {
                choice: subcategory.to_owned(),
                confidence: subcategory_conf,
                probabilities: HashMap::new(),
            },
            needs_taxonomy_gap: NoulAnswer { noul: gap },
            country: None,
            municipality: None,
            province: None,
            region: None,
            raw: json!({}),
        }
    }

    fn test_policy(apply: f64) -> SpendPolicy {
        SpendPolicy {
            apply_confidence: apply,
            subcategory_floor: (apply - 0.15).clamp(0.0, 1.0),
            cheap_skip_sub_above: (apply + 0.03).clamp(0.0, 1.0),
            gap_noul_threshold: 0.55,
        }
    }

    #[test]
    fn weak_agreement_confirms_and_gap_follow_up_only_for_none_fit() {
        let mut names = HashMap::new();
        names.insert(
            (
                "category".to_owned(),
                None,
                "pubblicaamministrazione".to_owned(),
            ),
            "Pubblica amministrazione".to_owned(),
        );
        let agreeing = judgment(
            "insufficient",
            0.3,
            "pubblicaamministrazione",
            0.64,
            "none_fit",
            0.0,
            0.1,
        );
        assert!(confirms_current_category(
            &agreeing,
            Some("Pubblica amministrazione"),
            &names
        ));
        assert!(!confirms_current_category(
            &agreeing,
            Some("Cultura"),
            &names
        ));
        assert!(!confirms_current_category(&agreeing, None, &names));
        let too_weak = judgment(
            "insufficient",
            0.5,
            "pubblicaamministrazione",
            0.55,
            "none_fit",
            0.0,
            0.1,
        );
        assert!(!confirms_current_category(
            &too_weak,
            Some("Pubblica amministrazione"),
            &names
        ));
        let none_fit = judgment("insufficient", 0.5, "none_fit", 0.7, "none_fit", 0.0, 0.1);
        assert!(!confirms_current_category(&none_fit, Some("Varie"), &names));

        let policy = test_policy(0.8);
        assert!(!category_needs_gap_followup(&agreeing, policy));
        assert!(category_needs_gap_followup(&none_fit, policy));
    }

    #[test]
    fn apply_when_confident_closed_set() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment("apply", 0.9, "istruzione", 0.92, "primaria", 0.91, 0.1),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Apply);
    }

    #[test]
    fn escalate_on_taxonomy_gap_signal() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment("apply", 0.95, "istruzione", 0.95, "none_fit", 0.0, 0.7),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Escalate);
    }

    #[test]
    fn category_only_applies_when_gap_below_threshold() {
        let taxonomy = vec![("associazioni".to_owned(), vec!["volontariato".to_owned()])];
        let action = decide_cascade_action(
            &judgment("apply", 0.9, "associazioni", 0.9, "none_fit", 0.0, 0.45),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Apply);
    }

    #[test]
    fn escalate_generative_with_strong_category_applies_category_only() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment(
                "escalate_generative",
                0.8,
                "istruzione",
                0.9,
                "none_fit",
                0.0,
                0.2,
            ),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Apply);
    }

    #[test]
    fn escalate_generative_with_gap_still_escalates() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment(
                "escalate_generative",
                0.8,
                "istruzione",
                0.9,
                "none_fit",
                0.0,
                0.7,
            ),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Escalate);
    }

    #[test]
    fn escalate_generative_with_weak_category_stays_on_jev_as_insufficient() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment(
                "escalate_generative",
                0.8,
                "istruzione",
                0.4,
                "none_fit",
                0.0,
                0.2,
            ),
            &taxonomy,
            test_policy(0.7),
            false,
        );
        assert_eq!(action, CascadeAction::Insufficient);
    }

    #[test]
    fn modest_confidence_applies_without_muse() {
        let taxonomy = vec![(
            "pubblicaamministrazione".to_owned(),
            vec!["law_enforcement".to_owned()],
        )];
        let action = decide_cascade_action(
            &judgment(
                "apply",
                0.57,
                "pubblicaamministrazione",
                0.73,
                "law_enforcement",
                0.67,
                0.35,
            ),
            &taxonomy,
            test_policy(0.7),
            false,
        );
        assert_eq!(action, CascadeAction::Apply);
    }

    #[test]
    fn none_fit_without_gap_is_insufficient_not_muse() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment("apply", 0.9, "none_fit", 0.8, "none_fit", 0.0, 0.2),
            &taxonomy,
            test_policy(0.7),
            false,
        );
        assert_eq!(action, CascadeAction::Insufficient);
    }

    #[test]
    fn weak_subcategory_applies_category_only() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let mut judged = judgment("apply", 0.9, "istruzione", 0.92, "primaria", 0.7, 0.1);
        assert_eq!(
            decide_cascade_action(&judged, &taxonomy, test_policy(0.85), false),
            CascadeAction::Apply
        );
        clear_weak_subcategory(&mut judged, 0.85);
        assert_eq!(judged.subcategory.choice, "none_fit");
    }

    #[test]
    fn strong_closed_set_apply_ignores_mild_gap() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment("apply", 0.9, "istruzione", 0.92, "primaria", 0.91, 0.5),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Apply);
    }

    #[test]
    fn mild_disposition_confidence_does_not_block_strong_category() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment("apply", 0.65, "istruzione", 0.92, "primaria", 0.91, 0.1),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Apply);
    }

    #[test]
    fn wrong_parent_subcategory_keeps_category_only_apply() {
        let taxonomy = vec![
            ("istruzione".to_owned(), vec!["primaria".to_owned()]),
            ("sanita".to_owned(), vec!["ospedale".to_owned()]),
        ];
        let mut judged = judgment("apply", 0.9, "istruzione", 0.92, "ospedale", 0.91, 0.2);
        assert_eq!(
            decide_cascade_action(&judged, &taxonomy, test_policy(0.85), false),
            CascadeAction::Apply
        );
        sanitize_subcategory_for_category(&mut judged, &taxonomy);
        assert_eq!(judged.subcategory.choice, "none_fit");
    }

    #[test]
    fn spend_tiers_buy_sub_only_for_high_value_medium_confidence() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let policy = SpendPolicy::default();
        let medium = judgment("apply", 0.9, "istruzione", 0.75, "none_fit", 0.0, 0.1);
        assert!(category_needs_subcategory_followup(
            &medium, &taxonomy, policy, true
        ));
        assert!(!category_needs_subcategory_followup(
            &medium, &taxonomy, policy, false
        ));
        let cheap = judgment("apply", 0.9, "istruzione", 0.92, "none_fit", 0.0, 0.1);
        assert!(category_needs_subcategory_followup(
            &cheap, &taxonomy, policy, true
        ));
        assert!(!category_needs_subcategory_followup(
            &cheap, &taxonomy, policy, false
        ));
        assert!(is_high_value_contact(
            "segreteria@istituto-esempio.edu.it",
            r#"[{"fields":{"Email":"segreteria@istituto-esempio.edu.it","Denominazione":"Istituto Comprensivo Esempio"}}]"#
        ));
        assert!(!is_high_value_contact(
            "mario.rossi@gmail.com",
            r#"[{"fields":{"Email":"mario.rossi@gmail.com"}}]"#
        ));
    }

    #[test]
    fn high_value_with_strong_sub_applies_at_subcategory_floor() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let policy = SpendPolicy::default();
        let action = decide_cascade_action(
            &judgment("apply", 0.9, "istruzione", 0.74, "primaria", 0.8, 0.1),
            &taxonomy,
            policy,
            true,
        );
        assert_eq!(action, CascadeAction::Apply);
        let low_value = decide_cascade_action(
            &judgment("apply", 0.9, "istruzione", 0.74, "primaria", 0.8, 0.1),
            &taxonomy,
            policy,
            false,
        );
        assert_eq!(low_value, CascadeAction::Insufficient);
    }

    #[test]
    fn strong_closed_set_overrides_escalate_disposition() {
        let taxonomy = vec![("istruzione".to_owned(), vec!["primaria".to_owned()])];
        let action = decide_cascade_action(
            &judgment(
                "escalate_generative",
                0.7,
                "istruzione",
                0.92,
                "primaria",
                0.9,
                0.2,
            ),
            &taxonomy,
            test_policy(0.85),
            false,
        );
        assert_eq!(action, CascadeAction::Apply);
    }

    #[test]
    fn extracts_gazetteer_places_from_free_text() {
        let source = serde_json::json!([{
            "source_id": "reviewed_benchmark",
            "fields": {
                "Evidence": "Scuola Secondaria Statale Dante Alighieri; Piazza Navona, Roma, RM, Lazio"
            }
        }])
        .to_string();
        let places = extract_place_candidates(&source, "Italia");
        assert!(
            places
                .municipalities
                .iter()
                .any(|name| name.eq_ignore_ascii_case("Roma")),
            "expected Roma in {:?}",
            places.municipalities
        );
        assert!(
            places
                .regions
                .iter()
                .any(|name| name.eq_ignore_ascii_case("Lazio")),
            "expected Lazio in {:?}",
            places.regions
        );
        assert_eq!(places.admin_level, "province");
    }

    #[test]
    fn extracts_swiss_canton_candidates() {
        let source = serde_json::json!([{
            "source_id": "reviewed_benchmark",
            "fields": {
                "Evidence": "Municipio Città di Lugano; Piazza Riforma, Cantone Ticino, Svizzera"
            }
        }])
        .to_string();
        let places = extract_place_candidates(&source, "Svizzera");
        assert!(
            places
                .municipalities
                .iter()
                .any(|name| name.to_lowercase().contains("lugano")),
            "expected Lugano in {:?}",
            places.municipalities
        );
        assert!(
            places
                .admin_areas
                .iter()
                .any(|name| name.to_lowercase().contains("ticino")),
            "expected Ticino in {:?}",
            places.admin_areas
        );
        assert_eq!(places.admin_level, "canton");
        assert!(places.regions.is_empty());
    }

    #[test]
    fn compact_jev_fields_keeps_essential_and_drops_duplicates() {
        let source = serde_json::json!([
            {
                "source_id": "row-a",
                "source_path": "/huge/path/ignored.csv",
                "fields": {
                    "Email": "utente@example.test",
                    "junk_column": "xxxxxxxxxxx",
                    "Comune": "Roma"
                }
            },
            {
                "source_id": "canonical_record",
                "fields": {
                    "Email": "utente@example.test",
                    "Denominazione": "Comune di Roma",
                    "Provincia": "RM"
                }
            },
            {
                "source_id": "row-b",
                "fields": {
                    "email": "altro@example.test",
                    "Denominazione": "Should not override canonical"
                }
            }
        ])
        .to_string();
        let fields = compact_jev_fields(&source);
        assert_eq!(
            fields.get("Email").and_then(Value::as_str),
            Some("utente@example.test")
        );
        assert_eq!(
            fields.get("Denominazione").and_then(Value::as_str),
            Some("Comune di Roma")
        );
        assert_eq!(fields.get("Comune").and_then(Value::as_str), Some("Roma"));
        assert_eq!(fields.get("Provincia").and_then(Value::as_str), Some("RM"));
        assert_eq!(
            fields.get("Domain").and_then(Value::as_str),
            Some("example.test")
        );
        assert_eq!(fields.get("Local").and_then(Value::as_str), Some("utente"));
        assert!(fields.get("junk_column").is_none());
        assert!(fields.get("source_path").is_none());
    }

    #[test]
    fn project_contact_answers_strips_batch_suffix() {
        let answers = json!({
            "category__0": { "type": "choice", "choice": "istruzione", "confidence": 0.91 },
            "category__1": { "type": "choice", "choice": "none_fit", "confidence": 0.7 },
        });
        let projected = project_contact_answers(&answers, 0);
        let mut judgment =
            parse_projected_cascade_judgment(projected, json!({"batch": true})).unwrap();
        synthesize_policy_answers(&mut judgment, test_policy(0.7));
        assert_eq!(judgment.disposition.choice, "apply");
        assert_eq!(judgment.category.choice, "istruzione");
        assert_eq!(judgment.subcategory.choice, "none_fit");
        assert!((judgment.needs_taxonomy_gap.noul - 0.0).abs() < f64::EPSILON);
        assert_eq!(judgment.raw.get("batch"), Some(&json!(true)));
    }

    #[test]
    fn unique_gazetteer_places_skip_jev_questions() {
        let places = PlaceCandidates {
            municipalities: vec!["Roma".to_owned()],
            admin_areas: vec!["Roma".to_owned(), "Milano".to_owned()],
            regions: vec!["Lazio".to_owned()],
            admin_level: "province",
        };
        let ambiguous = ambiguous_places_for_jev(&places);
        assert!(ambiguous.municipalities.is_empty());
        assert_eq!(ambiguous.admin_areas.len(), 2);
        assert!(ambiguous.regions.is_empty());
        let (comune, provincia, regione) = unique_geo_values(&places);
        assert_eq!(comune.as_deref(), Some("Roma"));
        assert!(provincia.is_none());
        assert_eq!(regione.as_deref(), Some("Lazio"));
        let mut judgment = judgment("apply", 0.9, "istruzione", 0.9, "none_fit", 0.0, 0.0);
        apply_unique_place_answers(&mut judgment, &places);
        assert_eq!(
            judgment.municipality.as_ref().map(|a| a.choice.as_str()),
            Some("Roma")
        );
        assert!(judgment.province.is_none());
        assert_eq!(
            judgment.region.as_ref().map(|a| a.choice.as_str()),
            Some("Lazio")
        );
    }

    #[test]
    fn batched_pass1_payload_is_much_smaller_than_legacy_per_contact() {
        let taxonomy = vec![
            (
                "istruzione".to_owned(),
                vec!["primaria".to_owned(), "infanzia".to_owned()],
            ),
            (
                "pubblicaamministrazione".to_owned(),
                vec!["comune".to_owned(), "law_enforcement".to_owned()],
            ),
            (
                "associazioni".to_owned(),
                vec!["alpini".to_owned(), "cai".to_owned()],
            ),
        ];
        let mut display_names = HashMap::new();
        for (category, subs) in &taxonomy {
            display_names.insert(
                ("category".to_owned(), None, category.clone()),
                category.clone(),
            );
            for sub in subs {
                display_names.insert(
                    (
                        "subcategory".to_owned(),
                        Some(category.clone()),
                        sub.clone(),
                    ),
                    sub.clone(),
                );
            }
        }
        let legacy = build_cascade_questions(&taxonomy, &display_names);
        let places = vec![PlaceCandidates::default(); JEV_CONTACT_BATCH];
        let batched =
            build_batched_pass1_questions(JEV_CONTACT_BATCH, &taxonomy, &display_names, &places);
        let taxonomy_blob = taxonomy_state_blob_filtered(&taxonomy, &display_names, None, true);
        let compact_contact = compact_jev_fields(
            &json!([{
                "source_id": "canonical_record",
                "fields": {
                    "Email": "utente@example.test",
                    "Denominazione": "Scuola Primaria Example",
                    "Comune": "Milano"
                }
            }])
            .to_string(),
        );
        let legacy_per_contact = json!({
            "questions": legacy,
            "state": {
                "source_records": [{
                    "source_id": "canonical_record",
                    "source_path": "Database/canonical_contact",
                    "fields": compact_contact.clone()
                }]
            }
        })
        .to_string()
        .len()
            * JEV_CONTACT_BATCH;
        let batched_request = json!({
            "questions": batched,
            "state": {
                "taxonomy": taxonomy_blob,
                "contacts": (0..JEV_CONTACT_BATCH)
                    .map(|_| json!({ "fields": compact_contact }))
                    .collect::<Vec<_>>()
            }
        })
        .to_string()
        .len();
        assert!(
            batched_request * 2 < legacy_per_contact,
            "batched {batched_request} should be under half of legacy {legacy_per_contact}"
        );
    }

    #[test]
    fn credit_exhaustion_and_rate_limits_pause_instead_of_escalating() {
        use crate::ai_provider::ProviderErrorKind;
        for status in [402, 403] {
            let error = status_error(status, "").unwrap();
            assert_eq!(error.kind, ProviderErrorKind::Quota);
            assert!(!error.retryable, "HTTP {status} needs a manual Riprendi");
        }
        for status in [429, 503, 529] {
            let error = status_error(status, "").unwrap();
            assert_eq!(error.kind, ProviderErrorKind::Quota);
            assert!(error.retryable, "HTTP {status} should auto-retry");
        }
        let overloaded = status_error(500, "high server load").unwrap();
        assert!(overloaded.is_auto_retryable_limit());
        for status in [400, 401, 500] {
            assert_eq!(
                status_error(status, "").unwrap().kind,
                ProviderErrorKind::Structural
            );
        }
        assert!(status_error(200, "").is_none());
    }

    #[test]
    fn free_text_places_ignore_providers_tlds_aliases_and_foreign_pages() {
        let towns = |fields: serde_json::Value| {
            extract_place_candidates(
                &serde_json::json!([{ "source_id": "s1", "fields": fields }]).to_string(),
                "Italia",
            )
            .municipalities
        };
        // `.com` is not Como (GeoNames alias `Com`), `virgilio.it` is not Borgo Virgilio.
        assert!(towns(serde_json::json!({ "Email": "utente@gmail.com" })).is_empty());
        assert!(towns(serde_json::json!({ "Email": "utente@example.com" })).is_empty());
        assert!(towns(serde_json::json!({ "Email": "utente@virgilio.it" })).is_empty());
        assert!(towns(serde_json::json!({ "Email": "campi@anci.it" })).is_empty());
        // Everyday words that are also towns stay out of free text.
        assert_eq!(
            towns(serde_json::json!({ "Email": "lettere@tribunatreviso.it" })),
            Vec::<String>::new()
        );
        // Path words that are also towns stay out; the TLD of a URL host too.
        assert!(
            towns(serde_json::json!({
                "Email": "biblioteca@example.test",
                "Sito Web": "https://www.iccu.sbn.it/it/SBN/poli-e-biblioteche/polo/"
            }))
            .is_empty()
        );
        assert!(
            towns(serde_json::json!({
                "Email": "utente@example.test",
                "Sito Web": "https://www.example.com/"
            }))
            .is_empty()
        );
        // Real evidence still resolves: local part, own domain labels, own site.
        assert_eq!(
            towns(serde_json::json!({ "Email": "anagrafe.aprigliano@libero.it" })),
            vec!["Aprigliano".to_owned()]
        );
        assert_eq!(
            towns(serde_json::json!({ "Email": "utente@comune.laterina.ar.it" })),
            vec!["Laterina Pergine Valdarno".to_owned()]
        );
        // A municipal listing page names the town in its host.
        assert_eq!(
            towns(serde_json::json!({
                "Email": "utente@libero.it",
                "Sito Web": "https://www.comune.chieri.to.it/chieri-aperta/associazioni"
            })),
            vec!["Chieri".to_owned()]
        );
    }

    #[test]
    fn province_fields_do_not_offer_their_capoluogo_as_the_town() {
        let places = |fields: serde_json::Value| {
            extract_place_candidates(
                &serde_json::json!([{ "source_id": "s1", "fields": fields }]).to_string(),
                "Italia",
            )
            .municipalities
        };
        assert_eq!(
            places(serde_json::json!({
                "Comune": "Monopoli",
                "Provincia": "Bari",
                "CAP": "70043",
                "Email": "scuola@example.test"
            })),
            vec!["Monopoli".to_owned()]
        );
        assert_eq!(
            places(serde_json::json!({
                "Comune": "Trani",
                "Provincia": "Barletta Andria Trani"
            })),
            vec!["Trani".to_owned()]
        );
        let without_comune = places(serde_json::json!({
            "Provincia": "Bari",
            "Email": "urp@comune.lucca.it"
        }));
        assert!(without_comune.contains(&"Lucca".to_owned()));
        assert!(!without_comune.contains(&"Bari".to_owned()));
    }
}
