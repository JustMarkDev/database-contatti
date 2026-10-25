use super::*;
use crate::{
    ai_runs::*, blacklist::*, contacts::*, discovery::*, imports::*, parsing::*, schema::*,
};
use rusqlite::{Connection, params};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

fn import_contact(email: &str) -> ImportContact {
    let (email_local_part, email_domain, email_tld) = email_parts(email);
    ImportContact {
        raw_email: email.to_owned(),
        normalized_email: email.to_owned(),
        comparison_key: email.to_lowercase(),
        normalization_method: "literal".to_owned(),
        email_local_part,
        email_domain,
        email_tld,
        status: EmailStatus::DomainUnverified,
        validation_notes: Vec::new(),
        source_file: std::sync::Arc::from("test.txt"),
        category: "Varie".to_owned(),
        subcategory: None,
        contact_kind: "istituzionale".to_owned(),
        paese: "Italia".to_owned(),
        is_pec: false,
        comune: None,
        provincia: None,
        regione: None,
        macroarea: None,
        website: None,
        source_url: None,
        source_sheet: None,
        source_row: 1,
        raw_context: std::sync::Arc::from(email),
        classification_method: "test".to_owned(),
        classification_confidence: 1.0,
        classification_evidence: Vec::new(),
    }
}

fn test_contact() -> ContactRecord {
    ContactRecord {
        id: "contact-1".to_owned(),
        email: "protocollo@comune.moncalieri.to.it".to_owned(),
        email_local_part: "protocollo".to_owned(),
        email_domain: "comune.moncalieri.to".to_owned(),
        email_tld: ".it".to_owned(),
        status: EmailStatus::Valid,
        validation_notes: Vec::new(),
        source_file: "test.txt".to_owned(),
        category: "Varie".to_owned(),
        subcategory: None,
        contact_kind: "istituzionale".to_owned(),
        paese: "Italia".to_owned(),
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
    }
}

fn migration_test_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "database-contatti-{name}-{}.sqlite3",
        uuid::Uuid::new_v4()
    ))
}

#[test]
fn imported_source_file_match_is_case_sensitive_and_includes_extension() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO imports(
            id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at
         ) VALUES ('import-1','Scuole1.xls','Scuole1.xls','hash','completed',1,1,'now')",
        [],
    )
    .unwrap();

    assert!(source_file_was_imported(&db, "Scuole1.xls").unwrap());
    assert!(!source_file_was_imported(&db, "scuole1.xls").unwrap());
    assert!(!source_file_was_imported(&db, "Scuole1.xlsx").unwrap());
    assert!(!source_file_was_imported(&db, "Scuole1.XLS").unwrap());
}

#[test]
fn source_manifest_uses_sha256_and_exact_relative_path() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO imports(
            id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at,
            content_sha256,hash_algorithm
         ) VALUES ('manifest-1','same.csv','Folder A/same.csv','legacy','completed',1,1,'now',?1,'sha256')",
        params![sha256_hex(b"original bytes")],
    )
    .unwrap();
    assert!(
        source_file_matches_import(&db, "Folder A/same.csv", &sha256_hex(b"original bytes"))
            .unwrap()
    );
    assert!(
        !source_file_matches_import(&db, "Folder B/same.csv", &sha256_hex(b"original bytes"))
            .unwrap()
    );
    assert!(
        !source_file_matches_import(&db, "Folder A/same.csv", &sha256_hex(b"changed bytes"))
            .unwrap()
    );
}

#[test]
fn standalone_file_selection_retains_corpus_folder_context() {
    assert_eq!(
        standalone_relative_path(
            &PathBuf::from("/archive/data/email/Scuole/istituti.csv"),
            "istituti.csv",
        ),
        "Scuole/istituti.csv"
    );
    assert_eq!(
        standalone_relative_path(
            &PathBuf::from("/archive/Parrocchie/contatti.csv"),
            "contatti.csv",
        ),
        "Parrocchie/contatti.csv"
    );
}

#[test]
fn source_file_filter_uses_a_base_name_without_extension() {
    assert_eq!(contacts::source_file_base_name("Alpini 1.xlsx"), "Alpini 1");
    assert_eq!(
        contacts::source_file_base_name("folder\\Alpini 1.csv"),
        "Alpini 1"
    );
    assert_eq!(contacts::source_file_base_name("Alpini 1"), "Alpini 1");
}

#[test]
fn parses_tab_separated_txt_with_metadata() {
    let file = ImportFile {
            path: None,
            name: "Comuni 6.txt".to_owned(),
            relative_path: "Comuni/Comuni 6.txt".to_owned(),
            bytes: b"cod_amm\tDescrizione\tRegione\tProvincia\tComune\tMail\tTipo Mail\nabam_043\tACCADEMIA DI BELLE ARTI\tMarche\tMC\tMacerata\tABAMC@PEC.IT\tpec\n".to_vec(),
        };

    let parsed = parse_file(&file).expect("tab-separated txt should parse");
    assert_eq!(parsed.contacts.len(), 1);
    let contact = &parsed.contacts[0];
    assert_eq!(contact.normalized_email, "ABAMC@pec.it");
    assert_eq!(contact.comparison_key, "abamc@pec.it");
    assert_eq!(contact.regione.as_deref(), Some("Marche"));
    assert_eq!(contact.provincia.as_deref(), Some("Macerata"));
    assert_eq!(contact.comune.as_deref(), Some("Macerata"));
    assert!(contact.is_pec);
}

#[test]
fn normalizes_extracted_province_name_and_expands_abbreviation() {
    assert_eq!(normalize_extracted_province("TORINO".to_owned()), "Torino");
    assert_eq!(normalize_extracted_province("bg".to_owned()), "Bergamo");
}

#[test]
fn normalizes_imported_geographic_fields() {
    let file = ImportFile {
        path: None,
        name: "scuole.csv".to_owned(),
        relative_path: "Scuole/scuole.csv".to_owned(),
        bytes: b"Regione;Provincia;Comune;Email\nEMILIA ROMAGNA;BO;SAN LAZZARO DI SAVENA;info@example.it\n"
            .to_vec(),
    };

    let parsed = parse_file(&file).expect("parse");
    let contact = &parsed.contacts[0];
    assert_eq!(contact.regione.as_deref(), Some("Emilia Romagna"));
    assert_eq!(contact.provincia.as_deref(), Some("Bologna"));
    assert_eq!(contact.comune.as_deref(), Some("San Lazzaro di Savena"));
}

#[test]
fn infers_province_and_region_from_filename_when_columns_are_missing() {
    let file = ImportFile {
        path: None,
        name: "Comuni Milano.xlsx".to_owned(),
        relative_path: "Comuni/Comuni Milano.xlsx".to_owned(),
        bytes: Vec::new(),
    };
    let rows = vec![
        vec!["Email".to_owned(), "Denominazione".to_owned()],
        vec!["info@example.it".to_owned(), "Comune di esempio".to_owned()],
    ];

    let contacts = parse_rows(&file, None, &rows);
    assert_eq!(contacts[0].provincia.as_deref(), Some("Milano"));
    assert_eq!(contacts[0].regione.as_deref(), Some("Lombardia"));
}

#[test]
fn classifies_mountain_communities_before_generic_municipalities() {
    let file = ImportFile {
        path: None,
        name: "Comunità montane.xlsx".to_owned(),
        relative_path: "Varie/Comunità montane.xlsx".to_owned(),
        bytes: Vec::new(),
    };
    let (category, subcategory, _, _, _) = deterministic_classification(&file, &BTreeMap::new());
    assert_eq!(category, "Pubblica amministrazione");
    assert_eq!(subcategory.as_deref(), Some("Comunità montana"));
    assert!(taxonomy_valid(
        "Pubblica amministrazione",
        Some("Comunità montana")
    ));
}

#[test]
fn classifies_personal_contacts_as_miscellaneous() {
    let file = ImportFile {
        path: None,
        name: "Contatti Personali.xlsx".to_owned(),
        relative_path: "Varie/Contatti Personali.xlsx".to_owned(),
        bytes: Vec::new(),
    };
    let (category, subcategory, _, _, _) = deterministic_classification(&file, &BTreeMap::new());
    assert_eq!(category, "Varie");
    assert_eq!(subcategory.as_deref(), Some("Contatto personale"));
    assert!(taxonomy_valid("Varie", Some("Contatto personale")));
}

#[test]
fn derives_country_from_filename_and_defaults_to_italy() {
    let rows = vec![vec!["info@example.ch".to_owned()]];
    let swiss = ImportFile {
        path: None,
        name: "Scuole Svizzera.txt".to_owned(),
        relative_path: "Scuole/Scuole Svizzera.txt".to_owned(),
        bytes: Vec::new(),
    };
    let italian = ImportFile {
        path: None,
        name: "Scuole Lombardia.txt".to_owned(),
        relative_path: "Scuole/Scuole Lombardia.txt".to_owned(),
        bytes: Vec::new(),
    };
    assert_eq!(parse_rows(&swiss, None, &rows)[0].paese, "Svizzera");
    assert_eq!(parse_rows(&italian, None, &rows)[0].paese, "Italia");
}

#[test]
fn keeps_religious_orders_and_associations_as_explicit_subcategories() {
    for (category, subcategory) in [
        ("Enti religiosi", "Frati Minori"),
        ("Enti religiosi", "Domenicani"),
        ("Enti religiosi", "Agostiniani"),
        ("Enti religiosi", "Salesiani"),
        ("Associazioni", "Alpini"),
        ("Associazioni", "CAI"),
        ("Associazioni", "NOI"),
        ("Associazioni", "Lions"),
        ("Associazioni", "Rotary"),
    ] {
        assert!(taxonomy_valid(category, Some(subcategory)));
    }
}

#[test]
fn groups_multiple_addresses_by_unique_domain() {
    let contacts = vec![
        import_contact("one@example.it"),
        import_contact("two@example.it"),
        import_contact("three@example.it"),
        import_contact("four@other.it"),
    ];
    let (immediate, groups) = group_contacts_for_validation(&contacts, &HashSet::new());
    assert!(immediate.is_empty());
    assert_eq!(groups.len(), 2);
    assert_eq!(groups["example.it"], vec![0, 1, 2]);
    assert_eq!(groups["other.it"], vec![3]);
}

#[test]
fn invalid_and_blacklisted_addresses_do_not_enter_dns_groups() {
    let contacts = vec![
        import_contact("not-an-email"),
        import_contact("blocked@example.it"),
        import_contact("valid@example.it"),
    ];
    let blacklist = HashSet::from(["blocked@example.it".to_owned()]);
    let (immediate, groups) = group_contacts_for_validation(&contacts, &blacklist);
    assert_eq!(immediate.len(), 2);
    assert_eq!(immediate[0].1, EmailStatus::InvalidSyntax);
    assert_eq!(immediate[1].1, EmailStatus::Blacklisted);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups["example.it"], vec![2]);
}

#[test]
fn automated_mailboxes_are_suppressed_before_dns() {
    for address in [
        "noreply@example.test",
        "noreply_crm@example.test",
        "no-reply.alerts@example.test",
        "no_reply@example.test",
        "no.reply@example.test",
        "donotreply@example.test",
        "do-not-reply_1@example.test",
        "do.not.reply@example.test",
        "mailer-daemon@example.test",
        "mailer_daemon.mailer@example.test",
        "mailerdaemon@example.test",
        "bounce@example.test",
        "bounces_2024@example.test",
        "autoresponder@example.test",
        "autoreply@example.test",
        "auto_reply-system@example.test",
    ] {
        assert_eq!(
            non_contact_mailbox_reason(address),
            Some("non_contact_mailbox"),
            "{address} should be excluded from outreach"
        );
    }
    for address in [
        "noreplying@example.test",
        "notifications@example.test",
        "postmaster@example.test",
        "abuse@example.test",
        "privacy@example.test",
        "webmaster@example.test",
    ] {
        assert_eq!(
            non_contact_mailbox_reason(address),
            None,
            "{address} remains a potentially useful contact"
        );
    }

    let contacts = vec![import_contact("noreply_crm@example.test")];
    let (immediate, groups) = group_contacts_for_validation(&contacts, &HashSet::new());
    assert_eq!(groups.len(), 0);
    assert_eq!(immediate.len(), 1);
    assert_eq!(immediate[0].1, EmailStatus::Blacklisted);
    assert!(immediate[0].2[0].contains("non_contact_mailbox"));
}

#[test]
fn applies_one_domain_result_to_every_address_in_the_group() {
    let mut contacts = vec![
        import_contact("one@example.it"),
        import_contact("two@example.it"),
    ];
    let (_, groups) = group_contacts_for_validation(&contacts, &HashSet::new());
    let results = HashMap::from([(
        "example.it".to_owned(),
        DomainValidation {
            status: EmailStatus::Valid,
            notes: vec!["risultato condiviso".to_owned()],
            observed_ttl_seconds: None,
            dns_rcode: None,
            mx_hosts: Vec::new(),
            dns_checked_at: None,
            dns_expires_at: None,
        },
    )]);
    apply_domain_results(&mut contacts, &groups, &results);
    assert!(
        contacts
            .iter()
            .all(|contact| contact.status == EmailStatus::Valid)
    );
    assert!(
        contacts
            .iter()
            .all(|contact| contact.validation_notes[0] == "risultato condiviso")
    );
}

#[test]
fn dns_cache_ttl_uses_observed_values_with_conservative_bounds() {
    assert_eq!(
        effective_cache_ttl_seconds(EmailStatus::Valid, Some(42)),
        300
    );
    assert_eq!(
        effective_cache_ttl_seconds(EmailStatus::Valid, Some(3_600)),
        3_600
    );
    assert_eq!(
        effective_cache_ttl_seconds(EmailStatus::Valid, Some(200_000)),
        86_400
    );
    assert_eq!(
        effective_cache_ttl_seconds(EmailStatus::DomainMissingMx, Some(120)),
        300
    );
    assert_eq!(
        effective_cache_ttl_seconds(EmailStatus::DomainMissingMx, Some(7_200)),
        7_200
    );
    assert_eq!(
        effective_cache_ttl_seconds(EmailStatus::DomainUnverified, Some(86_400)),
        900
    );
}

#[test]
fn annotates_role_disposable_placeholder_and_typo_risks() {
    assert!(address_risk_notes("info@mailinator.com").len() >= 2);
    assert!(
        address_risk_notes("person@example.com")
            .iter()
            .any(|note| note.contains("placeholder"))
    );
    assert!(
        address_risk_notes("person@gmial.com")
            .iter()
            .any(|note| note.contains("gmail.com"))
    );
}

#[test]
fn parses_semicolon_txt_with_standard_email() {
    let file = ImportFile {
            path: None,
            name: "scuole.txt".to_owned(),
            relative_path: "Scuole/scuole.txt".to_owned(),
            bytes: b"Denominazione;Comune;Provincia;Regione;Mail\nIstituto Test;Verona;VR;Veneto;segreteria@scuola.test.it\n".to_vec(),
        };

    let parsed = parse_file(&file).expect("semicolon txt should parse");
    assert_eq!(parsed.contacts.len(), 1);
    let contact = &parsed.contacts[0];
    assert_eq!(contact.normalized_email, "segreteria@scuola.test.it");
    assert_eq!(contact.comune.as_deref(), Some("Verona"));
    assert!(!contact.is_pec);
}

#[test]
fn recognizes_email_and_source_headers() {
    for (email_header, source_header) in [
        ("Email", "Sorgente"),
        ("Email", "Source"),
        ("Email/Telefono/Skype/UID", "Source"),
        ("Email/Telefono/Skype/UID", "URL/Casella di posta"),
    ] {
        let file = ImportFile {
            path: None,
            name: "Alpini.xlsx".to_owned(),
            relative_path: "Associazioni/Alpini.xlsx".to_owned(),
            bytes: Vec::new(),
        };
        let rows = vec![
            vec![email_header.to_owned(), source_header.to_owned()],
            vec![
                "sezione@example.it".to_owned(),
                "https://www.example.it/alpini".to_owned(),
            ],
        ];

        let contacts = parse_rows(&file, Some("Foglio1".to_owned()), &rows);

        assert_eq!(contacts.len(), 1);
        assert_eq!(
            contacts[0].source_url.as_deref(),
            Some("https://www.example.it/alpini")
        );
    }
}

#[test]
fn recognizes_postal_email_and_compact_ministry_headers() {
    let postal = vec![
        vec![
            "Provincia".to_owned(),
            "Comune".to_owned(),
            "Indirizzo posta elettronica".to_owned(),
            "Indirizzo posta elettronica certificata".to_owned(),
        ],
        vec![
            "TO".to_owned(),
            "MONCALIERI".to_owned(),
            "info@example.it".to_owned(),
            "protocollo@pec.example.it".to_owned(),
        ],
    ];
    let compact = vec![
        vec![
            "REGIONE".to_owned(),
            "PROVINCIA".to_owned(),
            "DESCRIZIONECOMUNE".to_owned(),
            "INDIRIZZOEMAILSCUOLA".to_owned(),
            "INDIRIZZOPECSCUOLA".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "MONCALIERI".to_owned(),
            "segreteria@example.it".to_owned(),
            "scuola@pec.example.it".to_owned(),
        ],
    ];

    for rows in [postal, compact] {
        let headers = detect_headers(&rows).expect("headers");
        assert_eq!(headers.0, 0);
        assert!(headers.1.len() >= 4);
    }
}

#[test]
fn header_scoring_prefers_real_headers_over_titles_and_data() {
    let rows = vec![
        vec![
            "ELENCO INDIRIZZI EMAIL SCUOLE".to_owned(),
            "Fonte: OPEN DATA Ministero dell'Istruzione".to_owned(),
        ],
        vec![
            "protocollo@comune.example.it".to_owned(),
            "Via Provinciale 1".to_owned(),
        ],
        vec![
            "REGIONE".to_owned(),
            "PROVINCIA".to_owned(),
            "COMUNE".to_owned(),
            "EMAIL".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "MONCALIERI".to_owned(),
            "info@example.it".to_owned(),
        ],
    ];

    assert_eq!(detect_headers(&rows).map(|headers| headers.0), Some(2));
}

#[test]
fn aligned_school_layout_is_not_rewritten() {
    let file = ImportFile {
        path: None,
        name: "Scuole-Paritarie-2023-24.xlsx".to_owned(),
        relative_path: "indirizzi mail/Scuole-Paritarie-2023-24.xlsx".to_owned(),
        bytes: Vec::new(),
    };
    let rows = vec![
        vec![
            "REGIONE".to_owned(),
            "PROVINCIA".to_owned(),
            "DENOMINAZIONE SCUOLA".to_owned(),
            "INDIRIZZO SCUOLA".to_owned(),
            "DESCRIZIONE COMUNE".to_owned(),
            "TIPOLOGIA SCUOLA".to_owned(),
            "INDIRIZZO EMAIL SCUOLA".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "SCUOLA TEST".to_owned(),
            "VIA ROMA 1".to_owned(),
            "MONCALIERI".to_owned(),
            "SCUOLA PRIMARIA NON STATALE".to_owned(),
            "info1@example.it".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "SCUOLA TEST 2".to_owned(),
            "VIA MILANO 2".to_owned(),
            "RIVOLI".to_owned(),
            "SCUOLA PRIMARIA NON STATALE".to_owned(),
            "info2@example.it".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "SCUOLA TEST 3".to_owned(),
            "CORSO FRANCIA 3".to_owned(),
            "COLLEGNO".to_owned(),
            "SCUOLA PRIMARIA NON STATALE".to_owned(),
            "info3@example.it".to_owned(),
        ],
    ];

    let parsed = parse_rows_with_diagnostics(&file, Some("Scuole".to_owned()), &rows);
    assert!(parsed.warnings.is_empty());
    assert_eq!(parsed.contacts[0].comune.as_deref(), Some("Moncalieri"));
}

#[test]
fn repairs_shifted_school_layout_and_warns() {
    let file = ImportFile {
        path: None,
        name: "Scuole-Paritarie-Aosta-Trento-e-Bolzano-2023-24.xlsx".to_owned(),
        relative_path: "indirizzi mail/Scuole-Paritarie-Aosta-Trento-e-Bolzano-2023-24.xlsx"
            .to_owned(),
        bytes: Vec::new(),
    };
    let header = vec![
        "REGIONE",
        "PROVINCIA",
        "DENOMINAZIONESCUOLA",
        "INDIRIZZOSCUOLA",
        "DESCRIZIONECOMUNE",
        "TIPOLOGIA SCUOLA",
        "INDIRIZZOEMAILSCUOLA",
        "INDIRIZZOPECSCUOLA",
        "SITOWEBSCUOLA",
        "",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let make_row = |code: &str, school: &str, address: &str, comune: &str, email: &str| {
        vec![
            "TRENTINO-ALTO ADIGE".to_owned(),
            "BOLZANO".to_owned(),
            code.to_owned(),
            school.to_owned(),
            address.to_owned(),
            comune.to_owned(),
            "SCUOLA PRIMARIA NON STATALE".to_owned(),
            email.to_owned(),
            "Non Disponibile".to_owned(),
            "www.example.it".to_owned(),
        ]
    };
    let rows = vec![
        header,
        make_row(
            "TB1E002003",
            "SCUOLA UNO",
            "VIA ROMA 1",
            "BRESSANONE",
            "uno@example.it",
        ),
        make_row(
            "TB1E002004",
            "SCUOLA DUE",
            "PIAZZA DUOMO 2",
            "BOLZANO",
            "due@example.it",
        ),
        make_row(
            "TB1E002005",
            "SCUOLA TRE",
            "CORSO ITALIA 3",
            "MERANO",
            "tre@example.it",
        ),
    ];

    let parsed = parse_rows_with_diagnostics(&file, Some("Scuole".to_owned()), &rows);

    assert_eq!(parsed.warnings.len(), 1);
    assert!(parsed.warnings[0].contains("riallineata automaticamente"));
    assert_eq!(parsed.contacts[0].comune.as_deref(), Some("Bressanone"));
    assert_eq!(parsed.contacts[0].subcategory.as_deref(), Some("Primaria"));
    assert_eq!(
        parsed.contacts[0].source_url.as_deref(),
        Some("www.example.it")
    );
}

#[test]
fn warns_without_repairing_ambiguous_school_layout() {
    let file = ImportFile {
        path: None,
        name: "scuole.xlsx".to_owned(),
        relative_path: "Scuole/scuole.xlsx".to_owned(),
        bytes: Vec::new(),
    };
    let rows = vec![
        vec![
            "REGIONE".to_owned(),
            "PROVINCIA".to_owned(),
            "DENOMINAZIONE SCUOLA".to_owned(),
            "TIPOLOGIA SCUOLA".to_owned(),
            "EMAIL SCUOLA".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "SCUOLA UNO".to_owned(),
            "DATO NON CLASSIFICABILE".to_owned(),
            "uno@example.it".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "SCUOLA DUE".to_owned(),
            "DATO NON CLASSIFICABILE".to_owned(),
            "due@example.it".to_owned(),
        ],
        vec![
            "PIEMONTE".to_owned(),
            "TORINO".to_owned(),
            "SCUOLA TRE".to_owned(),
            "DATO NON CLASSIFICABILE".to_owned(),
            "tre@example.it".to_owned(),
        ],
    ];

    let parsed = parse_rows_with_diagnostics(&file, Some("Scuole".to_owned()), &rows);
    assert_eq!(parsed.warnings.len(), 1);
    assert!(parsed.warnings[0].contains("verifica manuale"));
}

#[test]
fn detects_school_headers_after_introductory_rows() {
    let file = ImportFile {
        path: None,
        name: "Scuola-Paritarie-2023-24.xlsx".to_owned(),
        relative_path: "indirizzi mail/Scuola-Paritarie-2023-24.xlsx".to_owned(),
        bytes: Vec::new(),
    };
    let rows = vec![
        vec!["ELENCO SCUOLE PARITARIE 2023/24".to_owned()],
        vec![String::new()],
        vec!["Fonte: OPEN DATA Ministero dell'Istruzione".to_owned()],
        vec![String::new()],
        vec![String::new()],
        vec![
            "REGIONE".to_owned(),
            "PROVINCIA".to_owned(),
            "DENOMINAZIONE SCUOLA".to_owned(),
            "INDIRIZZO SCUOLA".to_owned(),
            "DESCRIZIONE COMUNE".to_owned(),
            "DESCRIZIONETIPOLOGIAGRADOISTRUZIONESCUOLA".to_owned(),
            "INDIRIZZO EMAIL SCUOLA".to_owned(),
        ],
        vec![
            "ABRUZZO".to_owned(),
            "CHIETI".to_owned(),
            "PITAGORA".to_owned(),
            "VIA F.P. TOSTI 38".to_owned(),
            "FRANCAVILLA AL MARE".to_owned(),
            "SCUOLA SEC. SECONDO GRADO NON STATALE".to_owned(),
            "istpitag@tin.it".to_owned(),
        ],
    ];

    let contacts = parse_rows(&file, Some("Scuole paritarie".to_owned()), &rows);

    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].regione.as_deref(), Some("Abruzzo"));
    assert_eq!(contacts[0].provincia.as_deref(), Some("Chieti"));
    assert_eq!(contacts[0].comune.as_deref(), Some("Francavilla al Mare"));
}

#[test]
fn detects_comma_pipe_and_ignores_delimiters_inside_quotes() {
    assert_eq!(
        detect_delimiter("\"Name, complete\",Email\nSchool,info@example.it\n", b';'),
        b','
    );
    assert_eq!(
        detect_delimiter("Name|Notes|Email\nSchool|\"a;b,c\"|info@example.it\n", b';'),
        b'|'
    );
}

#[test]
fn parses_windows_1252_csv() {
    let mut bytes = b"Comune;Email\nAl".to_vec();
    bytes.push(0xD3);
    bytes.extend_from_slice(b" dei Sardi;info@example.it\n");
    let file = ImportFile {
        path: None,
        name: "scuole.csv".to_owned(),
        relative_path: "Scuole/scuole.csv".to_owned(),
        bytes,
    };

    let parsed = parse_file(&file).expect("Windows-1252 CSV should parse");
    assert_eq!(parsed.contacts.len(), 1);
    assert_eq!(parsed.contacts[0].comune.as_deref(), Some("Alà dei Sardi"));
    assert_eq!(parsed.warnings.len(), 1);
}

#[test]
fn parses_obfuscated_email_from_free_text() {
    let file = ImportFile {
        path: None,
        name: "associazioni.txt".to_owned(),
        relative_path: "associazioni.txt".to_owned(),
        bytes: b"Contatti: info [at] associazione-example [dot] org".to_vec(),
    };

    let parsed = parse_file(&file).expect("free text should parse");
    assert_eq!(parsed.contacts.len(), 1);
    assert_eq!(
        parsed.contacts[0].normalized_email,
        "info@associazione-example.org"
    );
}

#[test]
fn parses_single_line_free_text_email_list_without_headers() {
    let file = ImportFile {
        path: None,
        name: "contatti media italiani.txt".to_owned(),
        relative_path: "Tutte Email/contatti media italiani.txt".to_owned(),
        bytes: b"uno@example.it; due@example.it; tre@example.it".to_vec(),
    };

    let parsed = parse_file(&file).expect("free text list should parse");
    assert_eq!(parsed.contacts.len(), 3);
    assert!(parsed.warnings.is_empty());
}

#[test]
fn recovers_addresses_with_padded_at_separator() {
    let file = ImportFile {
        path: None,
        name: "Scuole 64.txt".to_owned(),
        relative_path: "Scuole/Scuole 64.txt".to_owned(),
        bytes: "BOTF024508 @istruzione.it\nBOAA017001 @ istruzione.it\nnormale@example.test\n"
            .as_bytes()
            .to_vec(),
    };

    let parsed = parse_file(&file).expect("padded separator should parse");
    let emails = parsed
        .contacts
        .iter()
        .map(|contact| contact.normalized_email.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        emails,
        vec![
            "BOTF024508@istruzione.it",
            "BOAA017001@istruzione.it",
            "normale@example.test"
        ]
    );
}

#[test]
fn padded_at_separator_never_joins_two_columns() {
    let file = ImportFile {
        path: None,
        name: "colonne.csv".to_owned(),
        relative_path: "Varie/colonne.csv".to_owned(),
        bytes: b"denominazione;email\nComune di Rogno;@example.test\n".to_vec(),
    };

    let parsed = parse_file(&file).expect("columns should parse");
    assert!(parsed.contacts.is_empty());
}

#[test]
fn free_text_file_survives_an_unbalanced_quote() {
    let file = ImportFile {
        path: None,
        name: "Parrocchie 14.txt".to_owned(),
        relative_path: "Parrocchie/Parrocchie 14.txt".to_owned(),
        bytes: b"prima@example.test\nbi@works.r.l.\"s\nseconda@example.test\nterza@example.test\n"
            .to_vec(),
    };

    let parsed = parse_file(&file).expect("free text should parse");
    let emails = parsed
        .contacts
        .iter()
        .map(|contact| contact.normalized_email.as_str())
        .collect::<Vec<_>>();
    assert!(emails.contains(&"seconda@example.test"));
    assert!(emails.contains(&"terza@example.test"));

    assert_eq!(parsed.contacts[0].source_row, 1);
    assert_eq!(parsed.contacts.last().expect("rows").source_row, 4);
}

#[test]
fn malformed_csv_record_does_not_discard_the_rest_of_the_file() {
    let file = ImportFile {
        path: None,
        name: "rotto.csv".to_owned(),
        relative_path: "Varie/rotto.csv".to_owned(),
        bytes: b"email\nprima@example.test\n\"non chiuso;seconda@example.test\n".to_vec(),
    };

    let parsed = parse_file(&file).expect("malformed CSV should still parse");
    assert!(
        parsed
            .contacts
            .iter()
            .any(|contact| contact.normalized_email == "prima@example.test")
    );
    assert_eq!(parsed.row_errors.len(), 1);
    assert_eq!(parsed.row_errors[0].source_row, 3);
}

#[test]
fn extracts_unicode_addresses_and_preserves_case_and_comparison_key() {
    let file = ImportFile {
        path: None,
        name: "internazionali.txt".to_owned(),
        relative_path: "Varie/internazionali.txt".to_owned(),
        bytes: "Élodie@MÜNCHEN.公司\n".as_bytes().to_vec(),
    };
    let parsed = parse_file(&file).expect("Unicode text should parse");
    let contact = &parsed.contacts[0];
    assert_eq!(contact.raw_email, "Élodie@MÜNCHEN.公司");
    assert_eq!(contact.normalized_email, "Élodie@münchen.公司");
    assert_eq!(contact.comparison_key, "élodie@münchen.公司");
    assert_eq!(contact.normalization_method, "literal");
}

#[test]
fn records_obfuscation_method_without_losing_source_context() {
    let file = ImportFile {
        path: None,
        name: "offuscata.txt".to_owned(),
        relative_path: "Varie/offuscata.txt".to_owned(),
        bytes: b"info [at] example [dot] test\n".to_vec(),
    };
    let parsed = parse_file(&file).expect("obfuscated text should parse");
    let contact = &parsed.contacts[0];
    assert_eq!(contact.raw_email, "info [at] example [dot] test");
    assert_eq!(contact.normalized_email, "info@example.test");
    assert_eq!(contact.normalization_method, "bracket_separator");
    assert!(contact.raw_context.contains("[at]"));
}

#[test]
fn transformed_addresses_in_one_cell_keep_their_own_raw_fragments() {
    let file = ImportFile {
        path: None,
        name: "multiple-offuscate.txt".to_owned(),
        relative_path: "Varie/multiple-offuscate.txt".to_owned(),
        bytes: b"info [at] first [dot] test and info [at] second [dot] test\n".to_vec(),
    };
    let parsed = parse_file(&file).expect("multiple transformed addresses should parse");
    assert_eq!(parsed.contacts.len(), 2);
    assert_eq!(parsed.contacts[0].raw_email, "info [at] first [dot] test");
    assert_eq!(parsed.contacts[0].normalized_email, "info@first.test");
    assert_eq!(parsed.contacts[0].normalization_method, "bracket_separator");
    assert_eq!(parsed.contacts[1].raw_email, "info [at] second [dot] test");
    assert_eq!(parsed.contacts[1].normalized_email, "info@second.test");
    assert_eq!(parsed.contacts[1].normalization_method, "bracket_separator");
}

#[test]
fn literal_and_transformed_addresses_in_one_cell_get_distinct_methods() {
    let file = ImportFile {
        path: None,
        name: "mixed-offuscate.txt".to_owned(),
        relative_path: "Varie/mixed-offuscate.txt".to_owned(),
        bytes: b"literal@example.test and info [at] other [dot] test\n".to_vec(),
    };
    let parsed = parse_file(&file).expect("mixed addresses should parse");
    assert_eq!(parsed.contacts.len(), 2);
    assert_eq!(parsed.contacts[0].raw_email, "literal@example.test");
    assert_eq!(parsed.contacts[0].normalization_method, "literal");
    assert_eq!(parsed.contacts[1].raw_email, "info [at] other [dot] test");
    assert_eq!(parsed.contacts[1].normalization_method, "bracket_separator");
}

#[test]
fn parses_open_document_writer_rows() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <office:document-content xmlns:office="urn:o" xmlns:table="urn:t" xmlns:text="urn:x">
          <table:table><table:table-row><table:table-cell><text:p>Email</text:p></table:table-cell><table:table-cell><text:p>Nome</text:p></table:table-cell></table:table-row>
          <table:table-row><table:table-cell><text:p>info@esempio.test</text:p></table:table-cell><table:table-cell><text:p>Ufficio</text:p></table:table-cell></table:table-row></table:table>
        </office:document-content>"#;
    let rows = odt_rows(xml).expect("synthetic ODT XML should parse");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1][0], "info@esempio.test");
}

#[test]
fn opt_out_lists_are_recognised_regardless_of_extension() {
    for name in ["blacklist.csv", "Cancellami.xlsx", "blacklist.txt"] {
        let file = ImportFile {
            path: None,
            name: name.to_owned(),
            relative_path: name.to_owned(),
            bytes: Vec::new(),
        };
        assert!(is_blacklist_file(&file), "{name} should be an opt-out list");
    }
    let contacts = ImportFile {
        path: None,
        name: "Comuni 1.txt".to_owned(),
        relative_path: "Comuni/Comuni 1.txt".to_owned(),
        bytes: Vec::new(),
    };
    assert!(!is_blacklist_file(&contacts));
}

#[test]
fn recovers_every_supported_obfuscation_spelling() {
    let file = ImportFile {
        path: None,
        name: "offuscate.txt".to_owned(),
        relative_path: "Varie/offuscate.txt".to_owned(),
        bytes: "uno[at]example[dot]test\ndue(chiocciola)example(punto)test\ntre_at_example_dot_test\nquattro{at}example{dot}test\ncinque<at>example<dot>test\nsei&#64;example&#46;test\nsette%40example.test\notto at example dot test\nnove chiocciola example punto test\n"
            .as_bytes()
            .to_vec(),
    };

    let parsed = parse_file(&file).expect("obfuscated list should parse");
    let emails = parsed
        .contacts
        .iter()
        .map(|contact| contact.normalized_email.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "uno@example.test",
        "due@example.test",
        "tre@example.test",
        "quattro@example.test",
        "cinque@example.test",
        "sei@example.test",
        "sette@example.test",
        "otto@example.test",
        "nove@example.test",
    ] {
        assert!(
            emails.contains(&expected),
            "missing {expected} in {emails:?}"
        );
    }
}

#[test]
fn prose_containing_at_is_not_turned_into_an_address() {
    let file = ImportFile {
        path: None,
        name: "prosa.txt".to_owned(),
        relative_path: "Varie/prosa.txt".to_owned(),
        bytes: b"Ci vediamo at Roma domani\nIscrizioni at sede centrale\n".to_vec(),
    };

    let parsed = parse_file(&file).expect("prose should parse");
    assert!(parsed.contacts.is_empty(), "got {:?}", parsed.contacts);
}

#[test]
fn rot13_scrambled_addresses_are_decoded_not_dropped() {
    assert_eq!(
        decode_rot13_address("vasb@svgsrfgviny.pu").as_deref(),
        Some("info@fitfestival.ch")
    );
    assert_eq!(
        decode_rot13_address("hcfhfrtnan@tznvy.pbz").as_deref(),
        Some("upsusegana@gmail.com")
    );

    assert_eq!(decode_rot13_address("info@comune.milano.it"), None);
    assert_eq!(decode_rot13_address("segreteria@example.test"), None);
}

#[test]
fn byte_order_marks_select_the_encoding() {
    let utf16le: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain(
            "info@example.test"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        )
        .collect();
    let (text, warning) = decode_text(&utf16le);
    assert_eq!(text, "info@example.test");
    assert!(warning.unwrap_or_default().contains("UTF-16LE"));

    let (text, warning) = decode_text("citt\u{e0}@example.test".as_bytes());
    assert_eq!(text, "città@example.test");
    assert!(warning.is_none());

    let (text, warning) = decode_text(b"citt\xE0@example.test");
    assert_eq!(text, "città@example.test");
    assert!(warning.unwrap_or_default().contains("windows-1252"));
}

#[test]
fn repeated_header_names_are_kept_apart_in_the_captured_context() {
    let file = ImportFile {
        path: None,
        name: "doppio.csv".to_owned(),
        relative_path: "Varie/doppio.csv".to_owned(),
        bytes: b"comune;Email;Email\nMilano;uno@example.test;due@example.test\n".to_vec(),
    };

    let parsed = parse_file(&file).expect("duplicate headers should parse");
    assert_eq!(parsed.contacts.len(), 2);
    let context = &parsed.contacts[0].raw_context;
    assert!(context.contains("uno@example.test"), "{context}");
    assert!(context.contains("due@example.test"), "{context}");
}

#[test]
fn delimiter_detection_looks_past_a_long_blank_preamble() {
    let mut bytes = b"Email/Telefono/Skype/UID\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'\n', 40));
    bytes.extend_from_slice(b"uno@example.test,Milano\ndue@example.test,Roma\n");
    let file = ImportFile {
        path: None,
        name: "Associazioni Culturali.txt".to_owned(),
        relative_path: "Associazioni/Associazioni Culturali.txt".to_owned(),
        bytes,
    };

    let text = String::from_utf8(file.bytes.clone()).expect("utf8");
    assert_eq!(detect_delimiter(&text, b';'), b',');
    let parsed = parse_file(&file).expect("preamble file should parse");
    assert_eq!(parsed.contacts.len(), 2);
}

#[test]
fn additional_workbook_formats_are_routed_to_the_workbook_parser() {
    for extension in [".xlsb", ".ods", ".odt", ".xlsm"] {
        assert!(
            SPREADSHEET_EXTENSIONS.contains(&extension),
            "{extension} should be a workbook format"
        );
    }
}

#[test]
fn real_school_workbook_is_parsed_from_current_corpus() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/email/Scuole/Scuole 33.xlsx");
    if !path.is_file() {
        eprintln!("skipped: private corpus not found at {}", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("shifted school workbook fixture");
    let file = ImportFile {
        path: None,
        name: "Scuole-Paritarie-Aosta-Trento-e-Bolzano-2023-24.xlsx".to_owned(),
        relative_path: "Scuole/Scuole 33.xlsx".to_owned(),
        bytes,
    };

    let parsed = parse_file(&file).expect("shifted workbook should parse");
    let contact = parsed
        .contacts
        .iter()
        .find(|contact| contact.comparison_key == "info@kaiserhof.it")
        .expect("known school contact");

    assert!(parsed.parsed_rows > 0);
    assert_eq!(contact.category, "Istruzione");
}

#[test]
fn school_internal_type_has_priority_over_numbered_filename() {
    let file = ImportFile {
            path: None,
            name: "Scuole 1.csv".to_owned(),
            relative_path: "Scuole/Scuole 1.csv".to_owned(),
            bytes: b"Email;Comune;Tipologia istituto - denominazione\ninfanzia@example.it;Torino;SCUOLA DELL'INFANZIA\nprimaria@example.it;Torino;SCUOLA PRIMARIA\n".to_vec(),
        };
    let parsed = parse_file(&file).expect("parse");
    assert_eq!(parsed.contacts[0].subcategory.as_deref(), Some("Infanzia"));
    assert_eq!(parsed.contacts[1].subcategory.as_deref(), Some("Primaria"));
    assert!(
        parsed
            .contacts
            .iter()
            .all(|c| c.classification_method == "column")
    );
}

#[test]
fn school_csv_maps_email_pec_and_website_from_their_exact_headers() {
    let file = ImportFile {
        path: None,
        name: "Scuole 1.csv".to_owned(),
        relative_path: "Scuole/Scuole 1.csv".to_owned(),
        bytes: b"COMUNE;PROVINCIA;REGIONE;Codice scuola;Denominazione dell'istituto;Tipologia istituto - denominazione;Indirizzo (Via + numero civico);Codice Postale;Telefono;Fax;Email;Email - PEC;Sito web;Istituto principale di riferimento (sede direzione);Statale/Paritaria\nAglie';TORINO;Piemonte;TO1A16900R;REGINA M.CRISTINA;SCUOLA DELL'INFANZIA;V.BALUARDI 1;10011;012433352;012433352;segreteria@example.it;istituto@pec.example.it;www.example.it;TO1A16900R;PARITARIA\n".to_vec(),
    };

    let parsed = parse_file(&file).expect("school CSV should parse");
    assert_eq!(parsed.contacts.len(), 2);
    assert!(
        parsed
            .contacts
            .iter()
            .all(|contact| contact.website.as_deref() == Some("www.example.it"))
    );
}

#[test]
fn url_fields_reject_an_email_even_when_the_header_says_website() {
    let file = ImportFile {
        path: None,
        name: "Scuole 1.csv".to_owned(),
        relative_path: "Scuole/Scuole 1.csv".to_owned(),
        bytes: b"Email;Sito web\nsegreteria@example.it;errata@example.it\n".to_vec(),
    };

    let parsed = parse_file(&file).expect("school CSV should parse");
    assert_eq!(parsed.contacts.len(), 2);
    assert!(
        parsed
            .contacts
            .iter()
            .all(|contact| contact.website.is_none())
    );
    assert!(
        parsed
            .contacts
            .iter()
            .all(|contact| contact.source_url.is_none())
    );
}

#[test]
fn school_tipo_column_recognizes_additional_subcategories() {
    let file = ImportFile {
        path: None,
        name: "Scuole 46.csv".to_owned(),
        relative_path: "Scuole/Scuole 46.csv".to_owned(),
        bytes: b"Email;Comune;Tipo\ncomprensivo@example.it;Torino;ISTITUTO COMPRENSIVO\ncentro@example.it;Torino;CENTRO TERRITORIALE PERMANENTE\n".to_vec(),
    };
    let parsed = parse_file(&file).expect("parse");
    assert_eq!(
        parsed.contacts[0].subcategory.as_deref(),
        Some("Istituto comprensivo")
    );
    assert_eq!(
        parsed.contacts[1].subcategory.as_deref(),
        Some("Centro territoriale / CPIA")
    );
    assert!(
        parsed
            .contacts
            .iter()
            .all(|contact| contact.classification_method == "column")
    );
}

#[test]
fn mixed_filename_is_not_used_as_subcategory() {
    let file = ImportFile {
        path: None,
        name: "APT & Turismo & Proloco.xlsx".to_owned(),
        relative_path: "Associazioni/APT & Turismo & Proloco.xlsx".to_owned(),
        bytes: Vec::new(),
    };
    assert!(subcategory_from_file(&file).is_some());
    let fields = BTreeMap::from([("Denominazione".to_owned(), "Pro Loco Sondrio".to_owned())]);
    let (category, subcategory, _, _, _) = deterministic_classification(&file, &fields);
    assert_eq!(category, "Turismo e promozione territoriale");
    assert_eq!(subcategory.as_deref(), Some("Pro Loco"));
}

#[test]
fn multi_category_filename_requires_row_evidence() {
    let file = ImportFile {
        path: None,
        name: "Scuole, Comuni & Parrocchie.txt".to_owned(),
        relative_path: "Varie/Scuole, Comuni & Parrocchie.txt".to_owned(),
        bytes: Vec::new(),
    };
    let (category, subcategory, _, _, _) = deterministic_classification(&file, &BTreeMap::new());
    assert_eq!(category, "Varie");
    assert_eq!(subcategory.as_deref(), Some("Dato misto o da revisionare"));
}

#[test]
fn filter_sql_combines_include_and_exclude() {
    let filters = ListFilters {
        query: String::new(),
        category: FieldFilter {
            include: vec!["Scuole".to_owned(), "Associazioni".to_owned()],
            exclude: Vec::new(),
        },
        regione: FieldFilter {
            include: Vec::new(),
            exclude: vec!["Lombardia".to_owned()],
        },
        ..ListFilters::default()
    };
    let (sql, values) = filter_where(&filters);
    assert!(sql.contains("category"));
    assert!(sql.contains("lower(?)"));
    assert!(sql.contains("OR"));
    assert!(sql.contains("regione IS NULL OR NOT"));
    assert_eq!(values, vec!["Scuole", "Associazioni", "Lombardia"]);
}

#[test]
fn category_filter_uses_current_contact_category_not_historical_claims() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let mut contact = test_contact();
    contact.status = EmailStatus::Valid;
    insert_contact_record(&db, &contact).unwrap();
    insert_classification_claim(
        &db,
        ClassificationClaim {
            candidate_id: None,
            contact_id: Some(&contact.id),
            source_row_id: None,
            category: "Istruzione",
            subcategory: Some("Scuola primaria"),
            confidence: 0.9,
            method: "source",
            evidence: &["folder Scuole".to_owned()],
        },
    )
    .unwrap();
    insert_classification_claim(
        &db,
        ClassificationClaim {
            candidate_id: None,
            contact_id: Some(&contact.id),
            source_row_id: None,
            category: "Associazioni",
            subcategory: None,
            confidence: 0.8,
            method: "source",
            evidence: &["second source".to_owned()],
        },
    )
    .unwrap();
    let mut filters = ListFilters::default();
    filters.category.include.push("Istruzione".to_owned());
    assert_eq!(load_contacts(&db, &filters, 100, 0).unwrap().total, 1);
    filters.category.include = vec!["Associazioni".to_owned()];
    assert_eq!(load_contacts(&db, &filters, 100, 0).unwrap().total, 0);
}

#[test]
fn adaptive_context_keeps_normal_records_at_4k() {
    assert_eq!(
        ai_runs::adaptive_context_size(4096, &"x".repeat(7000)),
        4096
    );
    assert_eq!(
        ai_runs::adaptive_context_size(4096, &"x".repeat(7001)),
        8192
    );
}

#[test]
fn adaptive_context_does_not_exceed_supported_limit() {
    assert_eq!(ai_runs::adaptive_context_size(16384, "short"), 8192);
}

#[test]
fn pagination_contacts_covers_edges_bounds_stability_and_skips_facets() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for (id, email) in [
        ("b", "b@example.it"),
        ("z", "Same@example.it"),
        ("a", "same@example.it"),
        ("c", "z@example.it"),
    ] {
        let mut contact = test_contact();
        contact.id = id.to_owned();
        contact.email = email.to_owned();
        let (local, domain, tld) = email_parts(email);
        contact.email_local_part = local;
        contact.email_domain = domain;
        contact.email_tld = tld;
        insert_contact_record(&db, &contact).unwrap();
    }
    FACET_LOAD_COUNT.store(0, Ordering::SeqCst);
    let first = load_contacts(&db, &ListFilters::default(), 2, 0).unwrap();
    let middle = load_contacts(&db, &ListFilters::default(), 2, 2).unwrap();
    let last = load_contacts(&db, &ListFilters::default(), 2, 4).unwrap();
    let bounded = load_contacts(&db, &ListFilters::default(), 10_000, 0).unwrap();
    assert_eq!(
        first
            .contacts
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "a"]
    );
    assert_eq!(
        middle
            .contacts
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["z", "c"]
    );
    assert!(first.has_next && !first.has_previous);
    assert!(middle.has_previous && !middle.has_next);
    assert!(last.contacts.is_empty() && last.has_previous && !last.has_next);
    assert_eq!(bounded.limit, MAX_PAGE_SIZE);
    assert_eq!(FACET_LOAD_COUNT.load(Ordering::SeqCst), 0);
}

#[test]
fn load_contacts_joins_taxonomy_version_without_correlated_subquery() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let version_id: i64 = db
        .query_row(
            "SELECT id FROM taxonomy_versions WHERE status='active'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let version: String = db
        .query_row(
            "SELECT semantic_version FROM taxonomy_versions WHERE id=?1",
            params![version_id],
            |row| row.get(0),
        )
        .unwrap();
    for index in 0..500 {
        let mut contact = test_contact();
        contact.id = format!("c-{index}");
        contact.email = format!("user{index}@example.test");
        let (local, domain, tld) = email_parts(&contact.email);
        contact.email_local_part = local;
        contact.email_domain = domain;
        contact.email_tld = tld;
        insert_contact_record(&db, &contact).unwrap();
        db.execute(
            "UPDATE contacts SET taxonomy_version_id=?1 WHERE id=?2",
            params![version_id, contact.id],
        )
        .unwrap();
    }
    let started = std::time::Instant::now();
    let page = load_contacts(&db, &ListFilters::default(), 100, 0).unwrap();
    let list_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(page.contacts.len(), 100);
    assert!(
        page.contacts
            .iter()
            .all(|c| c.taxonomy_version.as_deref() == Some(version.as_str()))
    );
    let facet_started = std::time::Instant::now();
    let _ = load_filter_options(&db).unwrap();
    let facet_ms = facet_started.elapsed().as_secs_f64() * 1000.0;
    // First-pass guard: list path should stay under a soft ceiling on this synthetic set.
    assert!(
        list_ms < 500.0,
        "load_contacts took {list_ms:.1}ms for 500-row DB / 100-row page"
    );
    eprintln!("[perf] load_contacts 500→100 rows: {list_ms:.2}ms");
    eprintln!("[perf] load_filter_options on 500 rows: {facet_ms:.2}ms");
}

#[test]
fn list_search_uses_fts_and_skip_count_avoids_full_count() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for (id, email, comune) in [
        ("a", "alpha@example.test", "Torino"),
        ("b", "beta@example.test", "Milano"),
        ("c", "gamma@example.test", "Torino"),
    ] {
        let mut contact = test_contact();
        contact.id = id.to_owned();
        contact.email = email.to_owned();
        contact.comune = Some(comune.to_owned());
        let (local, domain, tld) = email_parts(email);
        contact.email_local_part = local;
        contact.email_domain = domain;
        contact.email_tld = tld;
        insert_contact_record(&db, &contact).unwrap();
    }
    let filters = ListFilters {
        query: "torino".to_owned(),
        ..ListFilters::default()
    };
    let page = load_contacts(&db, &filters, 10, 0).unwrap();
    assert_eq!(page.total, 2);
    assert!(
        page.contacts
            .iter()
            .all(|c| c.comune.as_deref() == Some("Torino"))
    );

    let skipped = load_contacts_page(&db, &ListFilters::default(), 2, 0, false).unwrap();
    assert_eq!(skipped.contacts.len(), 2);
    assert!(skipped.has_next);
    assert_eq!(skipped.total, 3);

    let fts_exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='contacts_fts')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(fts_exists);
    let list_index: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_contacts_list_sort')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(list_index);
    let dropped_geo: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_contacts_geo')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!dropped_geo);
}

#[test]
fn import_cache_rebuilds_fts_after_suspend() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let contact = ImportContact {
        raw_email: "utente@example.test".to_owned(),
        normalized_email: "utente@example.test".to_owned(),
        comparison_key: "utente@example.test".to_owned(),
        normalization_method: "literal".to_owned(),
        email_local_part: "utente".to_owned(),
        email_domain: "example.test".to_owned(),
        email_tld: ".test".to_owned(),
        status: EmailStatus::Valid,
        validation_notes: Vec::new(),
        source_file: std::sync::Arc::from("bench.csv"),
        category: "Comuni".to_owned(),
        subcategory: None,
        contact_kind: "istituzionale".to_owned(),
        paese: "Italia".to_owned(),
        is_pec: false,
        comune: Some("Torino".to_owned()),
        provincia: None,
        regione: None,
        macroarea: None,
        website: None,
        source_url: None,
        source_sheet: None,
        source_row: 1,
        raw_context: std::sync::Arc::from("{}"),
        classification_method: "deterministic".to_owned(),
        classification_confidence: 1.0,
        classification_evidence: Vec::new(),
    };

    let tx = db.transaction().unwrap();
    crate::contacts::suspend_contact_triggers(&tx).unwrap();
    let mut cache = crate::contacts::ContactImportCache::default();
    crate::contacts::save_contact_for_import(
        &tx,
        &contact,
        "",
        None,
        None,
        "2026-01-01T00:00:00Z",
        &mut cache,
    )
    .unwrap();
    crate::contacts::flush_contact_import_cache(&tx, &mut cache, "2026-01-01T00:00:00Z").unwrap();
    let triggers_while_suspended: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'contacts_fts_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        triggers_while_suspended, 0,
        "FTS sync triggers must be dropped for the import transaction"
    );
    crate::contacts::resume_contact_triggers(&tx).unwrap();
    let triggers_after: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'contacts_fts_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(triggers_after, 3);
    tx.commit().unwrap();

    let matched: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM contacts_fts WHERE contacts_fts MATCH 'torino'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(matched, 1);
    let filters = ListFilters {
        query: "torino".to_owned(),
        ..ListFilters::default()
    };
    let page = load_contacts(&db, &filters, 10, 0).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.contacts[0].email, "utente@example.test");
}

#[test]
fn pagination_blacklist_covers_first_middle_last_and_bound() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for email in ["a@blocked.it", "b@blocked.it", "c@blocked.it"] {
        db.execute(
            "INSERT INTO blacklist(normalized_email,source_file,imported_at) VALUES (?1,'x','now')",
            params![email],
        )
        .unwrap();
    }
    let first = load_blacklist_page(&db, "", 2, 0).unwrap();
    let middle = load_blacklist_page(&db, "", 2, 2).unwrap();
    let empty = load_blacklist_page(&db, "", 2, 4).unwrap();
    assert_eq!(first.entries.len(), 2);
    assert!(first.has_next && !first.has_previous);
    assert_eq!(middle.entries[0].email, "c@blocked.it");
    assert!(middle.has_previous && !middle.has_next);
    assert!(empty.entries.is_empty());
    assert_eq!(
        load_blacklist_page(&db, "", usize::MAX, 0).unwrap().limit,
        MAX_PAGE_SIZE
    );
}

#[test]
fn candidate_listing_includes_quarantine_and_malformed_source_rows() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO imports(id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at)
         VALUES ('candidate-import','source.csv','Folder/source.csv','legacy','partial',2,1,'now')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO source_rows(id,import_id,source_sheet,row_number,raw_context,parse_status,error_message,created_at)
         VALUES ('candidate-row','candidate-import','',1,'{}','parsed',NULL,'now'),
                ('error-row','candidate-import','',2,'broken','malformed','bad csv row','now')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO email_candidates(
            id,source_row_id,raw_email,canonical_email,comparison_key,status,candidate_status,
            current_disposition,disposition_reason,first_seen_at,last_seen_at
         ) VALUES ('candidate-1','candidate-row','bad@example.test','bad@example.test',
                   'bad@example.test','no_routing','no_routing','no_routing','domain_no_routing','now','now')",
        [],
    )
    .unwrap();

    let page = crate::candidates::load_candidates(
        &db,
        &crate::candidates::CandidateFilters::default(),
        100,
        0,
    )
    .unwrap();
    assert_eq!(page.total, 2);
    assert!(
        page.candidates
            .iter()
            .any(|candidate| candidate.disposition == "parse_error")
    );

    db.execute(
        "INSERT INTO email_candidates(
            id,source_row_id,raw_email,canonical_email,comparison_key,status,candidate_status,
            current_disposition,disposition_reason,occurrence_index,first_seen_at,last_seen_at
         ) VALUES ('candidate-2','candidate-row','bad@example.test','bad@example.test',
                   'bad@example.test','no_routing','no_routing','no_routing','domain_no_routing',1,'now','now')",
        [],
    )
    .unwrap();

    let page2 = crate::candidates::load_candidates(
        &db,
        &crate::candidates::CandidateFilters::default(),
        100,
        0,
    )
    .unwrap();
    assert_eq!(
        page2.total, 2,
        "duplicate candidate email should be counted once"
    );
    assert_eq!(page2.candidates.len(), 2);
}

#[test]
fn contact_insert_and_update_sets_is_pec_accurately() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();

    let pec_contact = ImportContact {
        raw_email: "protocollo@pec.example.it".to_owned(),
        normalized_email: "protocollo@pec.example.it".to_owned(),
        comparison_key: "protocollo@pec.example.it".to_owned(),
        normalization_method: "literal".to_owned(),
        email_local_part: "protocollo".to_owned(),
        email_domain: "pec.example.it".to_owned(),
        email_tld: ".it".to_owned(),
        status: EmailStatus::Valid,
        validation_notes: Vec::new(),
        source_file: std::sync::Arc::from("test.csv"),
        category: "Istruzione".to_owned(),
        subcategory: None,
        contact_kind: "istituzionale".to_owned(),
        paese: "Italia".to_owned(),
        is_pec: true,
        comune: None,
        provincia: None,
        regione: None,
        macroarea: None,
        website: None,
        source_url: None,
        source_sheet: None,
        source_row: 1,
        raw_context: std::sync::Arc::from("{}"),
        classification_method: "deterministic".to_owned(),
        classification_confidence: 1.0,
        classification_evidence: Vec::new(),
    };

    let mut cache = crate::contacts::ContactImportCache::default();
    let outcome = crate::contacts::save_contact_for_import(
        &db,
        &pec_contact,
        "",
        None,
        None,
        "2026-01-01T00:00:00Z",
        &mut cache,
    )
    .expect("save pec contact");

    let is_pec_val: i64 = db
        .query_row(
            "SELECT is_pec FROM contacts WHERE id=?1",
            params![outcome.contact_id],
            |r| r.get(0),
        )
        .expect("query is_pec");
    assert_eq!(is_pec_val, 1);

    let list = crate::contacts::load_contacts(&db, &crate::ListFilters::default(), 10, 0)
        .expect("load contacts");
    assert_eq!(list.contacts.len(), 1);
    assert!(list.contacts[0].is_pec);

    let mut pec_filter = crate::ListFilters::default();
    pec_filter.pec.include = vec!["pec".to_owned()];
    let pec_list = crate::contacts::load_contacts(&db, &pec_filter, 10, 0).expect("load pec");
    assert_eq!(pec_list.total, 1);

    let mut std_filter = crate::ListFilters::default();
    std_filter.pec.include = vec!["standard".to_owned()];
    let std_list = crate::contacts::load_contacts(&db, &std_filter, 10, 0).expect("load standard");
    assert_eq!(std_list.total, 0);

    let pec_facets = crate::contacts::facet_values(&db, "pec").expect("pec facets");
    assert_eq!(pec_facets.len(), 1);
    assert_eq!(pec_facets[0].value, "pec");
    assert_eq!(pec_facets[0].count, 1);
}

#[test]
fn geographic_facets_filter_out_cadastral_codes_and_numbers_and_normalize_names() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();

    db.execute_batch(
        "INSERT INTO contacts(
            id,email,email_local_part,email_domain,email_tld,status,validation_notes,
            source_file,category,regione,provincia,comune
         ) VALUES
         ('c-1','u1@example.it','u1','example','.it','valid','[]','f.txt','Varie','Abruzzi','BT','A754'),
         ('c-2','u2@example.it','u2','example','.it','valid','[]','f.txt','Varie','3','Barletta Andr5','B042'),
         ('c-3','u3@example.it','u3','example','.it','valid','[]','f.txt','Varie','Friuli Venezia G.','Bari','Abano Terme'),
         ('c-4','u4@example.it','u4','example','.it','valid','[]','f.txt','Varie','Valle d’Aosta','Forlì Cesena','Adorno Micca (bi)'),
         ('c-5','u5@example.it','u5','example','.it','valid','[]','f.txt','Varie','VALLE D''AOSTA','Forli'' Cesena','Adorno Micca'),
         ('c-6','u6@example.it','u6','example','.it','valid','[]','f.txt','Varie','Toscana','Massa Carrara','Comune di Firenze'),
         ('c-7','u7@example.it','u7','example','.it','valid','[]','f.txt','Varie','Toscana','MS','Firenze'),
         ('c-8','u8@example.it','u8','example','.it','valid','[]','f.txt','Varie','Marche','Pesaro-Urbino','Pesaro'),
         ('c-9','u9@example.it','u9','example','.it','valid','[]','f.txt','Varie','Marche','PU','Pesaro'),
         ('c-10','u10@example.it','u10','example','.it','valid','[]','f.txt','Varie','Sardegna','Carbonia-Iglesias','Carbonia'),
         ('c-11','u11@example.it','u11','example','.it','valid','[]','f.txt','Varie','Lombardia','Como','Cantù'),
         ('c-12','u12@example.it','u12','example','.it','valid','[]','f.txt','Varie','Veneto','Venezia','San Donà di Piave - VE'),
         ('c-13','u13@example.it','u13','example','.it','valid','[]','f.txt','Varie','Piemonte','Biella','Andorno Micca - BI'),
         ('c-14','u14@example.it','u14','example','.it','valid','[]','f.txt','Varie','Toscana','Pistoia','Abetone'),
         ('c-15','u15@example.it','u15','example','.it','valid','[]','f.txt','Varie','Toscana','Pistoia','Abetone Cutigliano'),
         ('c-16','u16@example.it','u16','example','.it','valid','[]','f.txt','Varie','Sardegna','Nuoro','Belvi'),
         ('c-17','u17@example.it','u17','example','.it','valid','[]','f.txt','Varie','Sardegna','Nuoro','Belvì'),
         ('c-18','u18@example.it','u18','example','.it','valid','[]','f.txt','Varie','Sardegna','Nuoro','Belvi'''),
         ('c-19','u19@example.it','u19','example','.it','valid','[]','f.txt','Varie','Sardegna','OG','Tortolì'),
         ('c-20','u20@example.it','u20','example','.it','valid','[]','f.txt','Varie','Sardegna','OT','Olbia'),
         ('c-21','u21@example.it','u21','example','.it','valid','[]','f.txt','Varie','Sardegna','VS','Sanluri'),
         ('c-22','u22@example.it','u22','example','.it','valid','[]','f.txt','Varie','valle daosta','AO','Saint-Vincent'),
         ('c-23','u23@example.it','u23','example','.it','valid','[]','f.txt','Varie','Fake Region','Barletta Andr7 Andrc.d.','Valid Comune'),
         ('c-24','u24@example.it','u24','example','.it','valid','[]','f.txt','Varie','Lazio','NonExistentProv123','Corrupted123Comune');"
    ).unwrap();

    let comuni = crate::contacts::facet_values(&db, "comune").unwrap();
    let comune_names: Vec<&str> = comuni.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(
        comune_names
            .iter()
            .filter(|&&c| c == "Andorno Micca")
            .count(),
        1,
        "Adorno Micca, Adorno Micca (bi) and Andorno Micca - BI should merge into exactly 1 facet"
    );
    assert_eq!(
        comune_names.iter().filter(|&&c| c == "Firenze").count(),
        1,
        "Comune di Firenze and Firenze should merge"
    );
    assert_eq!(
        comune_names
            .iter()
            .filter(|&&c| c == "Abetone Cutigliano")
            .count(),
        1,
        "Abetone and Abetone Cutigliano should merge into 1 facet"
    );
    assert_eq!(
        comune_names.iter().filter(|&&c| c == "Belvì").count(),
        1,
        "Belvi, Belvì, Belvi' should merge into 1 facet"
    );
    assert!(
        comune_names.contains(&"Cantù"),
        "Accented city Cantù should be preserved"
    );
    assert!(
        comune_names.contains(&"San Donà di Piave"),
        "Accented city San Donà with stripped - VE should be preserved"
    );
    assert!(!comune_names.contains(&"A754"));
    assert!(!comune_names.contains(&"B042"));
    assert!(!comune_names.contains(&"Corrupted123Comune"));

    let regioni = crate::contacts::facet_values(&db, "regione").unwrap();
    let region_names: Vec<&str> = regioni.iter().map(|r| r.value.as_str()).collect();
    assert_eq!(
        region_names
            .iter()
            .filter(|&&r| r == "Valle d'Aosta")
            .count(),
        1,
        "all Valle d'Aosta variants should merge into exactly 1 facet"
    );
    assert!(!region_names.contains(&"3"));
    assert!(!region_names.contains(&"Fake Region"));
    assert!(region_names.contains(&"Abruzzo"));
    assert!(region_names.contains(&"Friuli Venezia Giulia"));

    let province = crate::contacts::facet_values(&db, "provincia").unwrap();
    let prov_names: Vec<&str> = province.iter().map(|p| p.value.as_str()).collect();
    assert_eq!(
        prov_names.iter().filter(|&&p| p == "Forlì-Cesena").count(),
        1,
        "all Forlì Cesena variants should merge into exactly 1 facet"
    );
    assert_eq!(
        prov_names.iter().filter(|&&p| p == "Massa-Carrara").count(),
        1,
        "Massa Carrara and MS should merge into 1 facet"
    );
    assert_eq!(
        prov_names
            .iter()
            .filter(|&&p| p == "Pesaro e Urbino")
            .count(),
        1,
        "Pesaro-Urbino and PU should merge into 1 facet"
    );
    assert_eq!(
        prov_names
            .iter()
            .filter(|&&p| p == "Barletta-Andria-Trani")
            .count(),
        1,
        "Barletta Andr5 and Barletta Andr7 Andrc.d. should normalize to Barletta-Andria-Trani"
    );
    assert_eq!(
        prov_names.iter().filter(|&&p| p == "Sud Sardegna").count(),
        1,
        "Carbonia-Iglesias, OG and VS should normalize to Sud Sardegna"
    );
    assert_eq!(
        prov_names.iter().filter(|&&p| p == "Sassari").count(),
        1,
        "OT should normalize to Sassari"
    );
    assert!(!prov_names.contains(&"OG"));
    assert!(!prov_names.contains(&"OT"));
    assert!(!prov_names.contains(&"VS"));
    assert!(!prov_names.contains(&"Og"));
    assert!(!prov_names.contains(&"Ot"));
    assert!(!prov_names.contains(&"Vs"));
    assert!(!prov_names.contains(&"Barletta Andr7 Andrc.d."));
    assert!(!prov_names.contains(&"NonExistentProv123"));
}

#[test]
fn pagination_streamed_exports_preserve_csv_and_refuse_overwrite() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for index in 0..500 {
        let email = format!("person-{index:04}@example.it");
        let mut contact = test_contact();
        contact.id = format!("contact-{index:04}");
        contact.email = email.clone();
        let (local, domain, tld) = email_parts(&email);
        contact.email_local_part = local;
        contact.email_domain = domain;
        contact.email_tld = tld;
        contact.source_file = if index == 0 {
            "a;quoted.csv".to_owned()
        } else {
            "bulk.csv".to_owned()
        };
        insert_contact_record(&db, &contact).unwrap();
    }
    let target = migration_test_path("contacts-export").with_extension("csv");
    let count = export_contacts_to_path(&db, &ListFilters::default(), &target).unwrap();
    assert_eq!(count, 500);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_path(&target)
        .unwrap();
    let records = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        records.first().unwrap().get(0),
        Some("person-0000@example.it")
    );
    assert_eq!(records.first().unwrap().get(1), Some("a;quoted.csv"));
    assert_eq!(
        records.last().unwrap().get(0),
        Some("person-0499@example.it")
    );
    assert!(export_contacts_to_path(&db, &ListFilters::default(), &target).is_err());
    std::fs::remove_file(target).unwrap();

    let emails_target = migration_test_path("contact-emails-export").with_extension("csv");
    let count =
        export_contact_emails_to_path(&db, &ListFilters::default(), &emails_target).unwrap();
    assert_eq!(count, 500);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(&emails_target)
        .unwrap();
    let records = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(records.len(), 500);
    assert!(records.iter().all(|record| record.len() == 1));
    assert_eq!(
        records.first().unwrap().get(0),
        Some("person-0000@example.it")
    );
    assert_eq!(
        records.last().unwrap().get(0),
        Some("person-0499@example.it")
    );
    std::fs::remove_file(emails_target).unwrap();

    let raced_target = migration_test_path("contacts-export-race").with_extension("csv");
    let raced_copy = raced_target.clone();
    let race_result = publish_export(&raced_target, |output| {
        output.write_all(b"new export").map_err(|e| e.to_string())?;
        std::fs::write(&raced_copy, b"concurrent owner").map_err(|e| e.to_string())?;
        Ok(1)
    });
    assert!(race_result.is_err());
    assert_eq!(std::fs::read(&raced_target).unwrap(), b"concurrent owner");
    std::fs::remove_file(raced_target).unwrap();

    db.execute(
        "INSERT INTO blacklist(normalized_email,source_file,imported_at)
             VALUES ('blocked@example.it','source;file','now')",
        [],
    )
    .unwrap();
    let blacklist_target = migration_test_path("blacklist-export").with_extension("csv");
    assert_eq!(
        export_blacklist_to_path(&db, "", &blacklist_target).unwrap(),
        1
    );
    let bytes = std::fs::read(&blacklist_target).unwrap();
    assert!(bytes.starts_with(b"\xEF\xBB\xBF"));
    std::fs::remove_file(blacklist_target).unwrap();

    let blacklist_emails_target =
        migration_test_path("blacklist-emails-export").with_extension("csv");
    assert_eq!(
        export_blacklist_emails_to_path(&db, "", &blacklist_emails_target).unwrap(),
        1
    );
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(&blacklist_emails_target)
        .unwrap();
    let records = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].len(), 1);
    assert_eq!(records[0].get(0), Some("blocked@example.it"));
    std::fs::remove_file(blacklist_emails_target).unwrap();
}

#[test]
fn ordinary_exports_exclude_untrusted_classifications_unless_explicitly_included() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    // Insufficient evidence keeps the previous values, so it stays exportable.
    for (index, state) in [
        "classified",
        "insufficient_evidence",
        "conflicting",
        "taxonomy_gap",
    ]
    .into_iter()
    .enumerate()
    {
        let mut contact = test_contact();
        contact.id = format!("trust-{index}");
        contact.email = format!("trust-{index}@example.it");
        let (local, domain, tld) = email_parts(&contact.email);
        contact.email_local_part = local;
        contact.email_domain = domain;
        contact.email_tld = tld;
        insert_contact_record(&db, &contact).unwrap();
        db.execute(
            "UPDATE contacts SET classification_state=?1 WHERE id=?2",
            params![state, contact.id],
        )
        .unwrap();
    }

    let trusted_path = migration_test_path("trusted-contact-export").with_extension("csv");
    assert_eq!(
        export_contacts_to_path_with_options(&db, &ListFilters::default(), &trusted_path, false,)
            .unwrap(),
        2
    );
    std::fs::remove_file(trusted_path).unwrap();

    let all_path = migration_test_path("all-email-export").with_extension("csv");
    assert_eq!(
        export_contact_emails_to_path_with_options(&db, &ListFilters::default(), &all_path, true,)
            .unwrap(),
        4
    );
    std::fs::remove_file(all_path).unwrap();
}

#[test]
fn taxonomy_rejects_cross_category_subcategory() {
    assert!(taxonomy_valid("Associazioni", Some("Alpini")));
    assert!(!taxonomy_valid("Varie", Some("Alpini")));
    assert!(taxonomy_valid("Varie", None));
}

#[test]
fn maps_file_hints_to_standard_categories() {
    assert_eq!(
        standard_category(["Comuni 6"].into_iter()),
        "Pubblica amministrazione"
    );
    assert_eq!(standard_category(["Scuole"].into_iter()), "Istruzione");
    assert_eq!(
        standard_category(["Parrocchie"].into_iter()),
        "Enti religiosi"
    );
    assert_eq!(
        standard_category(["associazione NOI"].into_iter()),
        "Associazioni"
    );
    assert_eq!(
        standard_category(["ATP Turismo Proloco"].into_iter()),
        "Turismo e promozione territoriale"
    );
    assert_eq!(standard_category(["Altro"].into_iter()), "Varie");
}

#[test]
fn detects_pec_from_kind_address_and_mx_host() {
    assert!(is_pec_from_kind(
        Some("ufficio@pec.example.it"),
        "ufficio@example.it"
    ));
    assert!(!is_pec_from_kind(Some("mail ordinaria"), "info@example.it"));
    assert!(is_pec_from_kind(None, "protocollo@legalmail.it"));
    assert!(is_pec_mx_host("mx.pec.aruba.it"));
    assert!(!is_pec_mx_host("mx.aruba.it"));
    assert!(is_pec(
        "info@comune.example.it",
        &["mail.asmecal.it".to_owned()]
    ));
    assert!(!is_pec(
        "info@comune.example.it",
        &["in.arubabusiness.it".to_owned()]
    ));
}

#[test]
fn fast_database_clear_preserves_blacklist_and_restores_foreign_keys() {
    let mut db = Connection::open_in_memory().expect("database");
    setup_schema(&db).expect("schema");
    db.execute_batch(
        "INSERT INTO contacts(
             id,email,email_local_part,email_domain,email_tld,status,validation_notes,
             source_file,category
         ) VALUES ('contact-1','utente@example.test','utente','example','.test','valid','[]','source.txt','Varie');
         INSERT INTO imports(id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at)
         VALUES ('import-1','source.txt','Varie/source.txt','hash','completed',1,1,CURRENT_TIMESTAMP);
         INSERT INTO source_rows(id,import_id,source_sheet,row_number,raw_context,parse_status,candidate_count,created_at)
         VALUES ('row-1','import-1','',1,'{}','parsed',1,CURRENT_TIMESTAMP);
         INSERT INTO email_candidates(
             id,source_row_id,raw_email,canonical_email,comparison_key,normalization_method,
             status,syntax_status,routing_status,mailbox_status,candidate_status,current_disposition,
             first_seen_at,last_seen_at
         ) VALUES (
             'candidate-1','row-1','utente@example.test','utente@example.test',
             'utente@example.test','literal','linked','syntax_ok','routing_ok','unknown',
             'linked','linked',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP
         );
         INSERT INTO blacklist(normalized_email,source_file,imported_at)
         VALUES ('optout@example.test','blacklist.txt',CURRENT_TIMESTAMP);
         INSERT INTO suppressions(comparison_key,canonical_email,reason,created_at)
         VALUES ('optout@example.test','optout@example.test','blacklist',CURRENT_TIMESTAMP),
                ('robot@example.test','robot@example.test','non_contact_mailbox',CURRENT_TIMESTAMP);",
    )
    .expect("seed clear graph");

    assert_eq!(clear_database_connection(&mut db).expect("clear"), 1);
    for table in ["contacts", "imports", "source_rows", "email_candidates"] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count cleared table");
        assert_eq!(count, 0, "{table} should be empty");
    }
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT COUNT(*) FROM blacklist", [], |row| row.get(0))
            .expect("blacklist count"),
        1
    );
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT COUNT(*) FROM suppressions", [], |row| row.get(0))
            .expect("suppression count"),
        1
    );
    assert_eq!(
        db.pragma_query_value::<i64, _>(None, "foreign_keys", |row| row.get(0))
            .expect("foreign key setting"),
        1
    );
}

#[test]
fn macroarea_facets_and_automatic_derivation_from_regione() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();

    let mut c1 = test_contact();
    c1.id = "c-nord-ovest".to_owned();
    c1.email = "to@example.test".to_owned();
    c1.regione = Some("Piemonte".to_owned());
    insert_contact_record(&db, &c1).unwrap();

    let mut c2 = test_contact();
    c2.id = "c-nord-est".to_owned();
    c2.email = "ve@example.test".to_owned();
    c2.regione = Some("Veneto".to_owned());
    insert_contact_record(&db, &c2).unwrap();

    let mut c3 = test_contact();
    c3.id = "c-centro".to_owned();
    c3.email = "rm@example.test".to_owned();
    c3.regione = Some("Lazio".to_owned());
    insert_contact_record(&db, &c3).unwrap();

    let mut c4 = test_contact();
    c4.id = "c-sud".to_owned();
    c4.email = "na@example.test".to_owned();
    c4.regione = Some("Campania".to_owned());
    insert_contact_record(&db, &c4).unwrap();

    let mut c5 = test_contact();
    c5.id = "c-isole".to_owned();
    c5.email = "pa@example.test".to_owned();
    c5.regione = Some("Sicilia".to_owned());
    insert_contact_record(&db, &c5).unwrap();

    let mut c6 = test_contact();
    c6.id = "c-unknown".to_owned();
    c6.email = "unknown@example.test".to_owned();
    c6.regione = None;
    insert_contact_record(&db, &c6).unwrap();

    let facets = crate::contacts::facet_values(&db, "macroarea").expect("macroarea facets");
    let facet_map: std::collections::BTreeMap<String, usize> = facets
        .into_iter()
        .map(|opt| (opt.value, opt.count))
        .collect();

    assert_eq!(facet_map.get("Nord-Ovest"), Some(&1));
    assert_eq!(facet_map.get("Nord-Est"), Some(&1));
    assert_eq!(facet_map.get("Centro"), Some(&1));
    assert_eq!(facet_map.get("Sud"), Some(&1));
    assert_eq!(facet_map.get("Isole"), Some(&1));
    assert_eq!(facet_map.len(), 5);

    let mut filters = ListFilters::default();
    filters.macroarea.include.push("Nord-Ovest".to_owned());
    let list = load_contacts(&db, &filters, 10, 0).expect("load contacts");
    assert_eq!(list.total, 1);
    assert_eq!(list.contacts[0].email, "to@example.test");
    assert_eq!(list.contacts[0].macroarea.as_deref(), Some("Nord-Ovest"));
}

fn seed_lead_contact(db: &Connection, id: &str, email: &str) -> ContactRecord {
    let mut contact = test_contact();
    contact.id = id.to_owned();
    contact.email = email.to_owned();
    let (local, domain, tld) = email_parts(email);
    contact.email_local_part = local;
    contact.email_domain = domain;
    contact.email_tld = tld;
    contact.source_file = "bulk.csv".to_owned();
    insert_contact_record(db, &contact).unwrap();
    contact
}

fn stored_score(db: &Connection, id: &str) -> Option<i64> {
    db.query_row(
        "SELECT quality_score FROM contacts WHERE id=?1",
        params![id],
        |row| row.get(0),
    )
    .unwrap()
}

fn sqlite_now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[test]
fn contact_list_sorts_by_column_with_empty_values_last() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for (id, email, comune, score) in [
        ("a", "a@example.test", Some("Torino"), 40),
        ("b", "b@example.test", Some("Asti"), 90),
        ("c", "c@example.test", None, 65),
        ("d", "d@example.test", Some("biella"), 65),
    ] {
        seed_lead_contact(&db, id, email);
        db.execute(
            "UPDATE contacts SET comune=?1 WHERE id=?2",
            params![comune, id],
        )
        .unwrap();
        // After the comune: it feeds the score, which the trigger would recompute.
        db.execute(
            "UPDATE contacts SET quality_score=?1 WHERE id=?2",
            params![score, id],
        )
        .unwrap();
    }
    let order = |column: &str, direction: &str| {
        let filters = ListFilters {
            sort: Some(crate::ListSort {
                column: column.to_owned(),
                direction: direction.to_owned(),
            }),
            ..ListFilters::default()
        };
        load_contacts(&db, &filters, 10, 0)
            .unwrap()
            .contacts
            .into_iter()
            .map(|contact| contact.id.clone())
            .collect::<Vec<_>>()
    };
    // Text ignores case and keeps the empty comune last in both directions.
    assert_eq!(order("comune", "asc"), ["b", "d", "a", "c"]);
    assert_eq!(order("comune", "desc"), ["a", "d", "b", "c"]);
    // Numbers sort numerically; ties follow the address in the direction of the sort.
    assert_eq!(order("score", "desc"), ["b", "d", "c", "a"]);
    assert_eq!(order("score", "asc"), ["a", "c", "d", "b"]);
    assert_eq!(order("", "asc"), ["a", "b", "c", "d"]);
    assert_eq!(order("email", "desc"), ["d", "c", "b", "a"]);
    // Classification sorts by the method label the table shows, not by confidence.
    for (id, method) in [
        ("a", "ai"),
        ("b", "rule"),
        ("c", "deterministic"),
        ("d", "column"),
    ] {
        db.execute(
            "UPDATE contacts SET classification_method=?1 WHERE id=?2",
            params![method, id],
        )
        .unwrap();
    }
    // Colonna, Deterministico, IA, Regola
    assert_eq!(order("classification", "asc"), ["d", "c", "a", "b"]);
    assert_eq!(order("classification", "desc"), ["b", "a", "c", "d"]);

    // Pages walk the non-empty values and then the empty ones without gaps or repeats.
    for (index, id) in ["e", "f", "g"].into_iter().enumerate() {
        seed_lead_contact(&db, id, &format!("{id}@example.test"));
        if index == 0 {
            db.execute(
                "UPDATE contacts SET comune='Cuneo' WHERE id=?1",
                params![id],
            )
            .unwrap();
        }
    }
    for direction in ["asc", "desc"] {
        let filters = ListFilters {
            sort: Some(crate::ListSort {
                column: "comune".to_owned(),
                direction: direction.to_owned(),
            }),
            ..ListFilters::default()
        };
        let whole = order("comune", direction);
        let mut paged = Vec::new();
        for offset in (0..whole.len()).step_by(3) {
            for include_total in [true, false] {
                let page = load_contacts_page(&db, &filters, 3, offset, include_total).unwrap();
                if include_total {
                    paged.extend(page.contacts.iter().map(|contact| contact.id.clone()));
                }
            }
        }
        assert_eq!(paged, whole, "{direction}");
    }

    // A sorted page reads an index instead of sorting the list.
    for (column, direction) in [
        ("comune", "asc"),
        ("comune", "desc"),
        ("score", "desc"),
        ("email", "desc"),
    ] {
        let plan: String = db
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT id FROM contacts WHERE is_exportable=1 ORDER BY {}",
                match (column, direction) {
                    ("comune", "asc") => "(coalesce(nullif(comune,''),char(1114111)) COLLATE NOCASE),lower(email),id",
                    ("comune", _) => "(coalesce(nullif(comune,''),char(1114111)) COLLATE NOCASE) DESC,lower(email) DESC,id DESC",
                    ("score", _) => "quality_score DESC,lower(email) DESC,id DESC",
                    _ => "lower(email) DESC,id DESC",
                }
            ))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" / ");
        assert!(
            plan.contains("USING INDEX") && !plan.contains("TEMP B-TREE"),
            "{column} {direction}: {plan}"
        );
    }
    let unknown = ListFilters {
        sort: Some(crate::ListSort {
            column: "id; DROP TABLE contacts".to_owned(),
            direction: "asc".to_owned(),
        }),
        ..ListFilters::default()
    };
    assert!(load_contacts(&db, &unknown, 10, 0).is_err());
}

#[test]
fn contact_score_is_stored_on_insert_and_follows_the_record() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let now = sqlite_now();
    seed_lead_contact(&db, "weak", "info@example.test");
    seed_lead_contact(&db, "strong", "giulia.bianchi@example.test");
    let weak = stored_score(&db, "weak").expect("scored on insert");
    assert!((0..=100).contains(&weak));

    db.execute(
        "UPDATE contacts SET person_name='Giulia Bianchi', role='Presidente',
            contact_kind='personale', organization_name='Associazione Esempio',
            comune='Torino', website='https://example.test', address='Via Roma 1',
            official_code='X1', category='Associazioni', subcategory='Alpini',
            last_seen_at=?1, routing_status='routing_ok', mailbox_status='accepted'
         WHERE id='strong'",
        params![now],
    )
    .unwrap();
    assert_eq!(stored_score(&db, "strong"), Some(100));
    assert!(weak < 100);

    // The score depends on what the record says, not on a campaign keyword.
    db.execute(
        "UPDATE contacts SET role='Referente bullismo' WHERE id='strong'",
        [],
    )
    .unwrap();
    assert_eq!(stored_score(&db, "strong"), Some(100));

    // Deliverability: a PEC address and a suspected typo score lower.
    db.execute("UPDATE contacts SET is_pec=1 WHERE id='strong'", [])
        .unwrap();
    assert_eq!(stored_score(&db, "strong"), Some(85));
    db.execute(
        "UPDATE contacts SET is_pec=0, is_typo_suspected=1 WHERE id='strong'",
        [],
    )
    .unwrap();
    let typo = stored_score(&db, "strong").unwrap();
    assert!(
        typo > 85 && typo < 100,
        "typo penalty is milder than PEC: {typo}"
    );
}

#[test]
fn contact_score_ignores_sources_and_classification_method_but_rewards_rich_specific_data() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_lead_contact(&db, "a", "utente@example.test");
    let score = stored_score(&db, "a").unwrap();
    // Source count and classification method or confidence do not move it.
    for (sources, method, confidence) in [
        (1, "ai", 0.4),
        (5, "deterministic", 1.0),
        (9, "column", 0.0),
    ] {
        db.execute(
            "UPDATE contacts SET independent_source_count=?1, classification_method=?2,
                classification_confidence=?3",
            params![sources, method, confidence],
        )
        .unwrap();
        assert_eq!(
            stored_score(&db, "a"),
            Some(score),
            "{sources} {method} {confidence}"
        );
    }

    // Each descriptive field adds to it.
    let mut previous = score;
    for update in [
        "organization_name='Istituto Esempio'",
        "comune='Bergamo'",
        "website='https://example.test'",
        "address='Via Roma 1'",
        "official_code='BGIC00000A'",
    ] {
        db.execute(&format!("UPDATE contacts SET {update}"), [])
            .unwrap();
        let next = stored_score(&db, "a").unwrap();
        assert!(next > previous, "{update}: {next} vs {previous}");
        previous = next;
    }

    // A catch-all subcategory scores below a specific one, and an empty category lowest.
    db.execute(
        "UPDATE contacts SET category='Istruzione', subcategory='Altro ente formativo'",
        [],
    )
    .unwrap();
    let generic = stored_score(&db, "a").unwrap();
    db.execute("UPDATE contacts SET subcategory='Primaria'", [])
        .unwrap();
    let specific = stored_score(&db, "a").unwrap();
    db.execute(
        "UPDATE contacts SET category='Varie', subcategory='Ente non determinato'",
        [],
    )
    .unwrap();
    let unclassified = stored_score(&db, "a").unwrap();
    assert!(
        unclassified < generic && generic < specific,
        "{unclassified} {generic} {specific}"
    );
    for name in [
        "Associazione generica",
        "Altro ente pubblico",
        "Dato misto o da revisionare",
        "",
    ] {
        db.execute(
            "UPDATE contacts SET category='Istruzione', subcategory=?1",
            params![name],
        )
        .unwrap();
        assert_eq!(stored_score(&db, "a"), Some(generic), "{name:?}");
    }

    // Migration 64 rescores a database scored with the earlier formula.
    db.execute("UPDATE contacts SET quality_score=-1", [])
        .unwrap();
    db.execute("DELETE FROM schema_migrations WHERE version=64", [])
        .unwrap();
    db.pragma_update(None, "user_version", 63).unwrap();
    migrate_database(&mut db, None, false).unwrap();
    assert_eq!(stored_score(&db, "a"), Some(generic));
}

#[test]
fn contact_score_rescoring_skips_unchanged_records() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_lead_contact(&db, "a", "utente.a@example.test");
    seed_lead_contact(&db, "b", "utente.b@example.test");
    let real = stored_score(&db, "a").unwrap();
    let sentinel = if real == 7 { 8 } else { 7 };
    db.execute("UPDATE contacts SET quality_score=?1", params![sentinel])
        .unwrap();

    // A column the score does not read leaves the stored value alone.
    db.execute(
        "UPDATE contacts SET source_file='altro.csv' WHERE id='a'",
        [],
    )
    .unwrap();
    assert_eq!(stored_score(&db, "a"), Some(sentinel));
    // A column it reads rescores that record only.
    db.execute("UPDATE contacts SET role='Segretario' WHERE id='a'", [])
        .unwrap();
    assert_ne!(stored_score(&db, "a"), Some(sentinel));
    assert_eq!(stored_score(&db, "b"), Some(sentinel));

    // The daily refresh rewrites stale rows only, then finds nothing to do.
    assert_eq!(crate::schema::refresh_stale_scores(&db).unwrap(), 1);
    assert_eq!(stored_score(&db, "b"), Some(real));
    assert_eq!(crate::schema::refresh_stale_scores(&db).unwrap(), 0);
}

#[test]
fn contact_score_refresh_runs_at_most_once_a_day() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let now = chrono::Utc::now().timestamp();
    // The migration already refreshed the scores just now.
    assert_eq!(
        crate::schema::refresh_scores_if_due(&db, now + 60).unwrap(),
        None
    );
    seed_lead_contact(&db, "a", "utente@example.test");
    db.execute("UPDATE contacts SET quality_score=-1", [])
        .unwrap();
    assert_eq!(
        crate::schema::refresh_scores_if_due(&db, now + 86_400 + 60).unwrap(),
        Some(1)
    );
    assert_eq!(
        crate::schema::refresh_scores_if_due(&db, now + 86_400 + 120).unwrap(),
        None
    );
    assert!(stored_score(&db, "a").unwrap() >= 0);
}

#[test]
fn contact_score_covers_bulk_inserts_and_filters_lists() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    crate::contacts::suspend_contact_triggers(&db).unwrap();
    seed_lead_contact(&db, "bulk", "bulk@example.test");
    assert_eq!(stored_score(&db, "bulk"), None);
    crate::contacts::resume_contact_triggers(&db).unwrap();
    let bulk = stored_score(&db, "bulk").expect("scored when triggers resume");

    seed_lead_contact(&db, "named", "nome.cognome@example.test");
    db.execute(
        "UPDATE contacts SET person_name='Giulia Bianchi', role='Presidente',
            contact_kind='personale', classification_confidence=1.0,
            independent_source_count=3, last_seen_at=?1
         WHERE id='named'",
        params![sqlite_now()],
    )
    .unwrap();
    let named = stored_score(&db, "named").unwrap();
    assert!(named > bulk);

    let filters = ListFilters {
        min_score: Some(named),
        ..ListFilters::default()
    };
    let list = load_contacts(&db, &filters, 10, 0).unwrap();
    assert_eq!(list.total, 1);
    assert_eq!(list.contacts[0].email, "nome.cognome@example.test");
    assert_eq!(list.contacts[0].quality_score, Some(named));

    // Exports use the same filter.
    let path = std::env::temp_dir().join(format!("score-{}.csv", uuid::Uuid::new_v4()));
    assert_eq!(
        crate::imports::export_contacts_to_path(&db, &filters, &path).unwrap(),
        1
    );
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn contact_quality_score_migration_backfills_from_v59_and_drops_sandbox_scores() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_lead_contact(&db, "pre-upgrade", "utente.sandbox@example.test");
    let expected = stored_score(&db, "pre-upgrade").unwrap();
    db.execute_batch(
        "DROP TRIGGER contacts_score_ai;
         DROP TRIGGER contacts_score_au;
         DROP INDEX idx_contacts_sort_organization; DROP INDEX idx_contacts_sort_category; DROP INDEX idx_contacts_sort_subcategory; DROP INDEX idx_contacts_sort_comune; DROP INDEX idx_contacts_sort_provincia; DROP INDEX idx_contacts_sort_regione; DROP INDEX idx_contacts_sort_pec; DROP INDEX idx_contacts_sort_sources; DROP INDEX idx_contacts_sort_score; DROP INDEX idx_contacts_sort_classification;
         ALTER TABLE contacts DROP COLUMN quality_score;
         ALTER TABLE search_leads ADD COLUMN score INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE search_leads ADD COLUMN factors TEXT NOT NULL DEFAULT '[]';
         CREATE INDEX idx_search_leads_run_score ON search_leads(run_id, score DESC, email_comparison_key);
         DELETE FROM app_settings WHERE key='score_refreshed_at';
         DELETE FROM schema_migrations WHERE version>=60;",
    )
    .unwrap();
    db.pragma_update(None, "user_version", 59).unwrap();

    migrate_database(&mut db, None, false).unwrap();
    assert_eq!(stored_score(&db, "pre-upgrade"), Some(expected));
    let lead_columns = table_columns_for_test(&db, "search_leads");
    assert!(
        !lead_columns
            .iter()
            .any(|column| column == "score" || column == "factors")
    );
    let index: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='idx_search_leads_run_score'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index, 0);
    let contacts: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(contacts, 1);
    // Idempotent: a second run changes nothing.
    db.execute("DELETE FROM schema_migrations WHERE version=60", [])
        .unwrap();
    db.pragma_update(None, "user_version", 59).unwrap();
    migrate_database(&mut db, None, false).unwrap();
    assert_eq!(stored_score(&db, "pre-upgrade"), Some(expected));
}

const DISCOVERY_PAGE_ONE: &str = r#"<html><body>
<h1>Referenti bullismo in Lombardia</h1>
<p>Referente bullismo: Giulia Bianchi, <a href="mailto:giulia.bianchi@example.test">giulia.bianchi@example.test</a></p>
<p>Informazioni: <a href="mailto:info@example.test">info@example.test</a></p>
<p>PEC: <a href="mailto:protocollo@pec.example.test">protocollo@pec.example.test</a></p>
<a href="/contatti">Contatti</a>
<a href="/documenti/piano-bullismo.pdf">Piano bullismo</a>
</body></html>"#;

const DISCOVERY_PAGE_TWO: &str = r#"<html><body>
<h1>Contatti</h1>
<p>Referente bullismo: Giulia Bianchi - giulia.bianchi@example.test</p>
<p>Scrivere a marco.rossi@example.test per informazioni generali.</p>
</body></html>"#;

fn discovery_pages() -> Vec<FetchedPage> {
    vec![
        FetchedPage {
            url: "https://example.test/".to_owned(),
            html: DISCOVERY_PAGE_ONE.to_owned(),
            via: "sito",
        },
        FetchedPage {
            url: "https://example.test/contatti".to_owned(),
            html: DISCOVERY_PAGE_TWO.to_owned(),
            via: "sito",
        },
    ]
}

#[test]
fn search_discovery_migration_creates_tables_fresh_and_upgrades_from_v46() {
    assert_eq!(LATEST_SCHEMA_VERSION, 66);

    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    assert!(has_table(&db, "search_runs").unwrap());
    assert!(has_table(&db, "search_leads").unwrap());
    assert_eq!(table_columns_for_test(&db, "search_runs").len(), 15);
    assert_eq!(table_columns_for_test(&db, "search_leads").len(), 22);
    let index_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_search_leads_run_score'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 0);
    setup_schema(&db).unwrap();
    let runs: i64 = db
        .query_row("SELECT COUNT(*) FROM search_runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs, 0);

    // Upgrade from the previous schema version preserves existing data.
    let mut upgraded = Connection::open_in_memory().unwrap();
    setup_schema(&upgraded).unwrap();
    seed_lead_contact(&upgraded, "pre-upgrade", "utente.sandbox@example.test");
    let contacts_before: i64 = upgraded
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    upgraded
        .execute_batch("DROP TABLE search_leads; DROP TABLE search_runs;")
        .unwrap();
    upgraded.pragma_update(None, "user_version", 46).unwrap();
    upgraded
        .execute("DELETE FROM schema_migrations WHERE version>=47", [])
        .unwrap();
    migrate_database(&mut upgraded, None, false).unwrap();

    assert!(has_table(&upgraded, "search_runs").unwrap());
    assert!(has_table(&upgraded, "search_leads").unwrap());
    let version: i64 = upgraded
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, LATEST_SCHEMA_VERSION);
    let contacts_after: i64 = upgraded
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(contacts_after, contacts_before);
    migrate_database(&mut upgraded, None, false).unwrap();
}

#[test]
fn lead_sandbox_mirror_migration_upgrades_from_v57_keeping_runs_and_leads() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute_batch(
        "DROP TABLE search_seeds;
         DROP INDEX idx_search_leads_comparison_key;
         ALTER TABLE search_runs DROP COLUMN kind;
         ALTER TABLE search_leads DROP COLUMN address;
         ALTER TABLE search_leads DROP COLUMN postal_code;
         ALTER TABLE search_leads DROP COLUMN paese;
         ALTER TABLE search_leads DROP COLUMN method;
         INSERT INTO search_runs (id, created_at, source_id) VALUES ('run-57', '2026-09-28T10:00:00Z', 'indicepa');
         INSERT INTO search_runs (id, created_at) VALUES ('web-57', '2026-09-28T11:00:00Z');
         INSERT INTO search_leads (id, run_id, email, email_comparison_key, retrieved_at, regione)
         VALUES ('lead-57', 'run-57', 'utente@example.test', 'utente@example.test', '2026-09-28T10:00:00Z', 'Lombardia');
         DELETE FROM schema_migrations WHERE version=58;",
    )
    .unwrap();
    db.pragma_update(None, "user_version", 57).unwrap();
    migrate_database(&mut db, None, false).unwrap();
    migrate_database(&mut db, None, false).unwrap();

    assert!(has_table(&db, "search_seeds").unwrap());
    assert_eq!(table_columns_for_test(&db, "search_leads").len(), 22);
    let kinds: Vec<(String, String)> = db
        .prepare("SELECT id, kind FROM search_runs ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        kinds,
        [
            ("run-57".to_owned(), "source".to_owned()),
            ("web-57".to_owned(), "web".to_owned())
        ]
    );
    let list = list_search_leads_page(&db, "run-57", 100, 0).unwrap();
    assert_eq!(list.leads[0].regione.as_deref(), Some("Lombardia"));
    assert_eq!(list.leads[0].address, None);
    assert_eq!(list.leads[0].source_count, 1);
}

#[test]
fn canonical_province_migration_upgrades_from_v58_renaming_english_and_prefixed_names() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for (id, provincia) in [
        ("firenze", "Florence"),
        ("roma", "Roma Capitale"),
        ("trento", "Provincia autonoma di Trento"),
        ("unito", "Milano | Florence"),
        ("estero", "Cantone Ticino"),
    ] {
        seed_lead_contact(&db, id, &format!("{id}@example.test"));
        db.execute(
            "UPDATE contacts SET provincia=?1 WHERE id=?2",
            params![provincia, id],
        )
        .unwrap();
    }
    db.execute_batch(
        "INSERT INTO search_runs (id, created_at) VALUES ('run-58', '2026-09-30T10:00:00Z');
         INSERT INTO search_leads (id, run_id, email, email_comparison_key, retrieved_at, provincia)
         VALUES ('lead-58', 'run-58', 'utente@example.test', 'utente@example.test', '2026-09-30T10:00:00Z', 'Florence');
         DELETE FROM schema_migrations WHERE version=59;",
    )
    .unwrap();
    db.pragma_update(None, "user_version", 58).unwrap();
    migrate_database(&mut db, None, false).unwrap();
    migrate_database(&mut db, None, false).unwrap();

    let provinces: Vec<(String, String)> = db
        .prepare("SELECT id, provincia FROM contacts ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let expected = [
        ("estero", "Cantone Ticino"),
        ("firenze", "Firenze"),
        ("roma", "Roma"),
        ("trento", "Trento"),
        ("unito", "Milano | Florence"),
    ];
    assert_eq!(
        provinces,
        expected.map(|(id, provincia)| (id.to_owned(), provincia.to_owned()))
    );
    let lead: String = db
        .query_row(
            "SELECT provincia FROM search_leads WHERE id='lead-58'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(lead, "Firenze");
}

#[test]
fn lead_sandbox_lists_one_row_per_address_across_runs_with_contact_filters() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let now = "2026-09-22T10:00:00Z";
    let seeds = vec!["https://example.test/".to_owned()];
    let first = persist_search_run(&db, "prima", &seeds, &discovery_pages(), now, 20, &[]).unwrap();
    let second =
        persist_search_run(&db, "seconda", &seeds, &discovery_pages(), now, 20, &[]).unwrap();
    db.execute(
        "UPDATE search_leads SET category='Istruzione', regione='Lombardia' WHERE run_id=?1 AND email LIKE 'info@%'",
        [&second.run_id],
    )
    .unwrap();
    let all = crate::discovery::list_sandbox_leads(
        &db,
        &crate::discovery::LeadFilters::default(),
        100,
        0,
    )
    .unwrap();
    assert_eq!(all.total, 4, "same addresses from two runs are listed once");
    assert!(
        all.leads
            .iter()
            .all(|lead| lead.source_count == 2 && !lead.in_database)
    );
    let filter = |category: &str| crate::discovery::LeadFilters {
        category: crate::FieldFilter {
            include: vec![category.to_owned()],
            exclude: Vec::new(),
        },
        pec: crate::FieldFilter {
            include: vec!["standard".to_owned()],
            exclude: Vec::new(),
        },
        ..crate::discovery::LeadFilters::default()
    };
    let schools = crate::discovery::list_sandbox_leads(&db, &filter("Istruzione"), 100, 0).unwrap();
    assert_eq!(schools.total, 1);
    assert_eq!(schools.leads[0].run_id, second.run_id);
    assert_eq!(schools.leads[0].run_label.as_deref(), Some("seconda"));
    let search = crate::discovery::LeadFilters {
        query: "GIULIA".to_owned(),
        ..crate::discovery::LeadFilters::default()
    };
    assert_eq!(
        crate::discovery::list_sandbox_leads(&db, &search, 100, 0)
            .unwrap()
            .total,
        1
    );
    let web_only = crate::discovery::LeadFilters {
        kind: crate::FieldFilter {
            include: vec!["source".to_owned()],
            exclude: Vec::new(),
        },
        ..crate::discovery::LeadFilters::default()
    };
    assert_eq!(
        crate::discovery::list_sandbox_leads(&db, &web_only, 100, 0)
            .unwrap()
            .total,
        0
    );
    let options = crate::discovery::lead_filter_options(&db, &[]).unwrap();
    assert!(
        crate::discovery::lead_filter_options(&db, &["source".to_owned()])
            .unwrap()
            .run
            .is_empty()
    );
    assert_eq!(options.run.len(), 2);
    assert_eq!(options.category[0].value, "Istruzione");
    assert!(
        options
            .stato
            .iter()
            .any(|option| option.value == "nuovo" && option.count == 4)
    );
    crate::discovery::delete_run(&db, &first.run_id).unwrap();
    let after = crate::discovery::list_sandbox_leads(
        &db,
        &crate::discovery::LeadFilters::default(),
        100,
        0,
    )
    .unwrap();
    assert!(after.leads.iter().all(|lead| lead.source_count == 1));
}

#[test]
fn lead_promotion_fills_empty_contact_fields_without_overwriting_known_ones() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_lead_contact(&db, "esistente", "sezione.bergamo@example.test");
    // Known category from the AI, no ente, no place, a blank (not NULL) role.
    db.execute_batch(
        "UPDATE contacts SET organization_name=NULL, role='  ', comune=NULL, provincia=NULL,
             regione=NULL, website='https://noto.example.test', classification_method='ai',
             category='Associazioni', subcategory='Alpini',
             email_comparison_key='sezione.bergamo@example.test'
         WHERE email='sezione.bergamo@example.test'",
    )
    .unwrap();
    let mut lead = crate::discovery::LeadAccumulator::from_source(
        "sezione.bergamo@example.test".to_owned(),
        false,
        "https://www.ana.example.test/",
        "2026-09-30T10:00:00Z",
        Some("Segreteria".to_owned()),
        None,
        crate::discovery::LeadContext {
            category: Some("Associazioni".to_owned()),
            subcategory: Some("Alpini".to_owned()),
            organization: Some("Sezione ANA Bergamo".to_owned()),
            comune: Some("Bergamo".to_owned()),
            provincia: Some("Bergamo".to_owned()),
            regione: Some("Lombardia".to_owned()),
            website: Some("https://www.anabergamo.example.test".to_owned()),
            ..Default::default()
        },
    );
    lead.method = "open_data".to_owned();
    let run = crate::discovery::insert_run(
        &db,
        &crate::discovery::RunRecord {
            kind: "source",
            query_text: "Fonte: ANA",
            source_id: Some("ana-sezioni"),
            seed_count: 0,
            now: "2026-09-30T10:00:00Z",
        },
        0,
        0,
        &[],
        [lead],
    )
    .unwrap();
    let listed = list_search_leads_page(&db, &run.run_id, 100, 0).unwrap();
    assert_eq!(listed.leads[0].stato, "completa");
    assert!(listed.leads[0].in_database);

    let report =
        crate::discovery::promote_leads_inner(&db, &crate::discovery::LeadFilters::default())
            .unwrap();
    assert_eq!((report.added, report.updated), (0, 1));
    let (organization, role, comune, regione, website): (String, String, String, String, String) =
        db.query_row(
            "SELECT organization_name, role, comune, regione, website FROM contacts
             WHERE email='sezione.bergamo@example.test'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(organization, "Sezione ANA Bergamo");
    assert_eq!(role, "Segreteria");
    assert_eq!(
        (comune.as_str(), regione.as_str()),
        ("Bergamo", "Lombardia")
    );
    // Known values stay first; a different site is appended like any import merge.
    assert!(
        website.starts_with("https://noto.example.test"),
        "{website}"
    );
    let after = list_search_leads_page(&db, &run.run_id, 100, 0).unwrap();
    assert!(after.leads.is_empty(), "promoted leads leave the sandbox");
}

#[test]
fn lead_promotion_keeps_known_status_and_collapses_repeated_imports() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_lead_contact(&db, "noto", "sezione@example.test");
    db.execute_batch(
        "UPDATE contacts SET status='valid', routing_status='routing_ok',
             email_comparison_key=email WHERE email='sezione@example.test'",
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO domain_verification_cache(domain,routing_status,mx_hosts_json,ttl_seconds,checked_at,details_json)
         VALUES ('instradabile.test','routing_ok','[]',0,'2020-01-01T00:00:00Z','{}')",
    )
    .unwrap();
    let context = || crate::discovery::LeadContext {
        category: Some("Associazioni".to_owned()),
        subcategory: Some("Alpini".to_owned()),
        ..Default::default()
    };
    // The same source imported twice leaves two identical leads per address.
    for _ in 0..2 {
        for email in [
            "sezione@example.test",
            "nuovo@instradabile.test",
            "ignoto@sconosciuto.test",
        ] {
            insert_source_lead(&db, "ana-sezioni", email, context());
        }
    }
    let report =
        crate::discovery::promote_leads_inner(&db, &crate::discovery::LeadFilters::default())
            .unwrap();
    assert_eq!((report.added, report.updated, report.unverified), (2, 1, 1));
    let status = |email: &str| -> (String, i64) {
        db.query_row(
            "SELECT status, is_exportable FROM contacts WHERE email=?1",
            [email],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(status("sezione@example.test"), ("valid".to_owned(), 1));
    assert_eq!(status("nuovo@instradabile.test"), ("valid".to_owned(), 1));
    assert_eq!(
        status("ignoto@sconosciuto.test"),
        ("domain_unverified".to_owned(), 0)
    );
    let left: i64 = db
        .query_row("SELECT COUNT(*) FROM search_leads", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn promoted_contact_status_migration_restores_routable_contacts() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for email in [
        "perso@instradabile.test",
        "nuovo@instradabile.test",
        "ignoto@sconosciuto.test",
    ] {
        seed_lead_contact(&db, email, email);
    }
    db.execute_batch(
        "INSERT INTO domain_verification_cache(domain,routing_status,mx_hosts_json,ttl_seconds,checked_at,details_json)
         VALUES ('instradabile.test','routing_ok','[]',0,'2020-01-01T00:00:00Z','{}');
         UPDATE contacts SET status='domain_unverified', routing_status='unknown';
         UPDATE contacts SET routing_status='routing_ok' WHERE email='perso@instradabile.test'",
    )
    .unwrap();
    crate::schema::migrate_promoted_contact_status(&db).unwrap();
    let valid: Vec<String> = db
        .prepare(
            "SELECT email FROM contacts WHERE status='valid' AND is_exportable=1 ORDER BY email",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        valid,
        ["nuovo@instradabile.test", "perso@instradabile.test"]
    );
}

/// One `source` run with one lead, as `store_source_import` writes it.
fn insert_source_lead(
    db: &Connection,
    source_id: &str,
    email: &str,
    context: crate::discovery::LeadContext,
) -> String {
    let mut lead = crate::discovery::LeadAccumulator::from_source(
        email.to_owned(),
        false,
        "https://registro.example.test/",
        "2026-09-30T10:00:00Z",
        None,
        None,
        context,
    );
    lead.method = "open_data".to_owned();
    crate::discovery::insert_run(
        db,
        &crate::discovery::RunRecord {
            kind: "source",
            query_text: "Fonte",
            source_id: Some(source_id),
            seed_count: 0,
            now: "2026-09-30T10:00:00Z",
        },
        0,
        0,
        &[],
        [lead],
    )
    .unwrap()
    .run_id
}

#[test]
fn lead_promotion_lets_official_registries_replace_ente_code_and_place_only() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for email in ["scuola@example.test", "sezione@example.test"] {
        seed_lead_contact(&db, email, email);
    }
    db.execute_batch(
        "UPDATE contacts SET organization_name='Vecchio nome', official_code='OLD1', comune='Milano',
             provincia='Milano', regione='Lombardia', role='Segreteria', address=NULL, postal_code=NULL,
             classification_evidence='[]', email_comparison_key=email",
    )
    .unwrap();
    let place = |comune: &str, provincia: &str| crate::discovery::LeadContext {
        organization: Some("Nome ufficiale".to_owned()),
        official_code: Some("BGIC000001".to_owned()),
        comune: Some(comune.to_owned()),
        provincia: Some(provincia.to_owned()),
        regione: Some("Lombardia".to_owned()),
        address: Some("Via Roma 1".to_owned()),
        postal_code: Some("24100".to_owned()),
        ..Default::default()
    };
    let official = insert_source_lead(
        &db,
        "miur-statali",
        "scuola@example.test",
        place("Bergamo", "Bergamo"),
    );
    let other = insert_source_lead(
        &db,
        "ana-sezioni",
        "sezione@example.test",
        place("Bergamo", "Bergamo"),
    );
    // The registry changes the contact; the other source has nothing to fill.
    let stato = |run: &str| {
        list_search_leads_page(&db, run, 100, 0).unwrap().leads[0]
            .stato
            .clone()
    };
    assert_eq!(stato(&official), "completa");
    assert_eq!(stato(&other), "nel_database");

    crate::discovery::promote_leads_inner(&db, &crate::discovery::LeadFilters::default()).unwrap();
    let read =
        |email: &str| -> (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ) {
            db.query_row(
            "SELECT organization_name, official_code, comune, regione, role, address, postal_code,
                    classification_evidence
             FROM contacts WHERE email=?1",
            [email],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?,
                ))
            },
        )
        .unwrap()
        };
    let school = read("scuola@example.test");
    assert_eq!(
        (
            school.0.as_str(),
            school.1.as_str(),
            school.2.as_str(),
            school.3.as_str(),
            school.4.as_str()
        ),
        (
            "Nome ufficiale",
            "BGIC000001",
            "Bergamo",
            "Lombardia",
            "Segreteria"
        )
    );
    assert!(
        school.7.contains(
            "sostituito da miur-statali: ente: Vecchio nome; codice: OLD1; comune: Milano"
        ),
        "{}",
        school.7
    );
    // Address and CAP only resolve the place: they stay in the sandbox.
    assert_eq!((school.5, school.6), (None, None));
    let section = read("sezione@example.test");
    assert_eq!(
        (section.0.as_str(), section.1.as_str(), section.2.as_str()),
        ("Vecchio nome", "OLD1", "Milano")
    );
    assert!(
        list_search_leads_page(&db, &official, 100, 0)
            .unwrap()
            .leads
            .is_empty()
    );
}

#[test]
fn lead_source_catalog_migration_upgrades_from_v47_keeping_sandbox_rows() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute_batch(
        "ALTER TABLE search_runs DROP COLUMN source_id;
         ALTER TABLE search_leads DROP COLUMN category;
         ALTER TABLE search_leads DROP COLUMN subcategory;
         ALTER TABLE search_leads DROP COLUMN organization;
         ALTER TABLE search_leads DROP COLUMN comune;
         ALTER TABLE search_leads DROP COLUMN provincia;
         ALTER TABLE search_leads DROP COLUMN regione;
         ALTER TABLE search_leads DROP COLUMN website;
         INSERT INTO search_runs (id, created_at) VALUES ('run-47', '2026-09-22T10:00:00Z');
         INSERT INTO search_leads (id, run_id, email, email_comparison_key, retrieved_at)
         VALUES ('lead-47', 'run-47', 'utente@example.test', 'utente@example.test', '2026-09-22T10:00:00Z');
         DELETE FROM schema_migrations WHERE version=48;",
    )
    .unwrap();
    db.pragma_update(None, "user_version", 47).unwrap();
    migrate_database(&mut db, None, false).unwrap();

    assert_eq!(table_columns_for_test(&db, "search_runs").len(), 15);
    assert_eq!(table_columns_for_test(&db, "search_leads").len(), 22);
    let (email, regione): (String, Option<String>) = db
        .query_row(
            "SELECT email, regione FROM search_leads WHERE id='lead-47'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(email, "utente@example.test");
    assert_eq!(regione, None);
    migrate_database(&mut db, None, false).unwrap();
}

#[test]
fn upgrade_from_v49_drops_compliance_columns_and_per_candidate_dns_copies() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_lead_contact(&db, "c-1", "utente@example.test");
    db.execute_batch(
        "ALTER TABLE search_leads DROP COLUMN official_code;
         ALTER TABLE search_leads DROP COLUMN department;
         ALTER TABLE contacts ADD COLUMN permitted_purpose TEXT;
         ALTER TABLE contacts ADD COLUMN lawful_basis TEXT;
         ALTER TABLE contacts ADD COLUMN consent_provenance TEXT;
         ALTER TABLE email_candidates ADD COLUMN lawful_basis TEXT;
         UPDATE contacts SET lawful_basis='x' WHERE id='c-1';
         INSERT INTO verification_events(id,comparison_key,method,checked_at) VALUES
           ('e-1','utente@example.test','dns_cache','now'),
           ('e-2','utente@example.test','suppression','now'),
           ('e-3','utente@example.test','dns_revalidation','now');
         DELETE FROM schema_migrations WHERE version>=50;",
    )
    .unwrap();
    db.pragma_update(None, "user_version", 49).unwrap();
    migrate_database(&mut db, None, false).unwrap();
    migrate_database(&mut db, None, false).unwrap();

    for table in ["contacts", "email_candidates"] {
        let columns = table_columns_for_test(&db, table);
        for column in ["permitted_purpose", "lawful_basis", "consent_provenance"] {
            assert!(
                !columns.iter().any(|name| name == column),
                "{table}.{column}"
            );
        }
    }
    let lead_columns = table_columns_for_test(&db, "search_leads");
    assert!(lead_columns.iter().any(|name| name == "official_code"));
    assert!(lead_columns.iter().any(|name| name == "department"));
    let methods: Vec<String> = db
        .prepare("SELECT method FROM verification_events ORDER BY method")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(methods, ["dns_revalidation", "suppression"]);
    let contacts: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(contacts, 1);
}

#[test]
fn pec_migration_promotes_mx_hosts_and_drops_email_kind() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    // Rebuild the pre-migration shape: email_kind still present.
    if !table_columns_for_test(&db, "contacts")
        .iter()
        .any(|name| name == "email_kind")
    {
        db.execute("ALTER TABLE contacts ADD COLUMN email_kind TEXT", [])
            .unwrap();
    }
    for (id, email, kind, pec, mx) in [
        (
            "p-pattern",
            "ufficio@pec.example.test",
            "standard",
            0_i64,
            "[]",
        ),
        (
            "p-mx",
            "info@comune.example.test",
            "standard",
            0,
            r#"["mail.asmecal.it"]"#,
        ),
        (
            "p-mixed",
            "segreteria@example.test",
            "standard | pec",
            0,
            r#"["mx.pec.aruba.it"]"#,
        ),
        (
            "p-ordinary",
            "info@theatre.example.test",
            "standard",
            0,
            r#"["in.arubabusiness.it"]"#,
        ),
        ("p-already", "protocollo@legalmail.it", "pec", 1, "[]"),
    ] {
        seed_lead_contact(&db, id, email);
        db.execute(
            "UPDATE contacts SET email_kind=?1, is_pec=?2 WHERE id=?3",
            params![kind, pec, id],
        )
        .unwrap();
        let domain = email.split('@').nth(1).unwrap();
        db.execute(
            "INSERT OR REPLACE INTO domain_verification_cache(
                domain,routing_status,mx_hosts_json,checked_at,expires_at
             ) VALUES (?1,'routing_ok',?2,'now','9999-01-01')",
            params![domain, mx],
        )
        .unwrap();
    }
    db.pragma_update(None, "user_version", 52).unwrap();
    db.execute("DELETE FROM schema_migrations WHERE version>=53", [])
        .unwrap();
    migrate_database(&mut db, None, false).unwrap();
    migrate_database(&mut db, None, false).unwrap();

    let columns = table_columns_for_test(&db, "contacts");
    assert!(!columns.iter().any(|name| name == "email_kind"));
    let flags: Vec<(String, i64)> = db
        .prepare("SELECT id, is_pec FROM contacts ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        flags,
        [
            ("p-already".into(), 1),
            ("p-mixed".into(), 1),
            ("p-mx".into(), 1),
            ("p-ordinary".into(), 0),
            ("p-pattern".into(), 1),
        ]
    );
}

fn macroareas(db: &Connection) -> Vec<(String, Option<String>, Option<String>)> {
    db.prepare("SELECT id, regione, macroarea FROM contacts ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn assert_fts_intact(db: &Connection) {
    db.execute_batch("INSERT INTO contacts_fts(contacts_fts) VALUES('integrity-check');")
        .unwrap();
}

#[test]
fn macroarea_follows_regione_on_every_write() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_lead_contact(&db, "m-1", "utente@example.test");
    let set_region = |region: Option<&str>| {
        db.execute("UPDATE contacts SET regione=?1 WHERE id='m-1'", [region])
            .unwrap();
        macroareas(&db)[0].2.clone()
    };
    assert_eq!(set_region(Some("Lombardia")).as_deref(), Some("Nord-Ovest"));
    assert_eq!(set_region(Some("Lazio")).as_deref(), Some("Centro"));
    assert_eq!(
        set_region(Some(" Emilia-Romagna")).as_deref(),
        Some("Nord-Est")
    );
    assert_eq!(
        set_region(Some("Valle d'Aosta")).as_deref(),
        Some("Nord-Ovest")
    );
    assert_eq!(set_region(Some("Lazio | Umbria")), None);
    assert_eq!(set_region(None), None);
    set_region(Some("Sicilia"));
    let hits: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM contacts_fts WHERE contacts_fts MATCH 'macroarea:isole'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1);
    assert_fts_intact(&db);
    assert!(
        db.execute("UPDATE contacts SET macroarea='Centro' WHERE id='m-1'", [])
            .is_err()
    );
}

#[test]
fn macroarea_migration_upgrades_from_v48_and_fixes_stored_values() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    // Rebuild the v48 shape: a plain, writable macroarea column.
    let triggers: Vec<(String, String)> = db
        .prepare("SELECT name, sql FROM sqlite_master WHERE type='trigger' AND name LIKE 'contacts_fts_%'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (name, _) in &triggers {
        db.execute_batch(&format!("DROP TRIGGER {name};")).unwrap();
    }
    db.execute_batch(
        "DROP INDEX idx_contacts_macroarea;
         ALTER TABLE contacts DROP COLUMN macroarea;
         ALTER TABLE contacts ADD COLUMN macroarea TEXT;
         CREATE INDEX idx_contacts_macroarea ON contacts(macroarea);",
    )
    .unwrap();
    for (_, sql) in &triggers {
        db.execute_batch(sql).unwrap();
    }
    for (id, region, stored) in [
        ("m-1", Some("Lombardia"), Some("Centro")),
        ("m-2", Some("Marche"), None),
        ("m-3", Some("Lazio | Umbria"), None),
        ("m-4", None, None),
    ] {
        seed_lead_contact(&db, id, &format!("{id}@example.test"));
        db.execute(
            "UPDATE contacts SET regione=?1, macroarea=?2 WHERE id=?3",
            rusqlite::params![region, stored, id],
        )
        .unwrap();
    }
    db.pragma_update(None, "user_version", 48).unwrap();
    db.execute("DELETE FROM schema_migrations WHERE version=49", [])
        .unwrap();
    migrate_database(&mut db, None, false).unwrap();

    let rows = macroareas(&db);
    let got: Vec<Option<&str>> = rows.iter().map(|row| row.2.as_deref()).collect();
    assert_eq!(got, [Some("Nord-Ovest"), Some("Centro"), None, None]);
    assert_eq!(rows[2].1.as_deref(), Some("Lazio | Umbria"));
    assert_fts_intact(&db);
    migrate_database(&mut db, None, false).unwrap();
    assert_eq!(macroareas(&db).len(), 4);
}

fn table_columns_for_test(db: &Connection, table: &str) -> Vec<String> {
    let mut statement = db.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn search_discovery_run_list_export_round_trip_with_fixture_html() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let now = "2026-09-22T10:00:00Z";
    let seeds = vec!["https://example.test/".to_owned()];
    let summary = persist_search_run(
        &db,
        "referenti bullismo",
        &seeds,
        &discovery_pages(),
        now,
        20,
        &[],
    )
    .unwrap();
    assert_eq!(summary.lead_count, 4);
    assert_eq!(summary.page_count, 2);
    assert_eq!(summary.pdf_found, 1);
    let runs = crate::discovery::list_runs(&db).unwrap();
    assert_eq!(runs[0].id, summary.run_id);
    assert_eq!(
        (runs[0].kind.as_str(), runs[0].status.as_str()),
        ("web", "completed")
    );
    let run_filter = || crate::discovery::LeadFilters {
        run: crate::FieldFilter {
            include: vec![summary.run_id.clone()],
            exclude: Vec::new(),
        },
        ..crate::discovery::LeadFilters::default()
    };

    let list = list_search_leads_page(&db, &summary.run_id, 100, 0).unwrap();
    assert_eq!(list.total, 4);
    assert_eq!(list.leads.len(), 4);
    assert_eq!(list.leads[0].email, "giulia.bianchi@example.test");
    assert_eq!(list.leads[0].person_name.as_deref(), Some("Giulia Bianchi"));
    assert_eq!(list.leads[0].role.as_deref(), Some("Referente bullismo"));
    assert!(!list.leads[0].is_pec);
    assert_eq!(
        list.leads[0].source_url.as_deref(),
        Some("https://example.test/")
    );
    assert_eq!(list.leads[1].email, "info@example.test");
    assert_eq!(list.leads[2].email, "marco.rossi@example.test");
    assert_eq!(list.leads[3].email, "protocollo@pec.example.test");
    assert!(list.leads[3].is_pec);

    // Leads list by address; pagination caps at MAX_PAGE_SIZE.
    let first_page = list_search_leads_page(&db, &summary.run_id, 2, 0).unwrap();
    assert_eq!(first_page.limit, 2);
    assert!(!first_page.has_previous);
    assert!(first_page.has_next);
    assert_eq!(first_page.leads.len(), 2);
    let second_page = list_search_leads_page(&db, &summary.run_id, 2, 2).unwrap();
    assert!(second_page.has_previous);
    assert!(!second_page.has_next);
    assert_eq!(second_page.leads.len(), 2);
    let clamped = list_search_leads_page(&db, &summary.run_id, 1000, 0).unwrap();
    assert_eq!(clamped.limit, 100);

    // Export de-duplicates, drops PEC, sorts by lower(email) and refuses to
    // overwrite an existing file.
    let dir = std::env::temp_dir().join(format!("discovery-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("lead.txt");
    assert_eq!(
        crate::discovery::export_leads_txt_to_path(&db, &run_filter(), &target).unwrap(),
        3
    );
    let content = std::fs::read_to_string(&target).unwrap();
    assert_eq!(
        content,
        "giulia.bianchi@example.test\ninfo@example.test\nmarco.rossi@example.test\n"
    );
    assert!(!content.contains("protocollo@pec.example.test"));
    let error =
        crate::discovery::export_leads_txt_to_path(&db, &run_filter(), &target).unwrap_err();
    assert!(error.contains("esiste già"));
    std::fs::remove_dir_all(&dir).unwrap();

    // Promotion copies every lead, PEC included (flagged), like a contact import.
    let promoted = crate::discovery::promote_leads_inner(&db, &run_filter()).unwrap();
    assert_eq!(promoted.added, 4);
    assert_eq!(promoted.updated, 0);
    let unscored: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM contacts WHERE quality_score IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unscored, 0, "promoted leads are scored as contacts");
    assert_eq!(promoted.skipped_blacklist, 0);
    let contacts: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(contacts, 4);
    let pecs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM contacts WHERE email LIKE '%pec.%' AND is_pec=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pecs, 1);
    let listed = list_search_leads_page(&db, &summary.run_id, 100, 0).unwrap();
    assert!(listed.leads.is_empty(), "promoted leads leave the sandbox");
    let again = crate::discovery::promote_leads_inner(&db, &run_filter()).unwrap();
    assert_eq!((again.added, again.updated), (0, 0));
    let new_only = crate::discovery::LeadFilters {
        stato: crate::FieldFilter {
            include: vec!["nuovo".to_owned()],
            exclude: Vec::new(),
        },
        ..crate::discovery::LeadFilters::default()
    };
    assert_eq!(
        crate::discovery::list_sandbox_leads(&db, &new_only, 100, 0)
            .unwrap()
            .total,
        0
    );
}

#[test]
fn search_discovery_leaves_contacts_and_blacklist_untouched() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let contact = seed_lead_contact(&db, "sandbox-contact", "utente.sandbox@example.test");
    db.execute(
        "INSERT INTO blacklist(normalized_email,source_file,imported_at,note)
         VALUES('bloccato@example.test','test.txt','now',NULL)",
        [],
    )
    .unwrap();
    let contacts_before: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    let blacklist_before: i64 = db
        .query_row("SELECT COUNT(*) FROM blacklist", [], |row| row.get(0))
        .unwrap();

    let seeds = vec!["https://example.test/".to_owned()];
    let summary = persist_search_run(
        &db,
        "referenti bullismo",
        &seeds,
        &discovery_pages(),
        "2026-09-22T10:00:00Z",
        20,
        &[],
    )
    .unwrap();
    assert!(summary.lead_count > 0);

    let contacts_after: i64 = db
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap();
    let blacklist_after: i64 = db
        .query_row("SELECT COUNT(*) FROM blacklist", [], |row| row.get(0))
        .unwrap();
    assert_eq!(contacts_after, contacts_before);
    assert_eq!(blacklist_after, blacklist_before);
    let stored_email: String = db
        .query_row(
            "SELECT email FROM contacts WHERE id='sandbox-contact'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_email, contact.email);
}

#[test]
fn search_discovery_extractor_reads_mailto_and_ignores_invalid() {
    let html = r#"<html><body>
      Referente bullismo: Giulia Bianchi <a href="mailto:giulia.bianchi@example.test">scrivici</a>
      Non valido: non-una-email
      PEC: protocollo@pec.example.test
      <a href="/x/documento.pdf">Pdf</a>
    </body></html>"#;
    let parsed = parse_page("https://example.test/", html);
    let emails: Vec<&str> = parsed.emails.iter().map(|hit| hit.email.as_str()).collect();
    assert!(emails.contains(&"giulia.bianchi@example.test"));
    assert!(emails.contains(&"protocollo@pec.example.test"));
    assert!(!emails.contains(&"non-una-email"));
    assert_eq!(
        parsed.pdf_urls,
        vec!["https://example.test/x/documento.pdf".to_owned()]
    );
    let giulia = parsed
        .emails
        .iter()
        .find(|hit| hit.email == "giulia.bianchi@example.test")
        .unwrap();
    assert_eq!(
        context_role(&giulia.context).as_deref(),
        Some("Referente bullismo")
    );
    assert_eq!(
        context_person_name(&giulia.context).as_deref(),
        Some("Giulia Bianchi")
    );
}

#[test]
fn search_discovery_validate_seeds_rejects_empty_invalid_and_oversized() {
    let empty_error = validate_seeds(&[]).unwrap_err();
    assert!(empty_error.contains("almeno un URL"), "{empty_error}");
    let blank_error = validate_seeds(&["   ".to_owned(), String::new()]).unwrap_err();
    assert!(blank_error.contains("almeno un URL"), "{blank_error}");

    let too_many: Vec<String> = (0..=MAX_SEEDS)
        .map(|index| format!("https://site-{index}.example.test/"))
        .collect();
    let too_many_error = validate_seeds(&too_many).unwrap_err();
    assert!(
        too_many_error.contains(&format!("Massimo {MAX_SEEDS} URL")),
        "{too_many_error}"
    );

    for seed in [
        "ftp://example.test/",
        "file:///tmp/utente@example.test",
        "example.test",
        "utente@example.test",
    ] {
        let error = validate_seeds(&[seed.to_owned()]).unwrap_err();
        assert!(error.contains("solo URL http o https"), "{seed}: {error}");
    }

    for seed in ["http://", "https:///percorso"] {
        let error = validate_seeds(&[seed.to_owned()]).unwrap_err();
        assert!(error.contains("URL non valido"), "{seed}: {error}");
    }

    // Internal addresses never reach context.dev.
    for seed in ["http://localhost:8080/contatti", "https://192.168.1.10/"] {
        let error = validate_seeds(&[seed.to_owned()]).unwrap_err();
        assert!(error.contains("non pubblico"), "{seed}: {error}");
    }
}

#[test]
fn search_discovery_validate_seeds_accepts_and_trims_http_seeds() {
    let seeds = validate_seeds(&[
        " https://prima.example.test/ ".to_owned(),
        "http://terza.example.test:8080/contatti".to_owned(),
        "https://seconda.example.test".to_owned(),
    ])
    .unwrap();
    assert_eq!(
        seeds,
        vec![
            "https://prima.example.test/".to_owned(),
            "http://terza.example.test:8080/contatti".to_owned(),
            "https://seconda.example.test".to_owned(),
        ]
    );
}

#[test]
fn search_discovery_run_limits_clamp_to_documented_bounds() {
    assert_eq!(
        clamp_run_limits(None, None),
        (DEFAULT_MAX_PAGES_PER_DOMAIN, DEFAULT_MAX_PDFS)
    );
    assert_eq!(clamp_run_limits(Some(0), Some(-1)), (1, 0));
    assert_eq!(clamp_run_limits(Some(4), Some(1)), (4, 1));
    assert_eq!(
        clamp_run_limits(Some(1_000), Some(1_000)),
        (MAX_PAGES_PER_DOMAIN, MAX_PDF_LINKS)
    );
}

#[test]
fn tauri_commands_touching_state_never_run_on_the_ui_thread() {
    // Tauri 2 runs synchronous commands on the main thread, freezing the window.
    let pattern = regex::Regex::new(
        r"#\[tauri::command\]\s*\n(?:\s*#\[[^\n]*\]\n)*\s*(?:pub(?:\(crate\))? )?(async )?fn (\w+)",
    )
    .unwrap();
    let allowed = [
        "control_import",
        "list_lead_directories",
        "cancel_lead_crawl",
    ];
    let mut sync_commands = Vec::new();
    let mut pending = vec![std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src"
    ))];
    let mut scanned = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for capture in pattern.captures_iter(&source) {
                scanned += 1;
                if capture.get(1).is_none() && !allowed.contains(&&capture[2]) {
                    sync_commands.push(capture[2].to_owned());
                }
            }
        }
    }
    assert!(scanned > 60, "found only {scanned} commands");
    assert!(sync_commands.is_empty(), "sync commands: {sync_commands:?}");
}

#[test]
fn cancelled_ai_run_release_wait_stops_on_expired_lease() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO ai_classification_runs(
            id,model,profile,prompt_version,status,total,started_at,owner_session,lease_expires_at
         ) VALUES('run-dead','m','p','v1','cancelled',1,'now','gone-session',1)",
        [],
    )
    .unwrap();
    let started = std::time::Instant::now();
    crate::ai_runs::wait_for_ai_run_release(&db, "run-dead").expect("expired lease releases");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn pending_review_list_is_bounded_but_count_is_exact() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO contacts(
            id,email,email_local_part,email_domain,email_tld,status,validation_notes,source_file,category
         ) VALUES ('contact-1','utente@example.test','utente','example','.test','valid','[]','f.txt','Varie')",
        [],
    )
    .unwrap();
    for index in 0..(crate::MAX_PAGE_SIZE + 1) {
        db.execute(
            "INSERT INTO classification_reviews(
                id,contact_id,current_category,proposed_category,confidence,evidence,model,profile,
                prompt_version,status,created_at,outcome
             ) VALUES (?1,'contact-1','Varie','Scuole',0.7,'[]','m','p','v','pending',?2,'taxonomy_gap')",
            params![format!("review-{index:03}"), format!("2026-01-01T00:00:{index:03}")],
        )
        .unwrap();
    }
    let reviews = crate::ai_runs::load_classification_reviews(&db, "pending").unwrap();
    assert_eq!(reviews.len(), crate::MAX_PAGE_SIZE);
    assert_eq!(reviews[0].id, "review-000");
    assert_eq!(
        crate::count_classification_reviews(&db, "pending").unwrap(),
        crate::MAX_PAGE_SIZE + 1
    );
}

fn exportable_flags(db: &Connection) -> BTreeMap<String, i64> {
    let mut statement = db
        .prepare("SELECT email,is_exportable FROM contacts")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    rows.map(Result::unwrap).collect()
}

fn insert_plain_contact(db: &Connection, id: &str, email: &str, comparison_key: Option<&str>) {
    let (local, domain) = email.split_once('@').unwrap();
    db.execute(
        "INSERT INTO contacts(
            id,email,email_local_part,email_domain,email_tld,status,validation_notes,
            source_file,category,email_comparison_key
         ) VALUES (?1,?2,?3,?4,'.test','valid','[]','f.txt','Varie',?5)",
        params![id, email, local, domain, comparison_key],
    )
    .unwrap();
}

#[test]
fn exportable_flag_follows_contacts_blacklist_and_suppressions() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_plain_contact(
        &db,
        "c-1",
        "utente@example.test",
        Some("utente@example.test"),
    );
    insert_plain_contact(
        &db,
        "c-2",
        "noreply@example.test",
        Some("noreply@example.test"),
    );
    insert_plain_contact(&db, "c-3", "noreply.team@example.test", None);
    insert_plain_contact(&db, "c-4", "noreplyer@example.test", None);
    insert_plain_contact(&db, "c-5", "Legacy@Example.test", None);
    let flag = |email: &str| exportable_flags(&db)[email];
    assert_eq!(flag("utente@example.test"), 1);
    assert_eq!(flag("noreply@example.test"), 0);
    assert_eq!(flag("noreply.team@example.test"), 0);
    assert_eq!(
        flag("noreplyer@example.test"),
        1,
        "a longer word is a person"
    );

    db.execute(
        "INSERT INTO blacklist(normalized_email,source_file,imported_at) VALUES('UTENTE@example.test','b','now')",
        [],
    )
    .unwrap();
    assert_eq!(flag("utente@example.test"), 0);
    db.execute("DELETE FROM blacklist", []).unwrap();
    assert_eq!(flag("utente@example.test"), 1);

    db.execute(
        "INSERT INTO suppressions(comparison_key,reason,created_at) VALUES('legacy@example.test','manual','now')",
        [],
    )
    .unwrap();
    assert_eq!(flag("Legacy@Example.test"), 0, "legacy rows match on email");
    db.execute("UPDATE suppressions SET lifted_at='later'", [])
        .unwrap();
    assert_eq!(flag("Legacy@Example.test"), 1);

    db.execute(
        "UPDATE contacts SET status='domain_missing_mx' WHERE id='c-1'",
        [],
    )
    .unwrap();
    assert_eq!(flag("utente@example.test"), 0);
    db.execute(
        "UPDATE contacts SET status='valid',suppression_status='suppressed' WHERE id='c-1'",
        [],
    )
    .unwrap();
    assert_eq!(flag("utente@example.test"), 0);
    db.execute(
        "UPDATE contacts SET suppression_status='active' WHERE id='c-1'",
        [],
    )
    .unwrap();
    assert_eq!(flag("utente@example.test"), 1);

    // Lists and facets read the same projection.
    let page = load_contacts_page(&db, &ListFilters::default(), 100, 0, true).unwrap();
    assert_eq!(page.total, 3);
    let facet_total: usize = crate::contacts::facet_values(&db, "category")
        .unwrap()
        .iter()
        .map(|option| option.count)
        .sum();
    assert_eq!(facet_total, 3);
}

#[test]
fn exportable_projection_migration_backfills_a_v53_database() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute_batch(
        "DROP TRIGGER contacts_exportable_ai; DROP TRIGGER contacts_exportable_au;
         DROP TRIGGER blacklist_exportable_ai; DROP TRIGGER blacklist_exportable_ad;
         DROP TRIGGER blacklist_exportable_au; DROP TRIGGER suppressions_exportable_ai;
         DROP TRIGGER suppressions_exportable_ad; DROP TRIGGER suppressions_exportable_au;
         DROP INDEX idx_contacts_list_sort; DROP INDEX idx_contacts_list_category;
         DROP INDEX idx_contacts_list_pec; DROP INDEX idx_contacts_email_nocase;
         DROP INDEX idx_contacts_comparison_key_nocase;
         DROP INDEX idx_contacts_sort_organization; DROP INDEX idx_contacts_sort_category; DROP INDEX idx_contacts_sort_subcategory; DROP INDEX idx_contacts_sort_comune; DROP INDEX idx_contacts_sort_provincia; DROP INDEX idx_contacts_sort_regione; DROP INDEX idx_contacts_sort_pec; DROP INDEX idx_contacts_sort_sources; DROP INDEX idx_contacts_sort_score; DROP INDEX idx_contacts_sort_classification;
         ALTER TABLE contacts DROP COLUMN is_exportable;",
    )
    .unwrap();
    insert_plain_contact(
        &db,
        "c-1",
        "utente@example.test",
        Some("utente@example.test"),
    );
    insert_plain_contact(
        &db,
        "c-2",
        "bloccato@example.test",
        Some("bloccato@example.test"),
    );
    insert_plain_contact(&db, "c-3", "mailer-daemon@example.test", None);
    db.execute(
        "INSERT INTO blacklist(normalized_email,source_file,imported_at) VALUES('bloccato@example.test','b','now')",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 53).unwrap();
    db.execute("DELETE FROM schema_migrations WHERE version>=54", [])
        .unwrap();

    migrate_database(&mut db, None, false).unwrap();
    migrate_database(&mut db, None, false).expect("idempotent");

    let flags = exportable_flags(&db);
    assert_eq!(flags.len(), 3, "contacts preserved");
    assert_eq!(flags["utente@example.test"], 1);
    assert_eq!(flags["bloccato@example.test"], 0);
    assert_eq!(flags["mailer-daemon@example.test"], 0);
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, LATEST_SCHEMA_VERSION);
    db.execute("DELETE FROM blacklist", []).unwrap();
    assert_eq!(
        exportable_flags(&db)["bloccato@example.test"],
        1,
        "triggers installed"
    );
}

#[test]
fn contact_list_walks_the_exportable_index_without_sorting() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let (where_sql, values) = filter_where(&ListFilters::default());
    let plan: Vec<String> = db
        .prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT contacts.id FROM contacts {where_sql}
             ORDER BY lower(contacts.email),contacts.id LIMIT 100"
        ))
        .unwrap()
        .query_map(rusqlite::params_from_iter(values.iter()), |row| row.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter()
            .any(|step| step.contains("idx_contacts_list_sort"))
            && !plan.iter().any(|step| step.contains("TEMP B-TREE")),
        "{plan:?}"
    );
}

#[test]
fn bulk_import_trigger_suspension_restores_exportable_flags_and_fts() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    let tx = db.transaction().unwrap();
    crate::contacts::suspend_contact_triggers(&tx).unwrap();
    insert_plain_contact(
        &tx,
        "c-1",
        "noreply@example.test",
        Some("noreply@example.test"),
    );
    insert_plain_contact(
        &tx,
        "c-2",
        "utente@example.test",
        Some("utente@example.test"),
    );
    crate::contacts::resume_contact_triggers(&tx).unwrap();
    tx.commit().unwrap();
    let flags = exportable_flags(&db);
    assert_eq!(flags["noreply@example.test"], 0);
    assert_eq!(flags["utente@example.test"], 1);

    // Triggers are back: later writes keep the flag and FTS current.
    db.execute(
        "UPDATE contacts SET status='invalid_syntax' WHERE id='c-2'",
        [],
    )
    .unwrap();
    assert_eq!(exportable_flags(&db)["utente@example.test"], 0);
    db.execute(
        "UPDATE contacts SET status='valid',category='Scuole' WHERE id='c-2'",
        [],
    )
    .unwrap();
    let hits: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM contacts_fts WHERE contacts_fts MATCH 'scuole'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1);
}

#[test]
fn a_second_import_is_rejected_while_one_is_running() {
    let first = crate::imports::acquire_import_slot_for_test().expect("slot is free");
    let second = crate::imports::acquire_import_slot_for_test();
    assert_eq!(
        second.err().as_deref(),
        Some("Un'importazione è già in corso")
    );
    drop(first);
    assert!(crate::imports::acquire_import_slot_for_test().is_ok());
}

#[test]
fn path_imports_read_file_bytes_only_when_loaded() {
    let path = std::env::temp_dir().join(format!("contatti-{}.txt", uuid::Uuid::new_v4()));
    std::fs::write(&path, "utente@example.test\n").unwrap();
    let entry = crate::imports::import_file_entry(&path, None).unwrap();
    assert!(
        entry.bytes.is_empty(),
        "scan must not keep file contents in memory"
    );
    std::fs::write(&path, "utente2@example.test\n").unwrap();
    let loaded = entry.load().unwrap();
    assert_eq!(loaded.bytes, b"utente2@example.test\n");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn full_csv_export_neutralizes_formula_cells_but_keeps_emails() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_plain_contact(
        &db,
        "c-1",
        "utente@example.test",
        Some("utente@example.test"),
    );
    db.execute(
        "UPDATE contacts SET source_file='=HYPERLINK(\"http://example.test\")',comune='-1+2' WHERE id='c-1'",
        [],
    )
    .unwrap();
    let path = std::env::temp_dir().join(format!("contatti-{}.csv", uuid::Uuid::new_v4()));
    crate::imports::export_contacts_to_path(&db, &ListFilters::default(), &path).unwrap();
    let csv = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let row = csv.lines().nth(1).unwrap();
    assert!(row.starts_with("utente@example.test;"), "{row}");
    assert!(row.contains("'=HYPERLINK"), "{row}");
    assert!(row.contains(";'-1+2;"), "{row}");
    assert_eq!(crate::imports::spreadsheet_safe("Varie".into()), "Varie");
}

#[test]
fn post_import_derivation_updates_counts_pec_and_score_for_touched_contacts() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    db.execute(
        "INSERT INTO imports(id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at)
         VALUES ('import-1','f.txt','f.txt','hash','completed',2,2,'now')",
        [],
    )
    .unwrap();
    insert_plain_contact(
        &db,
        "c-1",
        "utente@pec.example.test",
        Some("utente@pec.example.test"),
    );
    insert_plain_contact(&db, "c-2", "altro@example.test", Some("altro@example.test"));
    for (id, contact, row) in [("s-1", "c-1", 1), ("s-2", "c-1", 2), ("s-3", "c-2", 3)] {
        db.execute(
            "INSERT INTO contact_sources(id,contact_id,import_id,source_file,source_row)
             VALUES (?1,?2,'import-1','f.txt',?3)",
            params![id, contact, row],
        )
        .unwrap();
    }
    crate::imports::derive_imported_contacts(&db, &["import-1".to_owned()])
        .expect("derivation runs every statement");
    let (count, is_pec): (i64, i64) = db
        .query_row(
            "SELECT source_occurrence_count,is_pec FROM contacts WHERE id='c-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, is_pec), (2, 1));
    // The derivation's updates score the touched contacts through the triggers.
    let c1 = stored_score(&db, "c-1").expect("touched contact is scored");
    let c2 = stored_score(&db, "c-2").expect("touched contact is scored");
    assert!(
        c1 < c2,
        "a PEC address loses its deliverability points: {c1} vs {c2}"
    );
}

#[test]
fn infer_field_prefers_role_columns_skips_missing_values_and_reuses_key_sets() {
    let fields = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    let keys = ["comune", "descrizionecomune"];
    let first = fields(&[
        ("Comune di residenza", "Torino"),
        ("Descrizione Comune", "Milano"),
    ]);
    assert_eq!(
        crate::parsing::infer_field(&first, &keys).as_deref(),
        Some("Torino")
    );
    let missing_first = fields(&[
        ("Comune di residenza", "  "),
        ("Descrizione Comune", " Milano "),
    ]);
    assert_eq!(
        crate::parsing::infer_field(&missing_first, &keys).as_deref(),
        Some("Milano")
    );
    // Same key set, a different sheet whose headers never match.
    let other_sheet = fields(&[("Provincia", "TO"), ("Email", "utente@example.test")]);
    assert_eq!(crate::parsing::infer_field(&other_sheet, &keys), None);
    let source_keys = ["source_url", "url"];
    let with_role = fields(&[
        ("URL", "https://b.example.test"),
        ("__role_source_0", "https://a.example.test"),
    ]);
    assert_eq!(
        crate::parsing::infer_field(&with_role, &source_keys).as_deref(),
        Some("https://a.example.test")
    );
}

/// One imported contact row plus its linked candidate, as the importer stores it:
/// `email_domain` holds the domain without its TLD.
fn seed_linked_contact(db: &Connection, id: &str, email: &str) {
    let (local, domain, tld) = email_parts(email);
    db.execute(
        "INSERT OR IGNORE INTO imports(id,file_name,relative_path,file_hash,status,parsed_rows,contacts_found,imported_at)
         VALUES ('import-1','source.txt','source.txt','hash','completed',1,1,'2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO source_rows(id,import_id,source_sheet,row_number,raw_context,parse_status,candidate_count,created_at)
         VALUES (?1,'import-1',?2,1,'{}','parsed',1,'2026-01-01T00:00:00Z')",
        params![format!("row-{id}"), id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO contacts(id,email,email_local_part,email_domain,email_tld,status,validation_notes,
             source_file,category,email_comparison_key,routing_status)
         VALUES (?1,?2,?3,?4,?5,'valid','[]','source.txt','Varie',lower(?2),'routing_ok')",
        params![id, email, local, domain, tld],
    )
    .unwrap();
    db.execute(
        "INSERT INTO email_candidates(id,source_row_id,raw_email,canonical_email,comparison_key,
             normalization_method,status,syntax_status,routing_status,mailbox_status,candidate_status,
             current_disposition,contact_id,first_seen_at,last_seen_at)
         VALUES (?1,?2,?3,?3,lower(?3),'literal','linked','syntax_ok','routing_ok','unknown','linked',
             'linked',?4,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        params![format!("candidate-{id}"), format!("row-{id}"), email, id],
    )
    .unwrap();
}

fn routable() -> DomainValidation {
    DomainValidation {
        status: EmailStatus::Valid,
        notes: Vec::new(),
        observed_ttl_seconds: Some(300),
        dns_rcode: Some("NoError".to_owned()),
        mx_hosts: vec!["mx.example.test".to_owned()],
        dns_checked_at: None,
        dns_expires_at: None,
    }
}

#[test]
fn dns_revalidation_uses_full_domains_and_keeps_contacts() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_linked_contact(&db, "c1", "utente@example.test");
    let domains: Vec<String> = crate::mailbox_verification::dns_revalidation_domain_state(&db)
        .unwrap()
        .into_iter()
        .map(|(domain, _, _)| domain)
        .collect();
    assert_eq!(domains, vec!["example.test".to_owned()]);

    let report = crate::mailbox_verification::apply_dns_results(
        &mut db,
        vec![("example.test".to_owned(), routable())],
        1,
    )
    .unwrap();
    assert_eq!(report.separated_contacts, 0);
    let status: String = db
        .query_row("SELECT status FROM contacts WHERE id='c1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "valid");
}

#[test]
fn dns_revalidation_never_restores_removed_or_lifted_contacts() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for (id, reason) in [
        ("removed", "manual_contact_removal"),
        ("lifted", "blacklist_lifted"),
        ("dns", "dns_unverified"),
    ] {
        seed_linked_contact(&db, id, &format!("{id}@example.test"));
        db.execute("DELETE FROM contacts WHERE id=?1", params![id])
            .unwrap();
        db.execute(
            "UPDATE email_candidates SET contact_id=NULL,current_disposition='quarantined',
                 status='quarantined',candidate_status='quarantined',disposition_reason=?2
             WHERE id=?1",
            params![format!("candidate-{id}"), reason],
        )
        .unwrap();
    }
    let report = crate::mailbox_verification::apply_dns_results(
        &mut db,
        vec![("example.test".to_owned(), routable())],
        1,
    )
    .unwrap();
    assert_eq!(
        report.promoted_contacts, 1,
        "only the DNS quarantine recovers"
    );
    let emails: Vec<String> = db
        .prepare("SELECT email FROM contacts ORDER BY email")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(emails, vec!["dns@example.test".to_owned()]);
    let promoted_domain: String = db
        .query_row("SELECT email_domain FROM contacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        promoted_domain, "example",
        "same column format as the importer"
    );
}

#[test]
fn reimport_keeps_ai_and_deterministic_classifications() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    for (id, method) in [("ai", "ai"), ("det", "deterministic"), ("src", "legacy")] {
        seed_linked_contact(&db, id, &format!("{id}@example.test"));
        db.execute(
            "UPDATE contacts SET category='Cultura',subcategory='Museo',classification_method=?2 WHERE id=?1",
            params![id, method],
        )
        .unwrap();
        insert_classification_claim_deferred(
            &db,
            "2026-01-02T00:00:00Z",
            ClassificationClaim {
                candidate_id: Some(&format!("candidate-{id}")),
                contact_id: Some(id),
                source_row_id: Some(&format!("row-{id}")),
                category: "Istruzione",
                subcategory: None,
                confidence: 0.9,
                method: "column",
                evidence: &[],
            },
        )
        .unwrap();
    }
    recompute_imported_classifications(&db, "import-1").unwrap();
    let category = |id: &str| -> String {
        db.query_row(
            "SELECT category FROM contacts WHERE id=?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(category("ai"), "Cultura");
    assert_eq!(category("det"), "Cultura");
    assert_eq!(category("src"), "Istruzione");
}

#[test]
fn database_clear_after_ai_runs_leaves_no_orphans() {
    let mut db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    seed_linked_contact(&db, "c1", "utente@example.test");
    db.execute_batch(
        "INSERT INTO ai_classification_runs(id,model,profile,prompt_version,status,total,started_at)
         VALUES ('run-1','m','p','v','completed',1,'2026-01-01');
         INSERT INTO ai_run_batches(id,run_id,provider,model,contact_count,dispatched_at)
         VALUES ('batch-1','run-1','cascade','jev',1,'2026-01-01');
         INSERT INTO classification_follow_up_queue(contact_id,catalog_id,reason,created_at)
         VALUES ('c1','exact-it-v1','no_match','2026-01-01');
         INSERT INTO deterministic_rule_attributions(contact_id,rule_id,rule_version,catalog_id,run_id,taxonomy_version_id,applied_at)
         VALUES ('c1','it.school.mechanographic',1,'exact-it-v1','run-1',
                 (SELECT id FROM taxonomy_versions LIMIT 1),'2026-01-01');",
    )
    .unwrap();
    assert_eq!(clear_database_connection(&mut db).expect("clear"), 1);
    for table in [
        "ai_run_batches",
        "classification_follow_up_queue",
        "deterministic_rule_attributions",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[test]
fn pec_detection_uses_whole_labels_and_keeps_the_source_flag() {
    assert!(!is_pec_email("info@concert.it"));
    assert!(!is_pec_email("protocollo@comune.certaldo.fi.it"));
    assert!(is_pec_email("protocollo@cert.comune.example.it"));
    assert!(is_pec_email("ufficio@pec.example.it"));
    assert!(is_pec_email("ufficio@legalmail.it"));
    assert!(!is_pec_mx_host("mx.concert.it"));
    assert!(is_pec_mx_host("mx.cert.legalmail.it"));

    let mut contacts = vec![import_contact("segreteria@example.test")];
    contacts[0].is_pec = true;
    let groups = HashMap::from([("example.test".to_owned(), vec![0usize])]);
    let results = HashMap::from([("example.test".to_owned(), routable())]);
    apply_domain_results(&mut contacts, &groups, &results);
    assert!(contacts[0].is_pec, "tipo_mail PEC survives DNS validation");
}

#[test]
fn simple_hash_is_pinned_to_the_values_already_stored() {
    use std::hash::{DefaultHasher, Hash, Hasher};
    for value in [
        "",
        "a",
        "1234567",
        "12345678",
        "contact|row|Istruzione||cand",
        &"x".repeat(100),
    ] {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        assert_eq!(
            simple_hash(value),
            format!("{:016x}", hasher.finish()),
            "{value}"
        );
    }
    assert_eq!(
        simple_hash("utente@example.test"),
        simple_hash("utente@example.test")
    );
}

#[test]
fn failed_migration_keeps_user_version_at_the_last_committed_marker() {
    let db = Connection::open_in_memory().unwrap();
    run_ordered_migrations(&db, Some(30)).expect_err("injected failure");
    let version: i64 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let markers: i64 = db
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((version, markers), (29, 29));
}

#[test]
fn upgrade_to_v55_clears_false_pec_flags_only() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate_database(&mut db, None, false).unwrap();
    seed_linked_contact(&db, "concert", "info@concert.it");
    seed_linked_contact(&db, "declared", "segreteria@concert.example.it");
    seed_linked_contact(&db, "real", "protocollo@cert.example.it");
    db.execute("UPDATE contacts SET is_pec=1", []).unwrap();
    db.execute(
        "INSERT INTO contact_sources(id,contact_id,source_file,raw_context)
         VALUES ('cs-1','declared','source.txt','{\"tipo_mail\":\"PEC\"}')",
        [],
    )
    .unwrap();
    db.pragma_update(None, "user_version", 54).unwrap();
    db.execute("DELETE FROM schema_migrations WHERE version>=55", [])
        .unwrap();
    migrate_database(&mut db, None, false).unwrap();
    migrate_database(&mut db, None, false).expect("idempotent");
    let pec = |id: &str| -> bool {
        db.query_row(
            "SELECT is_pec FROM contacts WHERE id=?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert!(!pec("concert"));
    assert!(pec("declared"));
    assert!(pec("real"));
}

#[test]
fn upgrade_to_v56_reclassifies_superseded_deterministic_results() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate_database(&mut db, None, false).unwrap();
    seed_linked_contact(&db, "fire", "comando@vigilfuoco.it");
    seed_linked_contact(&db, "lab", "info@example.test");
    db.execute_batch(
        "UPDATE contacts SET classification_method='deterministic',
             classification_hash='deterministic:exact-it-v1',classification_state='classified',
             category='Pubblica amministrazione',subcategory='Forze dell''ordine' WHERE id='fire';
         UPDATE contacts SET classification_method='deterministic',
             classification_hash='deterministic:exact-it-v1',classification_state='classified',
             denomination='Laboratorio teatrale',
             category='Enti religiosi',subcategory='Oratorio / centro giovanile' WHERE id='lab';",
    )
    .unwrap();
    db.pragma_update(None, "user_version", 55).unwrap();
    db.execute("DELETE FROM schema_migrations WHERE version>=56", [])
        .unwrap();
    migrate_database(&mut db, None, false).unwrap();
    let row = |id: &str| -> (String, Option<String>, String) {
        db.query_row(
            "SELECT category,subcategory,classification_state FROM contacts WHERE id=?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(
        row("fire"),
        (
            "Pubblica amministrazione".into(),
            Some("Ministero / Ente statale".into()),
            "classified".into()
        )
    );
    assert_eq!(row("lab"), ("Varie".into(), None, "unprocessed".into()));
    let version: String = db
        .query_row(
            "SELECT semantic_version FROM deterministic_rule_catalogs WHERE id='exact-it-v1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // Migration 57 carries the catalog on to 1.4.0.
    assert_eq!(version, "1.4.0");
}

#[test]
fn mcp_server_searches_read_only_and_lists_filter_options() {
    let db = Connection::open_in_memory().unwrap();
    setup_schema(&db).unwrap();
    insert_contact_record(&db, &test_contact()).unwrap();
    let call = |name: &str, arguments: serde_json::Value| {
        let message = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        });
        let reply = crate::mcp::handle(&db, &message).unwrap();
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        (reply["result"]["isError"].as_bool().unwrap(), text)
    };

    let (error, text) = call(
        "search_contacts",
        serde_json::json!({"query": "moncalieri"}),
    );
    let found: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(!error);
    assert_eq!(found["total"], 1);
    assert_eq!(
        found["contacts"][0]["email"],
        "protocollo@comune.moncalieri.to.it"
    );
    assert!(
        found["contacts"][0].get("subcategory").is_none(),
        "null fields are dropped"
    );

    let (_, text) = call(
        "search_contacts",
        serde_json::json!({"category": {"exclude": ["Varie"]}}),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["total"],
        0
    );

    let (error, text) = call(
        "list_filter_options",
        serde_json::json!({"field": "category"}),
    );
    assert!(!error);
    assert!(text.contains("Varie"));
    assert!(call("list_filter_options", serde_json::json!({"field": "pec"})).0);
    assert!(call("drop_table", serde_json::json!({})).0);

    let list = crate::mcp::handle(
        &db,
        &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 2);
    let notification = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert!(crate::mcp::handle(&db, &notification).is_none());

    // The server's own connection refuses writes.
    let dir = std::env::temp_dir().join(format!("mcp-ro-{}", new_contact_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("contatti.sqlite3");
    Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t(x)")
        .unwrap();
    let read_only = crate::mcp::open_read_only(&path).unwrap();
    assert!(read_only.execute("INSERT INTO t VALUES (1)", []).is_err());
    std::fs::remove_dir_all(&dir).ok();
}
