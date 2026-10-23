//! Live cascade benchmark that mirrors production enrichment:
//! seed DB contacts + contact_sources, build `classification_source_context`,
//! run `call_cascade_batch_unified`, apply taxonomy/geography with the real
//! commit path, then score fill vs replace (and geo fill) against gold labels.

use crate::ai_batch::BatchContactItem;
use crate::ai_cascade::call_cascade_batch_unified;
use crate::ai_opencode::OpenCodeProvider;
use crate::ai_provider::InferenceProvider;
use crate::ai_runs::{
    apply_geography_result, apply_taxonomy_result, classification_source_context,
};
use crate::ai_selection::{
    PROVIDER_CASCADE, PROVIDER_OPENCODE, ProviderSelection, TYPESAFE_DEFAULT_MODEL,
};
use crate::ai_typesafe::{TypeSafeClient, resolve_api_key};
use crate::benchmark::{ReviewedCase, reviewed_corpus};
use crate::contacts::insert_contact_record;
use crate::normalize_key;
use crate::schema::{ACTIVE_TAXONOMY_VERSION, migrate_database};
use crate::{ContactRecord, EmailStatus, TAXONOMY};
use rusqlite::{Connection, params};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_CASE_LIMIT: usize = 100;
const DEFAULT_OPENCODE_ESCALATION_MODEL: &str = "opencode-go/muse-spark-1.3-contributor";
const DEFAULT_OPENCODE_ESCALATION_EFFORT: &str = "xhigh";

fn bench_selection() -> ProviderSelection {
    // CASCADE_BENCH_ESCALATION_MODEL=opencode-go/muse-spark-1.3-contributor
    // CASCADE_BENCH_ESCALATION_EFFORT=low|medium|high|xhigh
    let escalation_model = std::env::var("CASCADE_BENCH_ESCALATION_MODEL")
        .unwrap_or_else(|_| DEFAULT_OPENCODE_ESCALATION_MODEL.to_owned());
    let escalation_effort = std::env::var("CASCADE_BENCH_ESCALATION_EFFORT")
        .unwrap_or_else(|_| DEFAULT_OPENCODE_ESCALATION_EFFORT.to_owned());
    ProviderSelection {
        provider: PROVIDER_CASCADE.to_owned(),
        model: TYPESAFE_DEFAULT_MODEL.to_owned(),
        escalation_provider: Some(PROVIDER_OPENCODE.to_owned()),
        escalation_model: Some(escalation_model),
        escalation_effort: Some(escalation_effort),
        jev_model: TYPESAFE_DEFAULT_MODEL.to_owned(),
        ..ProviderSelection::default()
    }
    .normalize()
    .expect("cascade selection")
}

fn bench_generative(selection: &ProviderSelection) -> Arc<dyn InferenceProvider> {
    let model = selection.generative_model();
    let effort = selection.generative_effort();
    match selection.generative_provider_id() {
        PROVIDER_OPENCODE => Arc::new(
            OpenCodeProvider::new(model, effort, Duration::from_secs(600))
                .expect("opencode escalation provider"),
        ),
        other => panic!("unsupported cascade escalation provider: {other}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeedMode {
    /// Empty/placeholder taxonomy + empty geo → measure fills.
    Fill,
    /// Wrong taxonomy + empty geo → measure replaces + geo fill.
    Replace,
}

#[derive(Debug, Default)]
struct FieldScore {
    attempted: usize,
    correct: usize,
}

impl FieldScore {
    fn record(&mut self, ok: bool) {
        self.attempted += 1;
        if ok {
            self.correct += 1;
        }
    }

    fn rate(&self) -> f64 {
        if self.attempted == 0 {
            0.0
        } else {
            self.correct as f64 / self.attempted as f64
        }
    }
}

type TaxonomyFixture = (
    Vec<(String, Vec<String>)>,
    HashMap<(String, Option<String>, String), String>,
    String,
);

fn taxonomy_fixture() -> TaxonomyFixture {
    let mut categories = Vec::new();
    let mut display_names = HashMap::new();
    let mut labels = Vec::new();
    for (category, subcategories) in TAXONOMY {
        let category_key = normalize_key(category);
        display_names.insert(
            ("category".to_owned(), None, category_key.clone()),
            (*category).to_owned(),
        );
        let mut subcategory_keys = Vec::new();
        let mut subcategory_labels = Vec::new();
        for subcategory in *subcategories {
            let subcategory_key = if *subcategory == "Forze dell'ordine" {
                "law_enforcement".to_owned()
            } else {
                normalize_key(subcategory)
            };
            display_names.insert(
                (
                    "subcategory".to_owned(),
                    Some(category_key.clone()),
                    subcategory_key.clone(),
                ),
                (*subcategory).to_owned(),
            );
            subcategory_keys.push(subcategory_key);
            subcategory_labels.push(*subcategory);
        }
        labels.push(format!("{category}: {}", subcategory_labels.join(", ")));
        categories.push((category_key, subcategory_keys));
    }
    (categories, display_names, labels.join("\n"))
}

fn open_bench_db() -> Connection {
    let mut db = Connection::open_in_memory().expect("memory db");
    migrate_database(&mut db, None, false).expect("migrate");
    db
}

fn synthetic_email(case_id: &str, index: usize) -> String {
    let slug = case_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    format!("utente{index}-{slug}@example.test")
}

fn seed_mode(index: usize, case: &ReviewedCase) -> SeedMode {
    if case.category.starts_with("__") {
        return SeedMode::Fill;
    }
    if index.is_multiple_of(2) {
        SeedMode::Fill
    } else {
        SeedMode::Replace
    }
}

fn wrong_taxonomy(gold_category: &str) -> (&'static str, Option<&'static str>) {
    if gold_category == "Associazioni" {
        ("Istruzione", Some("Primaria"))
    } else {
        ("Associazioni", Some("Rotary"))
    }
}

fn is_label_case(case: &ReviewedCase) -> bool {
    !case.category.starts_with("__")
}

fn seed_contact(
    db: &Connection,
    case: &ReviewedCase,
    index: usize,
    mode: SeedMode,
) -> ContactRecord {
    let email = synthetic_email(case.id, index);
    let (category, subcategory) = match mode {
        SeedMode::Fill => ("Varie".to_owned(), None),
        SeedMode::Replace => {
            let (cat, sub) = wrong_taxonomy(case.category);
            (cat.to_owned(), sub.map(str::to_owned))
        }
    };
    let local = email.split('@').next().unwrap_or("utente").to_owned();
    let domain = email.split('@').nth(1).unwrap_or("example.test").to_owned();
    let contact = ContactRecord {
        id: case.id.to_owned(),
        email: email.clone(),
        email_local_part: local,
        email_domain: domain.trim_end_matches(".test").to_owned(),
        email_tld: ".test".to_owned(),
        status: EmailStatus::Valid,
        validation_notes: Vec::new(),
        source_file: "reviewed_benchmark.csv".to_owned(),
        category,
        subcategory,
        contact_kind: case.contact_kind.to_owned(),
        paese: case.country.to_owned(),
        is_pec: false,
        comune: None,
        provincia: None,
        regione: None,
        macroarea: None,
        website: None,
        source_url: None,
        category_id: None,
        subcategory_id: None,
        classification_method: "legacy".to_owned(),
        classification_confidence: 0.0,
        classification_evidence: Vec::new(),
        needs_review: false,
        classification_hash: None,
        taxonomy_version_id: None,
        taxonomy_version: None,
        classification_state: "unprocessed".to_owned(),
        classification_state_reason: None,
    };
    insert_contact_record(db, &contact).expect("insert contact");
    db.execute(
        "UPDATE contacts SET denomination=?1 WHERE id=?2",
        params![case.evidence, case.id],
    )
    .expect("denomination");
    let source_id = format!("src-{}", case.id);
    let raw = json!({
        "Evidence": case.evidence,
        "Denominazione": case.evidence,
        "Email": email,
        "ContactKind": case.contact_kind,
        "Paese": case.country,
    })
    .to_string();
    db.execute(
        "INSERT INTO contact_sources(id, contact_id, source_file, source_row, raw_context)
         VALUES (?1, ?2, 'reviewed_benchmark.csv', ?3, ?4)",
        params![source_id, case.id, index as i64, raw],
    )
    .expect("contact_sources");
    contact
}

fn read_contact_snapshot(
    db: &Connection,
    contact_id: &str,
) -> (String, Option<String>, Option<String>, String) {
    db.query_row(
        "SELECT category, subcategory, comune, classification_state FROM contacts WHERE id=?1",
        params![contact_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )
    .expect("read contact")
}

fn change_kinds(db: &Connection, contact_id: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut stmt = db
        .prepare(
            "SELECT field_name, change_kind FROM ai_field_changes
             WHERE contact_id=?1 ORDER BY created_at",
        )
        .expect("prepare changes");
    let rows = stmt
        .query_map(params![contact_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query changes");
    for row in rows {
        let (field, kind) = row.expect("change row");
        map.insert(field, kind);
    }
    map
}

fn same_label(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

#[tokio::test]
#[ignore = "live cascade enrichment benchmark; needs TYPESAFE_API_KEY and Pi or OpenCode. Run: bun run test:cascade"]
async fn cascade_jev_luna_live_benchmark() {
    let api_key = resolve_api_key(None).expect("TYPESAFE_API_KEY must be set");
    let selection = bench_selection();
    let generative = bench_generative(&selection);

    let corpus = reviewed_corpus();
    let limit = match std::env::var("CASCADE_BENCH_LIMIT") {
        Ok(value) if value.eq_ignore_ascii_case("all") || value == "0" => corpus.len(),
        Ok(value) => value
            .parse::<usize>()
            .unwrap_or(DEFAULT_CASE_LIMIT)
            .clamp(1, corpus.len()),
        Err(_) => DEFAULT_CASE_LIMIT.min(corpus.len()),
    };
    let cases = &corpus[..limit];

    let db = open_bench_db();
    let run_id = "cascade-live-bench";
    db.execute(
        "INSERT INTO ai_classification_runs(
            id,model,profile,prompt_version,status,total,started_at
         ) VALUES (?1,?2,'default',?3,'running',?4,?5)",
        params![
            run_id,
            selection.run_model_label(),
            ACTIVE_TAXONOMY_VERSION,
            cases.len() as i64,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .expect("ai run");

    let mut seeded = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        let mode = seed_mode(index, case);
        let contact = seed_contact(&db, case, index, mode);
        seeded.push((case, mode, contact));
    }

    let (taxonomy, display_names, labels) = taxonomy_fixture();
    let it_gazetteer = crate::gazetteer::catalog(&db, "Italia").expect("it gazetteer");
    let ch_gazetteer = crate::gazetteer::catalog(&db, "Svizzera").unwrap_or_default();
    let typesafe = TypeSafeClient::new(
        api_key,
        selection.jev_model.clone(),
        Duration::from_secs(120),
    );

    let mut category_fill = FieldScore::default();
    let mut category_replace = FieldScore::default();
    let mut subcategory_fill = FieldScore::default();
    let mut subcategory_replace = FieldScore::default();
    let mut geo_fill = FieldScore::default();
    let mut escalated = 0usize;
    let mut conflict_ok = 0usize;
    let mut conflict_n = 0usize;
    let mut gap_ok = 0usize;
    let mut gap_n = 0usize;
    let mut insufficient_ok = 0usize;
    let mut insufficient_n = 0usize;
    let mut rtts = Vec::new();
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_jev_calls = 0u64;
    let started_all = Instant::now();

    for chunk in seeded.chunks(32) {
        let items = chunk
            .iter()
            .map(|(case, _, _)| {
                let source_context =
                    classification_source_context(&db, case.id).expect("source context");
                BatchContactItem {
                    id: case.id.to_owned(),
                    country: case.country.to_owned(),
                    source_context,
                    previous_errors: Vec::new(),
                    current_category: None,
                }
            })
            .collect::<Vec<_>>();

        let started = Instant::now();
        let (raw, results) = call_cascade_batch_unified(
            &typesafe,
            generative.as_ref(),
            &items,
            &it_gazetteer,
            &ch_gazetteer,
            selection.spend_policy(),
            crate::ai_cascade::CascadeTaxonomy {
                taxonomy: &taxonomy,
                display_names: &display_names,
                labels: &labels,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("cascade batch failed: {}", error.message));
        let wall_ms = started.elapsed().as_secs_f64() * 1_000.0;

        let envelope = serde_json::from_str::<serde_json::Value>(&raw).ok();
        if let Some(jev_rtt) = envelope
            .as_ref()
            .and_then(|value| value.pointer("/cascade/jev_rtt"))
        {
            println!("[jev_rtt] {jev_rtt}");
            if let Some(mean) = jev_rtt.get("mean_ms").and_then(|v| v.as_f64()) {
                rtts.push(mean);
            }
        }
        if let Some(jev_usage) = envelope
            .as_ref()
            .and_then(|value| value.pointer("/cascade/jev_usage"))
        {
            println!("[jev_usage] {jev_usage}");
            if let Some(input) = jev_usage.get("input_tokens").and_then(|v| v.as_u64()) {
                total_input_tokens += input;
            }
            if let Some(output) = jev_usage.get("output_tokens").and_then(|v| v.as_u64()) {
                total_output_tokens += output;
            }
            if let Some(calls) = jev_usage.get("calls").and_then(|v| v.as_u64()) {
                total_jev_calls += calls;
            }
        }
        let escalated_ids = envelope
            .as_ref()
            .and_then(|value| value.pointer("/cascade/escalated"))
            .and_then(|ids| ids.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect::<std::collections::HashSet<_>>();

        for ((case, mode, contact), item) in chunk.iter().zip(items.iter()) {
            let before_cat = contact.category.clone();
            let before_sub = contact.subcategory.clone();
            let result = results.iter().find(|result| result.contact_id == case.id);
            let did_escalate = escalated_ids.contains(case.id);
            if did_escalate {
                escalated += 1;
            }

            match result {
                Some(batch) => match &batch.result {
                    Ok(unified) => {
                        let hash = format!("bench-{}", case.id);
                        let _ = apply_taxonomy_result(
                            &db,
                            contact,
                            &unified.taxonomy,
                            &selection.run_model_label(),
                            &hash,
                            ACTIVE_TAXONOMY_VERSION,
                            run_id,
                        );
                        let _ = apply_geography_result(&db, contact, &unified.geography, run_id);
                    }
                    Err(errors) => eprintln!("invalid {}: {errors:?}", case.id),
                },
                None => eprintln!("missing {}", case.id),
            }

            let (after_cat, after_sub, after_comune, state) = read_contact_snapshot(&db, case.id);
            let kinds = change_kinds(&db, case.id);

            if case.category == "__conflict__" {
                conflict_n += 1;
                if state == "conflicting" || state.contains("conflict") {
                    conflict_ok += 1;
                }
            } else if case.category == "__taxonomy_gap__" {
                gap_n += 1;
                if state == "taxonomy_gap" {
                    gap_ok += 1;
                }
            } else if case.category == "__insufficient__" {
                insufficient_n += 1;
                if state == "insufficient_evidence" {
                    insufficient_ok += 1;
                }
            } else if is_label_case(case) {
                let cat_ok = after_cat.eq_ignore_ascii_case(case.category);
                let sub_ok = same_label(after_sub.as_deref(), case.subcategory);
                match mode {
                    SeedMode::Fill => {
                        category_fill.record(cat_ok);
                        if case.subcategory.is_some() {
                            subcategory_fill.record(sub_ok);
                        }
                    }
                    SeedMode::Replace => {
                        category_replace.record(cat_ok);
                        if case.subcategory.is_some() {
                            subcategory_replace.record(sub_ok);
                        }
                    }
                }
                if let Some(gold_geo) = case.geography {
                    // Geography apply only fills empty comune; seed always starts empty.
                    let geo_ok = after_comune
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(gold_geo));
                    geo_fill.record(geo_ok);
                }
            }

            let mismatch = is_label_case(case)
                && (!after_cat.eq_ignore_ascii_case(case.category)
                    || !same_label(after_sub.as_deref(), case.subcategory));
            println!(
                "[{}] mode={:?} before={}/{:?} after={}/{:?} geo={:?} state={} escalated={} cat_kind={:?} sub_kind={:?}{}",
                case.id,
                mode,
                before_cat,
                before_sub,
                after_cat,
                after_sub,
                after_comune,
                state,
                did_escalate,
                kinds.get("category"),
                kinds.get("subcategory"),
                if mismatch { " MISMATCH" } else { "" }
            );
            let _ = item;
        }

        println!(
            "[progress] wall_ms={:.0} cat_fill={:.1}% ({}/{}) cat_repl={:.1}% ({}/{}) sub_fill={:.1}% ({}/{}) sub_repl={:.1}% ({}/{}) geo_fill={:.1}% ({}/{}) escalate={:.1}%",
            wall_ms,
            category_fill.rate() * 100.0,
            category_fill.correct,
            category_fill.attempted,
            category_replace.rate() * 100.0,
            category_replace.correct,
            category_replace.attempted,
            subcategory_fill.rate() * 100.0,
            subcategory_fill.correct,
            subcategory_fill.attempted,
            subcategory_replace.rate() * 100.0,
            subcategory_replace.correct,
            subcategory_replace.attempted,
            geo_fill.rate() * 100.0,
            geo_fill.correct,
            geo_fill.attempted,
            escalated as f64 * 100.0 / cases.len().max(1) as f64,
        );
    }

    let report = json!({
        "provider": "cascade",
        "model": selection.run_model_label(),
        "cases": cases.len(),
        "elapsed_ms": started_all.elapsed().as_millis(),
        "escalation_rate": escalated as f64 / cases.len().max(1) as f64,
        "jev_rtt_mean_ms": if rtts.is_empty() { json!(null) } else { json!(rtts.iter().sum::<f64>() / rtts.len() as f64) },
        "jev_usage": {
            "input_tokens": total_input_tokens,
            "output_tokens": total_output_tokens,
            "calls": total_jev_calls,
            "input_tokens_per_contact": if cases.is_empty() { 0.0 } else { total_input_tokens as f64 / cases.len() as f64 },
            "estimated_input_usd": total_input_tokens as f64 / 1_000_000.0 * 0.042,
        },
        "category_fill": { "correct": category_fill.correct, "attempted": category_fill.attempted, "rate": category_fill.rate() },
        "category_replace": { "correct": category_replace.correct, "attempted": category_replace.attempted, "rate": category_replace.rate() },
        "subcategory_fill": { "correct": subcategory_fill.correct, "attempted": subcategory_fill.attempted, "rate": subcategory_fill.rate() },
        "subcategory_replace": { "correct": subcategory_replace.correct, "attempted": subcategory_replace.attempted, "rate": subcategory_replace.rate() },
        "geo_comune_fill": { "correct": geo_fill.correct, "attempted": geo_fill.attempted, "rate": geo_fill.rate() },
        "conflict_quality": if conflict_n == 0 { json!(null) } else { json!(conflict_ok as f64 / conflict_n as f64) },
        "taxonomy_gap_quality": if gap_n == 0 { json!(null) } else { json!(gap_ok as f64 / gap_n as f64) },
        "insufficient_quality": if insufficient_n == 0 { json!(null) } else { json!(insufficient_ok as f64 / insufficient_n as f64) },
    });
    println!("{report}");

    assert!(
        category_fill.attempted + category_replace.attempted > 0,
        "expected labeled taxonomy cases"
    );
    let labeled_ok = category_fill.correct + category_replace.correct;
    let labeled_n = category_fill.attempted + category_replace.attempted;
    assert!(
        labeled_ok as f64 / labeled_n as f64 >= 0.5,
        "category apply accuracy too low: {labeled_ok}/{labeled_n}"
    );
}
