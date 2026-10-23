use crate::{
    DELIMITED_EXTENSIONS, ImportFile, SPREADSHEET_EXTENSIONS, is_blacklist_file, parse_file,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// The corpus holds real contacts: it lives in a private repo, cloned into `data/email`.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/email")
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/corpus-baseline.tsv")
}

fn is_supported(path: &Path) -> bool {
    let extension = format!(
        ".{}",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    );
    SPREADSHEET_EXTENSIONS.contains(&extension.as_str())
        || DELIMITED_EXTENSIONS.contains(&extension.as_str())
}

fn collect(base: &Path, root: &Path, files: &mut Vec<(String, PathBuf)>) {
    let entries = std::fs::read_dir(root).expect("corpus directory should be readable");
    for entry in entries {
        let path = entry.expect("corpus entry").path();
        if path.is_dir() {
            collect(base, &path, files);
        } else if path.is_file() && is_supported(&path) {
            let relative = path
                .strip_prefix(base)
                .expect("corpus paths are rooted at the corpus")
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
}

fn read_baseline() -> BTreeMap<String, usize> {
    let raw = std::fs::read_to_string(baseline_path()).unwrap_or_default();
    raw.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.rsplit_once('\t'))
        .map(|(name, count)| {
            (
                name.to_owned(),
                count.trim().parse::<usize>().expect("baseline count"),
            )
        })
        .collect()
}

#[test]
#[ignore = "real 25 MB import benchmark; run with `bun run test:performance`"]
fn benchmark_largest_real_import_file() {
    let relative = "Comuni/Comuni 7.txt";
    let path = corpus_root().join(relative);
    let bytes = std::fs::read(&path).expect("large benchmark fixture");
    assert!(
        bytes.len() >= 20_000_000,
        "benchmark fixture is no longer large"
    );
    let file = ImportFile {
        path: None,
        name: "Comuni 7.txt".to_owned(),
        relative_path: relative.to_owned(),
        bytes,
    };

    let started = Instant::now();
    let parsed = parse_file(&file).expect("large benchmark fixture should parse");
    let elapsed = started.elapsed();
    assert_eq!(parsed.parsed_rows, 107_519);
    assert_eq!(parsed.contacts.len(), 177_087);
    println!(
        "[performance] file={relative} bytes={} rows={} candidates={} parse_ms={:.3}",
        file.bytes.len(),
        parsed.parsed_rows,
        parsed.contacts.len(),
        elapsed.as_secs_f64() * 1_000.0,
    );
}

#[test]
#[ignore = "real large-file persistence benchmark; run with `bun run test:performance`"]
fn benchmark_largest_real_import_persistence() {
    const TIMED_OCCURRENCES: usize = 20_000;
    let relative = "Comuni/Comuni 7.txt";
    let path = corpus_root().join(relative);
    let file = ImportFile {
        path: None,
        name: "Comuni 7.txt".to_owned(),
        relative_path: relative.to_owned(),
        bytes: std::fs::read(&path).expect("large benchmark fixture"),
    };
    let mut parsed = parse_file(&file).expect("large benchmark fixture should parse");
    for contact in &mut parsed.contacts {
        contact.status = crate::EmailStatus::Valid;
    }
    let mut db = rusqlite::Connection::open_in_memory().expect("benchmark database");
    crate::schema::setup_schema(&db).expect("benchmark schema");
    let query_plan = {
        let mut statement = db
            .prepare("EXPLAIN QUERY PLAN SELECT id FROM contacts WHERE email_comparison_key=?1")
            .expect("deduplication query plan");
        statement
            .query_map(["utente@example.test"], |row| row.get::<_, String>(3))
            .expect("deduplication query plan rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("deduplication query plan details")
    };
    assert!(
        query_plan
            .iter()
            .any(|detail| detail.contains("idx_contacts_comparison_key")),
        "contact deduplication must use its index: {query_plan:?}"
    );
    let tx = db.transaction().expect("benchmark transaction");
    let mut cache = crate::contacts::ContactImportCache::default();
    crate::contacts::suspend_contact_triggers(&tx).expect("suspend fts");

    let started = Instant::now();
    for contact in parsed.contacts.iter().take(TIMED_OCCURRENCES) {
        crate::contacts::save_contact_for_import(
            &tx,
            contact,
            "",
            None,
            None,
            "2026-01-01T00:00:00Z",
            &mut cache,
        )
        .expect("persist benchmark contact");
    }
    crate::contacts::flush_contact_import_cache(&tx, &mut cache, "2026-01-01T00:00:00Z")
        .expect("flush benchmark contacts");
    crate::contacts::resume_contact_triggers(&tx).expect("resume fts");
    tx.commit().expect("commit benchmark contacts");
    let elapsed = started.elapsed();
    let canonical_contacts: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .expect("count benchmark contacts");
    println!(
        "[performance] file={relative} source_occurrences={} timed_occurrences={} canonical_contacts={} persist_ms={:.3}",
        parsed.contacts.len(),
        TIMED_OCCURRENCES,
        canonical_contacts,
        elapsed.as_secs_f64() * 1_000.0,
    );
}

#[test]
#[ignore = "real XLSX import benchmark; run with `bun run test:performance`"]
fn benchmark_largest_real_import_xlsx_file() {
    let relative = "Comuni/Comuni 4.xlsx";
    let path = corpus_root().join(relative);
    let bytes = std::fs::read(&path).expect("large xlsx benchmark fixture");
    assert!(
        bytes.len() >= 4_000_000,
        "xlsx benchmark fixture is no longer large"
    );
    let file = ImportFile {
        path: None,
        name: "Comuni 4.xlsx".to_owned(),
        relative_path: relative.to_owned(),
        bytes,
    };

    let started = Instant::now();
    let parsed = parse_file(&file).expect("large xlsx benchmark fixture should parse");
    let elapsed = started.elapsed();
    assert_eq!(parsed.parsed_rows, 107_519);
    assert_eq!(parsed.contacts.len(), 177_086);
    println!(
        "[performance] file={relative} bytes={} rows={} candidates={} parse_ms={:.3}",
        file.bytes.len(),
        parsed.parsed_rows,
        parsed.contacts.len(),
        elapsed.as_secs_f64() * 1_000.0,
    );
}

#[test]
#[ignore = "full real file pipeline benchmark; run with cargo test --release benchmark_full_pipeline -- --ignored --nocapture"]
fn benchmark_full_pipeline_multi_file() {
    let test_files = [
        "Comuni/Comuni 4.xlsx",
        "Comuni/Comuni 6.txt",
        "Parrocchie/Parrocchie 4.xlsx",
    ];

    let mut db = rusqlite::Connection::open_in_memory().expect("benchmark database");
    crate::schema::setup_schema(&db).expect("benchmark schema");

    for relative in test_files {
        let path = corpus_root().join(relative);
        if !path.exists() {
            println!("[skip] {} does not exist", relative);
            continue;
        }
        let bytes = std::fs::read(&path).expect("read fixture");
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        let file = ImportFile {
            path: None,
            name: file_name.clone(),
            relative_path: relative.to_owned(),
            bytes,
        };

        println!("\n=======================================================");
        println!("[benchmark] STARTING FULL PIPELINE: {}", relative);
        println!("=======================================================");

        let t_parse = Instant::now();
        let mut parsed = parse_file(&file).expect("parse file");
        let d_parse = t_parse.elapsed();
        println!(
            "[timing] 1. Parse: {:.3}s (rows={}, candidates={})",
            d_parse.as_secs_f64(),
            parsed.parsed_rows,
            parsed.contacts.len()
        );

        for contact in &mut parsed.contacts {
            contact.status = crate::EmailStatus::Valid;
        }

        let tx = db.transaction().expect("transaction");
        let now = "2026-01-01T00:00:00Z";
        let attempt_id = format!("attempt-{}", relative.replace(['/', ' ', '.'], "-"));
        let import_id = format!("test-{}", relative.replace(['/', ' ', '.'], "-"));

        tx.execute(
            "INSERT INTO import_attempts(id,status,parser_version,started_at) VALUES (?1,'running','parser-v2',?2)",
            rusqlite::params![attempt_id, now],
        ).expect("insert attempt");

        tx.execute(
            "INSERT INTO imports(id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at,
                import_attempt_id,content_sha256,hash_algorithm,file_size_bytes,parser_version,encoding,delimiter,sheet_count,started_at,completed_at,error_count,report_json)
             VALUES (?1,?2,?3,'hash','running',?4,?5,?6,?7,'hash','sha256',?8,'parser-v2',?9,?10,?11,?6,?6,0,'{}')",
            rusqlite::params![
                import_id, file.name, file.relative_path, parsed.parsed_rows as i64, parsed.contacts.len() as i64,
                now, attempt_id, file.bytes.len() as i64, parsed.encoding, parsed.delimiter, parsed.sheet_count as i64,
            ],
        ).expect("insert import");

        let t_src = Instant::now();
        let mut insert_source_row = tx.prepare_cached(
            "INSERT OR IGNORE INTO source_rows(id,import_id,source_sheet,row_number,raw_context,parse_status,error_message,candidate_count,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)"
        ).expect("prepare source rows");
        for source in &parsed.source_rows {
            if source.source_row == 0 {
                continue;
            }
            let row_id = format!(
                "{}:{}:{}",
                import_id,
                source.sheet.as_deref().unwrap_or(""),
                source.source_row
            );
            insert_source_row
                .execute(rusqlite::params![
                    row_id,
                    import_id,
                    source.sheet.as_deref().unwrap_or(""),
                    source.source_row as i64,
                    source.raw_context,
                    "parsed",
                    None::<String>,
                    source.candidate_count as i64,
                    now
                ])
                .expect("insert source row");
        }
        drop(insert_source_row);
        let d_src = t_src.elapsed();
        println!(
            "[timing] 2. Source rows ({} rows): {:.3}s",
            parsed.source_rows.len(),
            d_src.as_secs_f64()
        );

        let t_loop = Instant::now();
        let mut insert_candidate = tx.prepare_cached(
            "INSERT INTO email_candidates
             (id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
              extraction_method,contact_id,status,syntax_status,routing_status,mailbox_status,
              candidate_status,current_disposition,quality_score,score_factors_json,first_seen_at,last_seen_at,
              disposition_reason,category,subcategory,source_url,occurrence_index,outreach_eligible)
             VALUES (?1,?2,?3,?4,?5,?6,'parser',?7,'linked','syntax_ok','routing_ok','unknown','linked','linked',0.85,'{}',?8,?8,'routing_ok',?9,?10,?11,?12,1)"
        ).expect("prep candidate");
        let mut insert_disposition = tx.prepare_cached(
            "INSERT INTO candidate_dispositions(id,candidate_id,disposition,reason_code,created_at)
             VALUES (lower(hex(randomblob(16))),?1,'linked','routing_ok',?2)"
        ).expect("prep disp");

        let mut contact_cache = crate::contacts::ContactImportCache::default();
        crate::contacts::suspend_contact_triggers(&tx).expect("suspend fts");
        let mut current_source_row = (String::new(), usize::MAX, String::new());
        let mut occurrence_indexes = std::collections::HashMap::<String, usize>::new();

        let mut t_chunk = Instant::now();
        for (i, contact) in parsed.contacts.iter().enumerate() {
            let source_sheet = contact.source_sheet.as_deref().unwrap_or("");
            if current_source_row.0 != source_sheet || current_source_row.1 != contact.source_row {
                current_source_row = (
                    source_sheet.to_owned(),
                    contact.source_row,
                    format!("{}:{}:{}", import_id, source_sheet, contact.source_row),
                );
                occurrence_indexes.clear();
            }
            let source_row_id = &current_source_row.2;
            let occurrence_index = occurrence_indexes
                .entry(contact.comparison_key.clone())
                .or_default();
            let candidate_occurrence_index = *occurrence_index;
            *occurrence_index += 1;

            let candidate_id = crate::contacts::new_contact_id();
            let outcome = crate::contacts::save_contact_for_import(
                &tx,
                contact,
                "",
                Some(&import_id),
                Some(source_row_id),
                now,
                &mut contact_cache,
            )
            .expect("save contact");

            insert_candidate
                .execute(rusqlite::params![
                    candidate_id,
                    source_row_id,
                    contact.raw_email,
                    contact.normalized_email,
                    contact.comparison_key,
                    contact.normalization_method,
                    outcome.contact_id,
                    now,
                    contact.category,
                    contact.subcategory,
                    contact.source_url,
                    candidate_occurrence_index as i64
                ])
                .expect("insert candidate");

            crate::contacts::insert_classification_claim_deferred(
                &tx,
                now,
                crate::contacts::ClassificationClaim {
                    candidate_id: Some(&candidate_id),
                    contact_id: Some(&outcome.contact_id),
                    source_row_id: Some(source_row_id),
                    category: &contact.category,
                    subcategory: contact.subcategory.as_deref(),
                    confidence: contact.classification_confidence,
                    method: &contact.classification_method,
                    evidence: &contact.classification_evidence,
                },
            )
            .expect("claim");
            insert_disposition
                .execute(rusqlite::params![candidate_id, now])
                .expect("disp");

            if (i + 1) % 25_000 == 0 || i + 1 == parsed.contacts.len() {
                println!(
                    "[timing]    processed {}/{} candidates | last_25k={:.3}s total_loop={:.3}s",
                    i + 1,
                    parsed.contacts.len(),
                    t_chunk.elapsed().as_secs_f64(),
                    t_loop.elapsed().as_secs_f64()
                );
                t_chunk = Instant::now();
            }
        }
        drop(insert_candidate);
        drop(insert_disposition);
        let d_loop = t_loop.elapsed();
        println!(
            "[timing] 3. Candidate & contact loop total: {:.3}s",
            d_loop.as_secs_f64()
        );

        let t_flush = Instant::now();
        crate::contacts::flush_contact_import_cache(&tx, &mut contact_cache, now)
            .expect("flush cache");
        let d_flush = t_flush.elapsed();
        println!(
            "[timing] 4. Flush contact cache: {:.3}s",
            d_flush.as_secs_f64()
        );

        let t_recompute = Instant::now();
        crate::contacts::recompute_imported_classifications(&tx, &import_id).expect("recompute");
        let d_recompute = t_recompute.elapsed();
        println!(
            "[timing] 5. Recompute classifications: {:.3}s",
            d_recompute.as_secs_f64()
        );

        let t_fts = Instant::now();
        crate::contacts::resume_contact_triggers(&tx).expect("resume fts");
        let d_fts = t_fts.elapsed();
        println!(
            "[timing] 5b. Resume/rebuild contacts FTS: {:.3}s",
            d_fts.as_secs_f64()
        );

        let t_rec = Instant::now();
        let _persisted_rows: usize = tx
            .query_row(
                "SELECT COUNT(*) FROM source_rows WHERE import_id=?1",
                rusqlite::params![import_id],
                |row| Ok(row.get::<_, i64>(0)? as usize),
            )
            .expect("rec rows");
        let _persisted_candidates: usize = tx.query_row(
            "SELECT COUNT(*) FROM email_candidates ec JOIN source_rows sr ON sr.id=ec.source_row_id WHERE sr.import_id=?1",
            rusqlite::params![import_id], |row| Ok(row.get::<_, i64>(0)? as usize),
        ).expect("rec candidates");
        let _undisposed: usize = tx
            .query_row(
                "SELECT COUNT(*) FROM email_candidates ec
             JOIN source_rows sr ON sr.id=ec.source_row_id
             WHERE sr.import_id=?1 AND NOT EXISTS (
                SELECT 1 FROM candidate_dispositions cd
                WHERE cd.candidate_id=ec.id AND cd.is_current=1
             )",
                rusqlite::params![import_id],
                |row| Ok(row.get::<_, i64>(0)? as usize),
            )
            .expect("rec undisposed");
        let d_rec = t_rec.elapsed();
        println!(
            "[timing] 6. Reconciliation checks: {:.3}s",
            d_rec.as_secs_f64()
        );

        let t_commit = Instant::now();
        tx.commit().expect("commit");
        let d_commit = t_commit.elapsed();
        println!(
            "[timing] 7. Commit transaction: {:.3}s",
            d_commit.as_secs_f64()
        );

        let t_deriv = Instant::now();
        let tx_d = db.transaction().expect("tx_d");
        crate::imports::derive_imported_contacts(&tx_d, std::slice::from_ref(&import_id))
            .expect("derivation execute");
        tx_d.commit().expect("tx_d commit");
        let d_deriv = t_deriv.elapsed();
        println!(
            "[timing] 8. Post-import derivation: {:.3}s",
            d_deriv.as_secs_f64()
        );

        let total_file_time =
            d_parse + d_src + d_loop + d_flush + d_recompute + d_rec + d_commit + d_deriv;
        println!("-------------------------------------------------------");
        println!(
            "[benchmark] TOTAL END-TO-END TIME FOR {}: {:.3}s",
            relative,
            total_file_time.as_secs_f64()
        );
        println!("=======================================================\n");
    }
}

#[test]
#[ignore = "full-corpus parse; run with `bun run test:corpus`"]
fn corpus_extraction_matches_committed_baseline() {
    let root = corpus_root();
    if !root.is_dir() {
        panic!("compatibility corpus missing at {}", root.display());
    }
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let selected = std::env::var("CORPUS_ONLY").ok().map(|value| {
        value
            .split('|')
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    });
    if let Some(selected) = &selected {
        files.retain(|(relative, _)| selected.contains(relative));
        assert_eq!(
            files.len(),
            selected.len(),
            "every CORPUS_ONLY path must exist"
        );
    } else {
        assert!(
            files.len() > 200,
            "expected the full compatibility corpus, found {} files",
            files.len()
        );
    }

    let mut actual = BTreeMap::new();
    for (relative, path) in &files {
        let bytes = std::fs::read(path).expect("corpus file should be readable");
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_owned();
        let file = ImportFile {
            path: None,
            name,
            relative_path: relative.clone(),
            bytes,
        };

        let parsed = parse_file(&file)
            .unwrap_or_else(|error| panic!("{relative} should parse, got: {error}"));
        actual.insert(relative.clone(), parsed.contacts.len());
        assert!(
            is_blacklist_file(&file) || parsed.contacts.is_empty() || parsed.parsed_rows > 0,
            "{relative} produced contacts without counting any row"
        );
    }

    if std::env::var("UPDATE_CORPUS_BASELINE").is_ok() {
        assert!(
            selected.is_none(),
            "CORPUS_ONLY cannot be combined with UPDATE_CORPUS_BASELINE"
        );
        let mut rendered = String::from(
            "# Addresses extracted per corpus file. Regenerate with UPDATE_CORPUS_BASELINE=1.\n",
        );
        for (name, count) in &actual {
            rendered.push_str(&format!("{name}\t{count}\n"));
        }
        std::fs::create_dir_all(baseline_path().parent().expect("fixtures directory"))
            .expect("fixtures directory should be creatable");
        std::fs::write(baseline_path(), rendered).expect("baseline should be writable");
        println!(
            "[corpus] baseline updated: {} files, {} addresses",
            actual.len(),
            actual.values().sum::<usize>()
        );
        return;
    }

    let mut expected = read_baseline();
    if let Some(selected) = &selected {
        expected.retain(|relative, _| selected.contains(relative));
    }
    assert!(
        !expected.is_empty(),
        "corpus baseline is missing; regenerate it with UPDATE_CORPUS_BASELINE=1"
    );

    let mut regressions = Vec::new();
    let mut improvements = Vec::new();
    for (name, expected_count) in &expected {
        match actual.get(name) {
            None => regressions.push(format!("{name}: file no longer parsed")),
            Some(count) if count < expected_count => {
                regressions.push(format!("{name}: {expected_count} -> {count}"))
            }
            Some(count) if count > expected_count => {
                improvements.push(format!("{name}: {expected_count} -> {count}"))
            }
            Some(_) => {}
        }
    }
    for name in actual.keys() {
        if !expected.contains_key(name) {
            improvements.push(format!("{name}: new file"));
        }
    }

    assert!(
        regressions.is_empty(),
        "corpus extraction regressed for {} file(s):\n{}",
        regressions.len(),
        regressions.join("\n")
    );
    assert!(
        improvements.is_empty(),
        "corpus extraction improved for {} file(s); review and regenerate the baseline with UPDATE_CORPUS_BASELINE=1:\n{}",
        improvements.len(),
        improvements.join("\n")
    );
}

#[test]
fn every_legacy_corpus_file_decodes_as_western_european() {
    let root = corpus_root();
    if !root.is_dir() {
        eprintln!("skipped: private corpus not found at {}", root.display());
        return;
    }
    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();
    let mut checked = 0;
    for (relative, path) in &files {
        if SPREADSHEET_EXTENSIONS
            .iter()
            .any(|extension| relative.to_lowercase().ends_with(extension))
        {
            continue;
        }
        let bytes = std::fs::read(path).expect("corpus file should be readable");
        if std::str::from_utf8(&bytes).is_ok() {
            continue;
        }
        checked += 1;
        let (text, warning) = crate::parsing::decode_text(&bytes);
        let warning = warning.unwrap_or_default();
        assert!(
            warning.contains("windows-1252")
                || warning.contains("ISO-8859-15")
                || warning.contains("ISO-8859-2")
                || warning.contains("macintosh"),
            "{relative} decoded with an implausible encoding: {warning}"
        );
        assert!(
            !text.contains('\u{fffd}'),
            "{relative} produced replacement characters"
        );
    }
    assert!(
        checked >= 30,
        "expected the legacy non-UTF-8 files to still be present, checked {checked}"
    );
}
