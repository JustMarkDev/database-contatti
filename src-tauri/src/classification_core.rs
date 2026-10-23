use crate::{EvidenceGrade, GroundedSourceReference, TaxonomyOutcome, TaxonomyResult};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TaxonomyCommitPlan {
    Retain {
        state: &'static str,
        reason: Option<String>,
        evidence: Vec<String>,
    },
    ProposeTaxonomyGap,
    Apply {
        category: String,
        subcategory: Option<String>,
        agreement: bool,
        confidence: f64,
        evidence: Vec<String>,
    },
}

fn evidence_strings<'a>(
    references: impl Iterator<Item = &'a GroundedSourceReference>,
) -> Vec<String> {
    references
        .map(|reference| {
            format!(
                "[{} · {}] {}",
                reference.source_id, reference.field, reference.quote
            )
        })
        .collect()
}

fn grade_confidence(grade: Option<&EvidenceGrade>) -> f64 {
    match grade {
        Some(EvidenceGrade::Direct) => 0.9,
        Some(EvidenceGrade::Corroborated) => 0.8,
        _ => 0.6,
    }
}

pub(crate) fn plan_taxonomy_commit(
    current_category: &str,
    current_subcategory: Option<&str>,
    result: &TaxonomyResult,
    resolved_candidate: Option<(String, Option<String>)>,
) -> Result<TaxonomyCommitPlan, String> {
    match result.outcome {
        TaxonomyOutcome::Insufficient => Ok(TaxonomyCommitPlan::Retain {
            state: "insufficient_evidence",
            reason: result.summary.clone(),
            evidence: Vec::new(),
        }),
        TaxonomyOutcome::Conflict => Ok(TaxonomyCommitPlan::Retain {
            state: "conflicting",
            reason: result.summary.clone(),
            evidence: evidence_strings(result.candidates.iter().flat_map(|candidate| {
                candidate
                    .category_evidence
                    .iter()
                    .chain(candidate.subcategory_evidence.iter())
            })),
        }),
        TaxonomyOutcome::TaxonomyGap => Ok(TaxonomyCommitPlan::ProposeTaxonomyGap),
        TaxonomyOutcome::Proposal => {
            let candidate = result
                .candidates
                .first()
                .ok_or_else(|| "proposta di tassonomia senza candidati".to_owned())?;
            let (category, subcategory) = resolved_candidate
                .ok_or_else(|| "candidato della proposta di tassonomia non risolto".to_owned())?;
            let category_same = category.eq_ignore_ascii_case(current_category);
            let subcategory_same = subcategory
                .as_deref()
                .zip(current_subcategory)
                .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right));
            let confidence = grade_confidence(Some(&candidate.category_grade)).min(
                subcategory
                    .as_ref()
                    .map(|_| grade_confidence(candidate.subcategory_grade.as_ref()))
                    .unwrap_or(0.9),
            );
            let mut evidence = evidence_strings(
                candidate.category_evidence.iter().chain(
                    subcategory
                        .as_ref()
                        .map(|_| candidate.subcategory_evidence.iter())
                        .unwrap_or_default(),
                ),
            );
            if let Some(summary) = result
                .summary
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                evidence.push(format!("Motivo: {summary}"));
            }
            Ok(TaxonomyCommitPlan::Apply {
                category,
                subcategory,
                agreement: category_same
                    && (candidate.subcategory_key.is_none() || subcategory_same),
                confidence,
                evidence,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TaxonomyCandidate, TaxonomyGap};

    fn result(outcome: TaxonomyOutcome) -> TaxonomyResult {
        TaxonomyResult {
            outcome,
            candidates: Vec::new(),
            gap: None,
            summary: Some("diagnostic".to_owned()),
        }
    }

    #[test]
    fn conflicts_and_insufficient_evidence_retain_values_but_advance_state() {
        assert!(matches!(
            plan_taxonomy_commit("Varie", None, &result(TaxonomyOutcome::Insufficient), None)
                .unwrap(),
            TaxonomyCommitPlan::Retain {
                state: "insufficient_evidence",
                ..
            }
        ));
        assert!(matches!(
            plan_taxonomy_commit("Varie", None, &result(TaxonomyOutcome::Conflict), None).unwrap(),
            TaxonomyCommitPlan::Retain {
                state: "conflicting",
                ..
            }
        ));
    }

    #[test]
    fn proposal_produces_an_autonomous_apply_plan() {
        let mut proposal = result(TaxonomyOutcome::Proposal);
        proposal.candidates.push(TaxonomyCandidate {
            category_key: "istruzione".to_owned(),
            subcategory_key: Some("primaria".to_owned()),
            category_grade: EvidenceGrade::Direct,
            subcategory_grade: Some(EvidenceGrade::Corroborated),
            category_evidence: vec![GroundedSourceReference {
                source_id: "S1".to_owned(),
                field: "Nome".to_owned(),
                quote: "Scuola primaria".to_owned(),
            }],
            subcategory_evidence: Vec::new(),
        });
        let plan = plan_taxonomy_commit(
            "Varie",
            None,
            &proposal,
            Some(("Istruzione".to_owned(), Some("Primaria".to_owned()))),
        )
        .unwrap();
        assert!(matches!(
            plan,
            TaxonomyCommitPlan::Apply {
                agreement: false,
                confidence,
                ..
            } if (confidence - 0.8).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn taxonomy_gap_is_the_only_human_decision_plan() {
        let mut gap = result(TaxonomyOutcome::TaxonomyGap);
        gap.gap = Some(TaxonomyGap {
            level: "subcategory".to_owned(),
            nearest_category_key: Some("istruzione".to_owned()),
            nearest_subcategory_key: None,
            proposed_display_name: "Robotica".to_owned(),
            proposed_definition: "Scuola di robotica".to_owned(),
            parent_category_key: Some("istruzione".to_owned()),
            evidence: Vec::new(),
        });
        assert_eq!(
            plan_taxonomy_commit("Istruzione", None, &gap, None).unwrap(),
            TaxonomyCommitPlan::ProposeTaxonomyGap
        );
    }
}
