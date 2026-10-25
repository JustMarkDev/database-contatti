//! Release-mode app benchmarks for curriculum / capacity reporting.
//!
//! Run (non-AI only):
//!   bun run test:performance
//!
//! Methodology lives in docs/performance.md. Times depend on hardware and build.

use super::*;
use crate::{contacts::*, imports::*, schema::*};
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::Instant;

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/email")
}

fn print_host_banner() {
    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;
    println!("[performance] host os={os} arch={arch} profile=release");
}

fn throughput(count: usize, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    count as f64 / seconds
}

fn mb_per_sec(bytes: usize, seconds: f64) -> f64 {
    if seconds <= 0.0 {
        return 0.0;
    }
    (bytes as f64 / (1024.0 * 1024.0)) / seconds
}

fn seed_valid_contacts(db: &mut Connection, count: usize) -> Result<(), String> {
    suspend_contact_triggers(db)?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO contacts (
                    id, email, email_local_part, email_domain, email_tld, status, validation_notes,
                    source_file, category, subcategory, contact_kind, paese, is_pec, comune,
                    provincia, regione, website, source_url, classification_method,
                    classification_confidence, classification_evidence, needs_review,
                    classification_state
                 ) VALUES (
                    ?1, ?2, ?3, ?4, '.test', 'valid', '[]',
                    'bench.txt', 'Varie', 'Generico', 'istituzionale', 'Italia', 0,
                    ?5, 'TO', 'Piemonte', NULL, NULL, 'bench',
                    1.0, '[]', 0, 'classified'
                 )",
            )
            .map_err(|e| e.to_string())?;
        for index in 0..count {
            let email = format!("utente{index}@example.test");
            let comune = if index % 10 == 0 { "Torino" } else { "Milano" };
            stmt.execute(rusqlite::params![
                format!("bench-{index}"),
                email,
                format!("utente{index}"),
                "example",
                comune,
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    resume_contact_triggers(db)?;
    Ok(())
}

#[test]
#[ignore = "app performance suite; run with `bun run test:performance`"]
fn app_performance_import_parse_and_persist() {
    print_host_banner();
    let relative = "Comuni/Comuni 7.txt";
    let path = corpus_root().join(relative);
    let bytes = std::fs::read(&path).expect("large benchmark fixture");
    assert!(
        bytes.len() >= 20_000_000,
        "benchmark fixture is no longer large"
    );
    let byte_len = bytes.len();
    let file = ImportFile {
        path: None,
        name: "Comuni 7.txt".to_owned(),
        relative_path: relative.to_owned(),
        bytes,
    };

    let started = Instant::now();
    let mut parsed = parse_file(&file).expect("large benchmark fixture should parse");
    let parse_secs = started.elapsed().as_secs_f64();
    assert_eq!(parsed.parsed_rows, 107_519);
    assert_eq!(parsed.contacts.len(), 177_087);
    println!(
        "[performance] parse file={relative} bytes={byte_len} rows={} candidates={} \
         parse_ms={:.3} candidates_per_s={:.0} mb_per_s={:.2}",
        parsed.parsed_rows,
        parsed.contacts.len(),
        parse_secs * 1_000.0,
        throughput(parsed.contacts.len(), parse_secs),
        mb_per_sec(byte_len, parse_secs),
    );

    for contact in &mut parsed.contacts {
        contact.status = EmailStatus::Valid;
    }
    const TIMED_OCCURRENCES: usize = 20_000;
    let mut db = Connection::open_in_memory().expect("benchmark database");
    setup_schema(&db).expect("benchmark schema");
    let tx = db.transaction().expect("benchmark transaction");
    let mut cache = ContactImportCache::default();
    suspend_contact_triggers(&tx).expect("suspend fts");
    let started = Instant::now();
    for contact in parsed.contacts.iter().take(TIMED_OCCURRENCES) {
        save_contact_for_import(
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
    flush_contact_import_cache(&tx, &mut cache, "2026-01-01T00:00:00Z")
        .expect("flush benchmark contacts");
    resume_contact_triggers(&tx).expect("resume fts");
    tx.commit().expect("commit benchmark contacts");
    let persist_secs = started.elapsed().as_secs_f64();
    let canonical_contacts: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .expect("count benchmark contacts");
    println!(
        "[performance] persist timed_occurrences={TIMED_OCCURRENCES} \
         canonical_contacts={canonical_contacts} persist_ms={:.3} occurrences_per_s={:.0}",
        persist_secs * 1_000.0,
        throughput(TIMED_OCCURRENCES, persist_secs),
    );
}

#[test]
#[ignore = "app performance suite; run with `bun run test:performance`"]
fn app_performance_import_parse_xlsx() {
    print_host_banner();
    let relative = "Comuni/Comuni 4.xlsx";
    let path = corpus_root().join(relative);
    let bytes = std::fs::read(&path).expect("large xlsx benchmark fixture");
    assert!(
        bytes.len() >= 4_000_000,
        "xlsx benchmark fixture is no longer large"
    );
    let byte_len = bytes.len();
    let file = ImportFile {
        path: None,
        name: "Comuni 4.xlsx".to_owned(),
        relative_path: relative.to_owned(),
        bytes,
    };

    let started = Instant::now();
    let parsed = parse_file(&file).expect("large xlsx benchmark fixture should parse");
    let parse_secs = started.elapsed().as_secs_f64();
    assert_eq!(parsed.parsed_rows, 107_519);
    assert_eq!(parsed.contacts.len(), 177_086);
    println!(
        "[performance] parse file={relative} bytes={byte_len} rows={} candidates={} \
         parse_ms={:.3} candidates_per_s={:.0} mb_per_s={:.2}",
        parsed.parsed_rows,
        parsed.contacts.len(),
        parse_secs * 1_000.0,
        throughput(parsed.contacts.len(), parse_secs),
        mb_per_sec(byte_len, parse_secs),
    );
}

#[test]
#[ignore = "app performance suite; run with `bun run test:performance`"]
fn app_performance_list_search_export_scale() {
    print_host_banner();
    const CONTACT_COUNT: usize = 100_000;
    let mut db = Connection::open_in_memory().expect("benchmark database");
    setup_schema(&db).expect("benchmark schema");

    let seed_started = Instant::now();
    seed_valid_contacts(&mut db, CONTACT_COUNT).expect("seed contacts");
    let seed_secs = seed_started.elapsed().as_secs_f64();
    println!(
        "[performance] seed contacts={CONTACT_COUNT} seed_ms={:.3} contacts_per_s={:.0}",
        seed_secs * 1_000.0,
        throughput(CONTACT_COUNT, seed_secs),
    );

    let filters = ListFilters::default();
    let list_started = Instant::now();
    let page = load_contacts(&db, &filters, 100, 0).expect("list first page");
    let list_ms = list_started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(page.contacts.len(), 100);
    assert_eq!(page.total, CONTACT_COUNT);
    println!(
        "[performance] list contacts={CONTACT_COUNT} page=100 include_total=true list_ms={list_ms:.3}"
    );

    let skip_started = Instant::now();
    let skip_page = load_contacts_page(&db, &filters, 100, 50_000, false).expect("list mid page");
    let skip_ms = skip_started.elapsed().as_secs_f64() * 1_000.0;
    assert_eq!(skip_page.contacts.len(), 100);
    println!(
        "[performance] list contacts={CONTACT_COUNT} page=100 offset=50000 include_total=false list_ms={skip_ms:.3}"
    );

    for column in ["comune", "score"] {
        let sorted = ListFilters {
            sort: Some(crate::ListSort {
                column: column.to_owned(),
                direction: "desc".to_owned(),
            }),
            ..ListFilters::default()
        };
        let sort_started = Instant::now();
        let sorted_page = load_contacts(&db, &sorted, 100, 0).expect("sorted first page");
        let sort_ms = sort_started.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(sorted_page.contacts.len(), 100);
        println!(
            "[performance] list contacts={CONTACT_COUNT} page=100 sort={column} sort_ms={sort_ms:.3}"
        );
    }

    let search = ListFilters {
        query: "torino".to_owned(),
        ..ListFilters::default()
    };
    let search_started = Instant::now();
    let hits = load_contacts(&db, &search, 100, 0).expect("fts search");
    let search_ms = search_started.elapsed().as_secs_f64() * 1_000.0;
    assert!(hits.total > 0, "expected Torino hits");
    assert!(hits.contacts.len() <= 100);
    println!(
        "[performance] search query=torino contacts={CONTACT_COUNT} hits={} page={} search_ms={search_ms:.3}",
        hits.total,
        hits.contacts.len(),
    );

    let facet_started = Instant::now();
    let facets = load_filter_options(&db).expect("facets");
    let facet_ms = facet_started.elapsed().as_secs_f64() * 1_000.0;
    assert!(!facets.category.is_empty());
    println!("[performance] facets contacts={CONTACT_COUNT} facet_ms={facet_ms:.3}");

    let export_path = std::env::temp_dir().join(format!(
        "database-contatti-bench-export-{}.csv",
        uuid::Uuid::new_v4()
    ));
    let export_started = Instant::now();
    let exported =
        export_contacts_to_path(&db, &ListFilters::default(), &export_path).expect("export");
    let export_secs = export_started.elapsed().as_secs_f64();
    let export_bytes = std::fs::metadata(&export_path)
        .map(|meta| meta.len() as usize)
        .unwrap_or(0);
    let _ = std::fs::remove_file(&export_path);
    assert_eq!(exported, CONTACT_COUNT);
    println!(
        "[performance] export contacts={exported} bytes={export_bytes} \
         export_ms={:.3} contacts_per_s={:.0} mb_per_s={:.2}",
        export_secs * 1_000.0,
        throughput(exported, export_secs),
        mb_per_sec(export_bytes, export_secs),
    );
}

/// Migrates a copy of a real database and checks the materialized flag against the
/// pre-v54 per-row predicate. Run with
/// `DBC_MIGRATION_DB=/path/copy.sqlite3 cargo test --release real_database_migration -- --ignored --nocapture`.
#[test]
#[ignore = "needs DBC_MIGRATION_DB pointing at a database copy"]
fn real_database_migration_matches_previous_exportable_predicate() {
    let path = std::env::var("DBC_MIGRATION_DB").expect("DBC_MIGRATION_DB");
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .unwrap();
    let previous = "contacts.status='valid' AND coalesce(contacts.suppression_status,'active')='active' AND NOT (
            lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) IN
              ('noreply','no-reply','no_reply','no.reply','donotreply','do-not-reply','do_not_reply','do.not.reply',
               'mailer-daemon','mailer_daemon','mailer.daemon','mailerdaemon','bounce','bounces','autoresponder','autoreply','auto-reply','auto_reply','auto.reply')
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'noreply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'no-reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'no_reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'no.reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'donotreply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'do-not-reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'do_not_reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'do.not.reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'mailer-daemon[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'mailer_daemon[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'mailer.daemon[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'mailerdaemon[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'bounce[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'bounces[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'autoresponder[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'autoreply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'auto-reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'auto_reply[^a-z]*'
            OR lower(substr(contacts.email,1,instr(contacts.email,'@')-1)) GLOB 'auto.reply[^a-z]*'
        ) AND NOT EXISTS (SELECT 1 FROM blacklist b WHERE b.normalized_email=contacts.email COLLATE NOCASE) AND NOT EXISTS (SELECT 1 FROM suppressions s WHERE s.comparison_key=COALESCE(contacts.email_comparison_key,lower(contacts.email)) COLLATE NOCASE AND s.lifted_at IS NULL)";
    let before: i64 = db
        .query_row(
            &format!("SELECT COUNT(*) FROM contacts WHERE {previous}"),
            [],
            |row| row.get(0),
        )
        .unwrap();
    let started = Instant::now();
    migrate_database(&mut db, Some(std::path::Path::new(&path)), false).unwrap();
    println!(
        "[migration] v54 on real copy: {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let after: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM contacts WHERE is_exportable=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mismatched: i64 = db
        .query_row(
            &format!("SELECT COUNT(*) FROM contacts WHERE is_exportable IS NOT ({previous})"),
            [],
            |row| row.get(0),
        )
        .unwrap();
    println!("[migration] exportable before={before} after={after} mismatched={mismatched}");
    assert_eq!(mismatched, 0);
    for (label, offset, total) in [("page1+total", 0, true), ("deep page", 150_000, false)] {
        let started = Instant::now();
        let page = load_contacts_page(&db, &ListFilters::default(), 100, offset, total).unwrap();
        println!(
            "[migration] {label}: {:.1} ms (total={})",
            started.elapsed().as_secs_f64() * 1000.0,
            page.total
        );
    }
    let started = Instant::now();
    crate::imports::load_filter_options(&db).unwrap();
    println!(
        "[migration] facets: {:.1} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
}
