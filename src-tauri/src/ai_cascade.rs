use crate::ai_batch::{BatchContactItem, BatchItemResult, call_provider_batch_unified};
use crate::ai_provider::{InferenceProvider, ProviderError};
use crate::ai_runs::UnifiedResult;
use crate::ai_typesafe::{
    CascadeAction, CascadeJudgment, FieldProfile, JEV_CONTACT_BATCH, PlaceCandidates, SpendPolicy,
    TypeSafeClient, apply_unique_place_answers, build_batched_gap_questions,
    build_batched_pass1_questions, build_batched_subcategory_questions,
    category_needs_gap_followup, category_needs_subcategory_followup, clear_weak_subcategory,
    compact_jev_fields_with_profile, decide_cascade_action, extract_place_candidates,
    is_high_value_contact, judgment_to_unified, optional_subcategory_answer,
    parse_projected_cascade_judgment, project_contact_answers, sanitize_subcategory_for_category,
    synthesize_policy_answers, taxonomy_state_blob_filtered,
};
use futures::{StreamExt, stream};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Parallel System One contact-batches (each batch is up to JEV_CONTACT_BATCH contacts).
const TYPESAFE_BATCH_CONCURRENCY: usize = 3;
/// Muse Spark at `high` can truncate large unified batches; keep generative chunks small.
const GENERATIVE_BATCH_SIZE: usize = 4;
/// Concurrent OpenCode Muse Spark chunks (each chunk is its own CLI invoke).
#[allow(dead_code)]
const GENERATIVE_PARALLEL: usize = 2;

struct JevPass {
    item: BatchContactItem,
    places: PlaceCandidates,
    judgment: CascadeJudgment,
    action: CascadeAction,
    rtt_ms: f64,
}

enum JevOutcome {
    Ready(Box<JevPass>),
    Escalate {
        item: BatchContactItem,
        trace: Value,
        rtt_ms: Option<f64>,
    },
    Fatal(ProviderError),
}

fn client_rtt_ms(response: &Value) -> Option<f64> {
    response.get("_client_rtt_ms").and_then(Value::as_f64)
}

fn merge_usage_into(total: &mut Map<String, Value>, response: &Value) {
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
    let prev_in = total
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let prev_out = total
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let prev_calls = total.get("calls").and_then(Value::as_u64).unwrap_or(0);
    total.insert("input_tokens".to_owned(), json!(prev_in + input));
    total.insert("output_tokens".to_owned(), json!(prev_out + output));
    total.insert("calls".to_owned(), json!(prev_calls + 1));
}

fn escalate_trace(item: &BatchContactItem, rtt_ms: Option<f64>, extra: Value) -> Value {
    let mut trace = json!({
        "contact_id": item.id,
        "provider": crate::ai_typesafe::TYPESAFE_PROVIDER_ID,
        "action": "escalate",
        "rtt_ms": rtt_ms,
    });
    if let (Some(object), Some(extra_object)) = (trace.as_object_mut(), extra.as_object()) {
        for (key, value) in extra_object {
            object.insert(key.clone(), value.clone());
        }
    }
    trace
}

fn judgment_trace(item_id: &str, pass: &JevPass) -> Value {
    json!({
        "contact_id": item_id,
        "provider": crate::ai_typesafe::TYPESAFE_PROVIDER_ID,
        "action": format!("{:?}", pass.action).to_ascii_lowercase(),
        "rtt_ms": pass.rtt_ms,
        "disposition": pass.judgment.disposition.choice,
        "disposition_confidence": pass.judgment.disposition.confidence,
        "category": pass.judgment.category.choice,
        "category_confidence": pass.judgment.category.confidence,
        "subcategory": pass.judgment.subcategory.choice,
        "subcategory_confidence": pass.judgment.subcategory.confidence,
        "needs_taxonomy_gap": pass.judgment.needs_taxonomy_gap.noul,
        "municipality": pass.judgment.municipality.as_ref().map(|answer| &answer.choice),
        "admin_area": pass.judgment.province.as_ref().map(|answer| &answer.choice),
        "region": pass.judgment.region.as_ref().map(|answer| &answer.choice),
    })
}

fn contact_email(item: &BatchContactItem) -> String {
    let fields = compact_jev_fields_with_profile(&item.source_context, FieldProfile::Slim);
    fields
        .get("Email")
        .or_else(|| fields.get("PEC"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn build_batch_state(
    chunk: &[BatchContactItem],
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
    subcategory_categories: Option<&[String]>,
    include_all_subcategories: bool,
    profiles: &[FieldProfile],
) -> Value {
    let contacts = chunk
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let profile = profiles.get(index).copied().unwrap_or(FieldProfile::Slim);
            let mut entry = json!({
                "contact_id": item.id,
                "country": item.country,
                "fields": compact_jev_fields_with_profile(&item.source_context, profile),
            });
            if !item.previous_errors.is_empty() {
                entry
                    .as_object_mut()
                    .expect("object")
                    .insert("previous_errors".to_owned(), json!(item.previous_errors));
            }
            entry
        })
        .collect::<Vec<_>>();
    json!({
        "taxonomy": taxonomy_state_blob_filtered(
            taxonomy,
            display_names,
            subcategory_categories,
            include_all_subcategories,
        ),
        "contacts": contacts,
    })
}

async fn run_jev_contact_batch(
    typesafe: &TypeSafeClient,
    chunk: Vec<BatchContactItem>,
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
    policy: SpendPolicy,
) -> (Vec<JevOutcome>, Map<String, Value>) {
    let mut usage_total = Map::new();
    let high_value = chunk
        .iter()
        .map(|item| {
            let email = contact_email(item);
            is_high_value_contact(&email, &item.source_context)
        })
        .collect::<Vec<_>>();
    let places = chunk
        .iter()
        .map(|item| extract_place_candidates(&item.source_context, &item.country))
        .collect::<Vec<_>>();
    let pass1_profiles = high_value
        .iter()
        .map(|valuable| {
            if *valuable {
                FieldProfile::Full
            } else {
                FieldProfile::Slim
            }
        })
        .collect::<Vec<_>>();
    let state = build_batch_state(
        &chunk,
        taxonomy,
        display_names,
        Some(&[]),
        false,
        &pass1_profiles,
    );
    let questions = build_batched_pass1_questions(chunk.len(), taxonomy, display_names, &places);

    let pass1 = match typesafe.system_one(state.clone(), questions).await {
        Ok(response) => response,
        // Never dump a Jev outage onto Muse Spark: wait and retry Jev instead.
        Err(error) => {
            return (
                chunk
                    .into_iter()
                    .map(|_| JevOutcome::Fatal(error.clone()))
                    .collect(),
                usage_total,
            );
        }
    };
    merge_usage_into(&mut usage_total, &pass1);
    let rtt_ms = client_rtt_ms(&pass1).unwrap_or(0.0);
    let answers = pass1.get("answers").cloned().unwrap_or(json!({}));

    let mut draft: Vec<(
        BatchContactItem,
        PlaceCandidates,
        Result<CascadeJudgment, String>,
    )> = Vec::with_capacity(chunk.len());
    for (index, item) in chunk.into_iter().enumerate() {
        let projected = project_contact_answers(&answers, index);
        let parsed = parse_projected_cascade_judgment(projected, pass1.clone())
            .map_err(|error| error.message)
            .map(|mut judgment| {
                apply_unique_place_answers(&mut judgment, &places[index]);
                judgment
            });
        draft.push((item, places[index].clone(), parsed));
    }

    let gap_indices = draft
        .iter()
        .enumerate()
        .filter_map(|(index, (_, _, parsed))| {
            parsed
                .as_ref()
                .ok()
                .and_then(|judgment| category_needs_gap_followup(judgment, policy).then_some(index))
        })
        .collect::<Vec<_>>();

    if !gap_indices.is_empty() {
        // Follow-ups carry only the contacts they ask about, re-indexed from 0.
        let gap_items = gap_indices
            .iter()
            .map(|index| draft[*index].0.clone())
            .collect::<Vec<_>>();
        let gap_profiles = gap_indices
            .iter()
            .map(|index| pass1_profiles[*index])
            .collect::<Vec<_>>();
        let gap_state = build_batch_state(
            &gap_items,
            taxonomy,
            display_names,
            Some(&[]),
            false,
            &gap_profiles,
        );
        let local_indices = (0..gap_indices.len()).collect::<Vec<_>>();
        let gap_questions = build_batched_gap_questions(&local_indices);
        match typesafe.system_one(gap_state, gap_questions).await {
            Ok(gap_response) => {
                merge_usage_into(&mut usage_total, &gap_response);
                let gap_answers = gap_response.get("answers").cloned().unwrap_or(json!({}));
                for (local, index) in gap_indices.iter().enumerate() {
                    let projected = project_contact_answers(&gap_answers, local);
                    if let Some((_, _, Ok(judgment))) = draft.get_mut(*index)
                        && let Ok(Some(gap)) = crate::ai_typesafe::optional_gap_answer(&projected)
                    {
                        judgment.needs_taxonomy_gap = gap;
                    }
                }
            }
            Err(error) => {
                return (
                    draft
                        .into_iter()
                        .map(|(_, _, _)| JevOutcome::Fatal(error.clone()))
                        .collect(),
                    usage_total,
                );
            }
        }
    }

    // Paid subcategory only for high-value segments in the medium-confidence band.
    let mut sub_slots = Vec::new();
    for (index, (_, _, parsed)) in draft.iter().enumerate() {
        if let Ok(judgment) = parsed
            && category_needs_subcategory_followup(
                judgment,
                taxonomy,
                policy,
                high_value.get(index).copied().unwrap_or(false),
            )
        {
            sub_slots.push((index, judgment.category.choice.clone()));
        }
    }

    if !sub_slots.is_empty() {
        let sub_cats = sub_slots
            .iter()
            .map(|(_, category)| category.clone())
            .collect::<Vec<_>>();
        let sub_items = sub_slots
            .iter()
            .map(|(index, _)| draft[*index].0.clone())
            .collect::<Vec<_>>();
        let sub_profiles = vec![FieldProfile::Full; sub_items.len()];
        let sub_state = build_batch_state(
            &sub_items,
            taxonomy,
            display_names,
            Some(&sub_cats),
            false,
            &sub_profiles,
        );
        let local_slots = sub_slots
            .iter()
            .enumerate()
            .map(|(local, (_, category))| (local, category.clone()))
            .collect::<Vec<_>>();
        let sub_questions =
            build_batched_subcategory_questions(&local_slots, taxonomy, display_names);
        match typesafe.system_one(sub_state, sub_questions).await {
            Ok(sub_response) => {
                merge_usage_into(&mut usage_total, &sub_response);
                let sub_answers = sub_response.get("answers").cloned().unwrap_or(json!({}));
                for (local, (index, _)) in sub_slots.iter().enumerate() {
                    let projected = project_contact_answers(&sub_answers, local);
                    if let Some((_, _, Ok(judgment))) = draft.get_mut(*index)
                        && let Ok(Some(sub)) = optional_subcategory_answer(&projected)
                    {
                        judgment.subcategory = sub;
                    }
                }
            }
            Err(error) => {
                return (
                    draft
                        .into_iter()
                        .map(|(_, _, _)| JevOutcome::Fatal(error.clone()))
                        .collect(),
                    usage_total,
                );
            }
        }
    }

    let outcomes = draft
        .into_iter()
        .enumerate()
        .map(|(index, (item, places, parsed))| match parsed {
            Ok(mut judgment) => {
                let valuable = high_value.get(index).copied().unwrap_or(false);
                synthesize_policy_answers(&mut judgment, policy);
                sanitize_subcategory_for_category(&mut judgment, taxonomy);
                let mut action = decide_cascade_action(&judgment, taxonomy, policy, valuable);
                if matches!(action, CascadeAction::Insufficient)
                    && crate::ai_typesafe::confirms_current_category(
                        &judgment,
                        item.current_category.as_deref(),
                        display_names,
                    )
                {
                    // Category-only confirmation: keep the existing subcategory.
                    judgment.subcategory = crate::ai_typesafe::ChoiceAnswer {
                        choice: "none_fit".to_owned(),
                        confidence: 0.0,
                        probabilities: HashMap::new(),
                    };
                    action = CascadeAction::Apply;
                }
                if matches!(action, CascadeAction::Apply) {
                    let apply_bar =
                        crate::ai_typesafe::effective_apply_confidence(&judgment, valuable, policy);
                    clear_weak_subcategory(&mut judgment, apply_bar);
                }
                if matches!(action, CascadeAction::Escalate) {
                    JevOutcome::Escalate {
                        rtt_ms: Some(rtt_ms),
                        trace: escalate_trace(
                            &item,
                            Some(rtt_ms),
                            json!({
                                "disposition": judgment.disposition.choice,
                                "disposition_confidence": judgment.disposition.confidence,
                                "category": judgment.category.choice,
                                "category_confidence": judgment.category.confidence,
                                "subcategory": judgment.subcategory.choice,
                                "subcategory_confidence": judgment.subcategory.confidence,
                                "needs_taxonomy_gap": judgment.needs_taxonomy_gap.noul,
                                "high_value": valuable,
                            }),
                        ),
                        item,
                    }
                } else {
                    JevOutcome::Ready(Box::new(JevPass {
                        item,
                        places,
                        judgment,
                        action,
                        rtt_ms,
                    }))
                }
            }
            Err(message) => JevOutcome::Escalate {
                rtt_ms: Some(rtt_ms),
                trace: escalate_trace(&item, Some(rtt_ms), json!({ "error": message })),
                item,
            },
        })
        .collect();

    (outcomes, usage_total)
}

/// The taxonomy vocabulary a cascade batch classifies against.
pub(crate) struct CascadeTaxonomy<'a> {
    pub(crate) taxonomy: &'a [(String, Vec<String>)],
    pub(crate) display_names: &'a HashMap<(String, Option<String>, String), String>,
    pub(crate) labels: &'a str,
}

pub(crate) async fn call_cascade_batch_unified(
    typesafe: &TypeSafeClient,
    generative: &dyn InferenceProvider,
    items: &[BatchContactItem],
    it_gazetteer: &[crate::gazetteer::Place],
    ch_gazetteer: &[crate::gazetteer::Place],
    policy: SpendPolicy,
    vocabulary: CascadeTaxonomy<'_>,
) -> Result<(String, Vec<BatchItemResult<UnifiedResult>>), ProviderError> {
    let CascadeTaxonomy {
        taxonomy,
        display_names,
        labels,
    } = vocabulary;
    typesafe.ensure_warm().await;

    let chunk_outcomes =
        stream::iter(
            items
                .chunks(JEV_CONTACT_BATCH)
                .map(|chunk| chunk.to_vec())
                .collect::<Vec<_>>(),
        )
        .map(|chunk| {
            let typesafe = typesafe.clone();
            let taxonomy = taxonomy.to_vec();
            let display_names = display_names.clone();
            async move {
                run_jev_contact_batch(&typesafe, chunk, &taxonomy, &display_names, policy).await
            }
        })
        .buffer_unordered(TYPESAFE_BATCH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut usage_total = Map::new();
    usage_total.insert("input_tokens".to_owned(), json!(0));
    usage_total.insert("output_tokens".to_owned(), json!(0));
    usage_total.insert("calls".to_owned(), json!(0));

    let mut passes = Vec::new();
    for (outcomes, chunk_usage) in chunk_outcomes {
        let chunk_in = chunk_usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let chunk_out = chunk_usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let chunk_calls = chunk_usage
            .get("calls")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let prev_in = usage_total
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let prev_out = usage_total
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let prev_calls = usage_total
            .get("calls")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        usage_total.insert("input_tokens".to_owned(), json!(prev_in + chunk_in));
        usage_total.insert("output_tokens".to_owned(), json!(prev_out + chunk_out));
        usage_total.insert("calls".to_owned(), json!(prev_calls + chunk_calls));

        for outcome in outcomes {
            match outcome {
                JevOutcome::Ready(pass) => passes.push(Ok(*pass)),
                JevOutcome::Escalate {
                    item,
                    trace,
                    rtt_ms,
                } => passes.push(Err((item, trace, rtt_ms))),
                JevOutcome::Fatal(error) => return Err(error),
            }
        }
    }

    let mut ready = Vec::new();
    let mut escalate_items = Vec::new();
    let mut rtts = Vec::new();
    for pass in passes {
        match pass {
            Ok(pass) => {
                rtts.push(pass.rtt_ms);
                ready.push(pass);
            }
            Err((item, _trace, rtt_ms)) => {
                if let Some(ms) = rtt_ms {
                    rtts.push(ms);
                }
                escalate_items.push(item);
            }
        }
    }

    let mut results = ready
        .into_iter()
        .map(|pass| {
            let unified = judgment_to_unified(
                &pass.judgment,
                &pass.action,
                &pass.item.source_context,
                &pass.places,
            );
            BatchItemResult {
                contact_id: pass.item.id.clone(),
                result: Ok(unified),
                raw_item_json: Some(judgment_trace(&pass.item.id, &pass).to_string()),
                source: Some("jev".to_owned()),
            }
        })
        .collect::<Vec<_>>();

    let escalated_ids = escalate_items
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();

    // Muse Spark/pi escalation: small chunks; ai_runs owns the high-effort retry ladder.
    for chunk in escalate_items.chunks(GENERATIVE_BATCH_SIZE) {
        let (_raw, items) = call_provider_batch_unified(
            generative,
            chunk,
            taxonomy,
            it_gazetteer,
            ch_gazetteer,
            labels,
        )
        .await?;
        results.extend(items);
    }

    let mean_rtt = if rtts.is_empty() {
        0.0
    } else {
        rtts.iter().sum::<f64>() / rtts.len() as f64
    };
    let envelope = json!({
        "cascade": {
            "jev_batch_size": JEV_CONTACT_BATCH,
            "jev_usage": usage_total,
            "jev_rtt": { "mean_ms": mean_rtt, "samples": rtts.len() },
            "escalated": escalated_ids,
            "policy": {
                "apply_confidence": policy.apply_confidence,
                "subcategory_floor": policy.subcategory_floor,
                "cheap_skip_sub_above": policy.cheap_skip_sub_above,
                "gap_noul_threshold": policy.gap_noul_threshold,
            },
        }
    });
    Ok((envelope.to_string(), results))
}

/// Jev alone, no generative escalation: the `Apply` judgments (at or above
/// the policy's confidence) as `id -> (category key, subcategory key)`. Items
/// Jev is unsure about are left out; the caller decides what to do with them.
pub(crate) async fn jev_classify(
    typesafe: &TypeSafeClient,
    items: Vec<BatchContactItem>,
    taxonomy: &[(String, Vec<String>)],
    display_names: &HashMap<(String, Option<String>, String), String>,
    policy: SpendPolicy,
) -> Result<HashMap<String, (String, Option<String>)>, ProviderError> {
    typesafe.ensure_warm().await;
    let mut accepted = HashMap::new();
    for chunk in items.chunks(JEV_CONTACT_BATCH) {
        let (outcomes, _) =
            run_jev_contact_batch(typesafe, chunk.to_vec(), taxonomy, display_names, policy).await;
        for outcome in outcomes {
            match outcome {
                JevOutcome::Ready(pass) if pass.action == CascadeAction::Apply => {
                    let subcategory = (pass.judgment.subcategory.choice != "none_fit")
                        .then(|| pass.judgment.subcategory.choice.clone());
                    accepted.insert(
                        pass.item.id.clone(),
                        (pass.judgment.category.choice.clone(), subcategory),
                    );
                }
                JevOutcome::Fatal(error) => return Err(error),
                _ => {}
            }
        }
    }
    Ok(accepted)
}
