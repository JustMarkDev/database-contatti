use super::benchmark::*;
use std::collections::HashSet;

#[test]
fn corpus_labels_match_active_taxonomy() {
    let taxonomy: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
        crate::TAXONOMY
            .iter()
            .map(|(category, subcategories)| {
                (
                    *category,
                    subcategories
                        .iter()
                        .copied()
                        .collect::<std::collections::HashSet<_>>(),
                )
            })
            .collect();
    for case in reviewed_corpus() {
        if case.category.starts_with("__") {
            continue;
        }
        let Some(subcategories) = taxonomy.get(case.category) else {
            panic!("case {} uses unknown category {}", case.id, case.category);
        };
        if let Some(subcategory) = case.subcategory {
            assert!(
                subcategories.contains(subcategory),
                "case {} subcategory {:?} is not in active taxonomy for {}",
                case.id,
                subcategory,
                case.category
            );
        }
    }
}

#[test]
fn corpus_is_synthetic_and_covers_review_boundaries() {
    let cases = reviewed_corpus();
    assert_eq!(
        cases.len(),
        121,
        "the reviewed benchmark corpus must stay at 121 cases"
    );
    assert!(
        cases.len() >= 50,
        "corpus should contain at least 50 cases, found {}",
        cases.len()
    );

    let mut ids = HashSet::new();
    for case in &cases {
        assert!(
            ids.insert(case.id),
            "duplicate case id in reviewed corpus: {}",
            case.id
        );
        assert!(
            case.evidence.contains("Esempio")
                || case.evidence.contains("example.test")
                || case.evidence.contains("xlsx")
                || case.evidence.contains("csv")
                || case.evidence.contains("txt"),
            "evidence must use synthetic markers for case: {}",
            case.id
        );
        assert!(
            !case.id.is_empty(),
            "case id must not be empty for case: {:?}",
            case
        );
        assert!(
            !case.evidence.is_empty(),
            "evidence must not be empty for case: {}",
            case.id
        );
        assert!(
            !case.category.is_empty(),
            "category must not be empty for case: {}",
            case.id
        );
        assert!(
            !case.country.is_empty(),
            "country must not be empty for case: {}",
            case.id
        );
    }

    assert!(cases.iter().any(|c| c.country == "Italia"));
    assert!(cases.iter().any(|c| c.country == "Svizzera"));

    assert!(cases.iter().any(|c| c.contact_kind == "istituzionale"));
    assert!(cases.iter().any(|c| c.contact_kind == "personale"));
    assert!(cases.iter().any(|c| c.contact_kind == "non_determinato"));

    assert!(cases.iter().any(|c| c.category == "__taxonomy_gap__"));
    assert!(cases.iter().any(|c| c.category == "__not_applicable__"));
    assert!(cases.iter().any(|c| c.category == "__insufficient__"));

    let categories = [
        "Istruzione",
        "Pubblica amministrazione",
        "Sanità",
        "Associazioni",
        "Enti religiosi",
        "Cultura",
        "Turismo e promozione territoriale",
        "Varie",
    ];
    for cat in categories {
        assert!(
            cases.iter().any(|c| c.category == cat),
            "category '{}' must be represented in reviewed corpus",
            cat
        );
    }

    assert!(
        cases
            .iter()
            .any(|c| c.category == "Pubblica amministrazione"
                && c.subcategory == Some("Forze dell'ordine")),
        "Forze dell'ordine under Pubblica amministrazione must be represented"
    );

    assert!(
        cases
            .iter()
            .filter(|c| c.id.starts_with("sparse-file-"))
            .count()
            >= 4,
        "must cover sparse filename-only cases"
    );

    assert!(
        cases
            .iter()
            .filter(|c| c.id.starts_with("audit-agree-"))
            .count()
            >= 3,
        "must cover audit agreement cases"
    );
    assert!(
        cases
            .iter()
            .filter(|c| c.id.starts_with("audit-disagree-"))
            .count()
            >= 3,
        "must cover audit disagreement cases"
    );

    assert!(
        cases
            .iter()
            .filter(|c| c.id.starts_with("conflict-"))
            .count()
            >= 3,
        "must cover conflict cases"
    );

    assert!(
        cases.iter().any(|c| c.id == "gap-botanical-garden"),
        "botanical garden taxonomy gap case must be present"
    );
    assert!(
        cases.iter().any(|c| c.id == "direct-assoc-donatori-sangue"
            && c.category == "Associazioni"
            && c.subcategory == Some("Volontariato / solidarietà")),
        "blood donor association case must be present"
    );
    assert!(
        cases
            .iter()
            .any(|c| c.id == "gap-archivio-stato" && c.category == "__taxonomy_gap__"),
        "state archive taxonomy gap case must be present"
    );
    assert!(
        cases.iter().any(|c| c.id == "gap-consumer-group"),
        "consumer group taxonomy gap case must be present"
    );

    assert!(
        cases
            .iter()
            .filter(|c| c.id.starts_with("insufficient-"))
            .count()
            >= 3,
        "must cover insufficient evidence cases"
    );

    assert!(
        cases
            .iter()
            .any(|c| c.id == "geo-comune-roma" && c.geography == Some("Roma"))
    );
    assert!(
        cases
            .iter()
            .any(|c| c.id == "geo-provincia-firenze" && c.geography == Some("Firenze"))
    );
    assert!(
        cases
            .iter()
            .any(|c| c.id == "geo-regione-veneto" && c.geography == Some("Venezia"))
    );
    assert!(
        cases
            .iter()
            .any(|c| c.id == "geo-macroarea-sud" && c.geography == Some("Catanzaro"))
    );
    assert!(
        cases
            .iter()
            .any(|c| c.id == "geo-macroarea-isole" && c.geography == Some("Agrigento"))
    );
    assert!(
        cases
            .iter()
            .any(|c| c.id == "geo-macroarea-nordovest" && c.geography == Some("Torino"))
    );

    assert!(
        cases.iter().any(|c| c.id == "swiss-lugano-ticino"
            && c.country == "Svizzera"
            && c.geography == Some("Lugano")),
        "Swiss Lugano/Ticino case must be present"
    );
    assert!(
        cases.iter().any(|c| c.id == "swiss-bellinzona-ticino"
            && c.country == "Svizzera"
            && c.geography == Some("Bellinzona")),
        "Swiss Bellinzona/Ticino case must be present"
    );
    assert!(
        cases.iter().any(|c| c.id == "swiss-cultura-locarno"
            && c.country == "Svizzera"
            && c.geography == Some("Locarno")),
        "Swiss Locarno/Ticino case must be present"
    );
}

#[test]
fn persistence_seeds_every_taxonomy_level_idempotently() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE reviewed_benchmark_cases (case_id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, evidence TEXT NOT NULL, category TEXT NOT NULL, subcategory TEXT, contact_kind TEXT NOT NULL, country TEXT NOT NULL, geography TEXT, boundary TEXT NOT NULL, reviewed_at TEXT NOT NULL);").unwrap();
    persist_reviewed_benchmark(&db).unwrap();

    persist_reviewed_benchmark(&db).unwrap();

    let corpus_count = reviewed_corpus().len() as i64;
    let total_count: i64 = db
        .query_row("SELECT COUNT(*) FROM reviewed_benchmark_cases", [], |r| {
            r.get(0)
        })
        .unwrap();
    let taxonomy_levels: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM reviewed_benchmark_cases WHERE boundary='taxonomy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let boundary_cases: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM reviewed_benchmark_cases WHERE boundary='boundary'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let expected_taxonomy_levels: i64 = crate::TAXONOMY
        .iter()
        .map(|(_, subs)| subs.len())
        .sum::<usize>() as i64;

    assert_eq!(taxonomy_levels, expected_taxonomy_levels);
    assert_eq!(boundary_cases, corpus_count);
    assert_eq!(total_count, corpus_count + expected_taxonomy_levels);

    assert_eq!(
        db.query_row::<i64, _, _>(
            "SELECT COUNT(*) FROM reviewed_benchmark_cases WHERE email LIKE '%@example.test'",
            [],
            |r| r.get(0)
        )
        .unwrap(),
        total_count
    );

    let invalid_rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM reviewed_benchmark_cases WHERE case_id='' OR email='' OR evidence='' OR category='' OR contact_kind='' OR country='' OR boundary='' OR reviewed_at=''",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(invalid_rows, 0);
}

#[test]
fn metrics_keep_category_and_subcategory_independent() {
    let cases = reviewed_corpus();
    let predictions = cases
        .iter()
        .map(|c| BenchmarkPrediction {
            category: Some(c.category),
            subcategory: None,
            outcome: "proposal",
            automatic_fill: false,
            review_requested: false,
            escalated: false,
            latency_ms: 12.5,
            prompt_context_chars: 4096,
        })
        .collect::<Vec<_>>();
    let report = evaluate(&predictions);
    assert_eq!(report.cases, cases.len());
    assert_eq!(report.category_precision, 1.0);
    assert_eq!(report.category_coverage, 1.0);
    assert_eq!(report.subcategory_coverage, 0.0);
    assert_eq!(report.latency_ms, 12.5);
    assert_eq!(report.prompt_context_chars, 4096);
    assert_eq!(report.proposal_coverage, 1.0);
    assert_eq!(report.insufficient_rate, 0.0);
}

#[test]
fn outcomes_compute_review_quality_and_fill_errors_from_predictions() {
    let cases = reviewed_corpus();
    let predictions = cases
        .iter()
        .map(|c| BenchmarkPrediction {
            category: if c.id.starts_with("conflict") {
                Some("wrong")
            } else {
                Some(c.category)
            },
            subcategory: c.subcategory,
            outcome: if c.id.starts_with("conflict") {
                "conflict"
            } else if c.id.starts_with("gap") || c.category == "__taxonomy_gap__" {
                "taxonomy_gap"
            } else {
                "proposal"
            },
            automatic_fill: c.id.starts_with("sparse-file") || c.id.starts_with("conflict"),
            review_requested: c.id.starts_with("conflict")
                || c.id.starts_with("gap")
                || c.category == "__taxonomy_gap__",
            escalated: c.id.starts_with("gap") || c.id.starts_with("conflict"),
            latency_ms: 10.0,
            prompt_context_chars: 100,
        })
        .collect::<Vec<_>>();
    let report = evaluate(&predictions);
    assert!(report.automatic_fill_errors >= 1);
    assert_eq!(report.conflict_quality, 1.0);
    assert_eq!(report.taxonomy_gap_quality, 1.0);
    assert_eq!(report.useful_review_rate, 1.0);
}
