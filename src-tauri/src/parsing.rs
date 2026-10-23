use crate::ai_enrichment::{normalize_extracted_comune, normalize_extracted_regione};
use crate::{
    EMAIL_RE, ENTITY_AT_RE, ENTITY_DOT_RE, EmailStatus, ImportContact, ImportFile,
    OBFUSCATED_AT_RE, OBFUSCATED_DOT_RE, OBFUSCATION_HINT_RE, SPACED_AT_RE, TAXONOMY, WORD_DOT_RE,
    WORD_SEPARATOR_RE, normalize_extracted_province, standard_category,
};
use calamine::{Data, Reader, open_workbook_auto_from_rs};
use email_address::EmailAddress;
use futures::{StreamExt, stream};
use hickory_resolver::TokioResolver;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Cursor, Read};
use std::time::Instant;
use zip::ZipArchive;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum HeaderRole {
    Email,
    Pec,
    Source,
    Comune,
    Provincia,
    Regione,
    Website,
    Address,
    Phone,
    Denomination,
    SchoolType,
    EmailKind,
}

impl HeaderRole {
    fn key(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Pec => "pec",
            Self::Source => "source",
            Self::Comune => "comune",
            Self::Provincia => "provincia",
            Self::Regione => "regione",
            Self::Website => "website",
            Self::Address => "indirizzo",
            Self::Phone => "telefono",
            Self::Denomination => "denominazione",
            Self::SchoolType => "tipologia",
            Self::EmailKind => "tipo_mail",
        }
    }
}

#[derive(Clone, Debug)]
struct DetectedHeaders {
    index: usize,
    values: Vec<String>,
    roles: Vec<Option<HeaderRole>>,
}

pub(crate) struct ParsedRows {
    pub(crate) contacts: Vec<ImportContact>,
    pub(crate) warnings: Vec<String>,
    pub(crate) row_errors: Vec<RowParseError>,
    pub(crate) rows_without_candidates: usize,
    pub(crate) source_rows: Vec<ParsedSourceRow>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedSourceRow {
    pub(crate) sheet: Option<String>,
    pub(crate) source_row: usize,
    pub(crate) raw_context: std::sync::Arc<str>,
    pub(crate) parse_status: String,
    pub(crate) error: Option<String>,
    pub(crate) candidate_count: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct RowParseError {
    pub(crate) sheet: Option<String>,
    pub(crate) source_row: usize,
    pub(crate) message: String,
    pub(crate) raw_record: Option<String>,
}

pub(crate) const SPREADSHEET_EXTENSIONS: &[&str] =
    &[".xlsx", ".xlsm", ".xlsb", ".xla", ".xlam", ".ods", ".odt"];
pub(crate) const DELIMITED_EXTENSIONS: &[&str] = &[".csv", ".tsv", ".txt"];

pub(crate) fn parse_file(file: &ImportFile) -> Result<ParsedFile, String> {
    let lower = file.name.to_lowercase();
    if lower.ends_with(".odt") {
        parse_odt(file)
    } else if SPREADSHEET_EXTENSIONS
        .iter()
        .any(|extension| lower.ends_with(extension))
    {
        parse_workbook(file)
    } else if lower.ends_with(".csv") || lower.ends_with(".tsv") {
        parse_delimited(file, b';')
    } else if lower.ends_with(".txt") {
        parse_delimited_with_quoting(file, b';', false)
    } else {
        parse_text(file)
    }
}

const MAX_ODT_XML_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn parse_odt(file: &ImportFile) -> Result<ParsedFile, String> {
    let mut archive = ZipArchive::new(Cursor::new(file.bytes.as_slice())).map_err(|err| {
        format!(
            "{}: impossibile aprire il pacchetto ODT: {err}",
            display_name(file)
        )
    })?;
    let mut content = String::new();
    archive
        .by_name("content.xml")
        .map_err(|err| {
            format!(
                "{}: content.xml mancante nel pacchetto ODT: {err}",
                display_name(file)
            )
        })?
        // A small zip can expand a thousandfold: cap what is read into memory.
        .take(MAX_ODT_XML_BYTES)
        .read_to_string(&mut content)
        .map_err(|err| {
            format!(
                "{}: impossibile leggere content.xml: {err}",
                display_name(file)
            )
        })?;

    let rows = odt_rows(&content)?;
    let parsed = parse_rows_with_diagnostics(file, None, &rows);
    Ok(ParsedFile {
        contacts: parsed.contacts,
        parsed_rows: rows.len(),
        warnings: parsed.warnings,
        row_errors: parsed.row_errors,
        rows_without_candidates: parsed.rows_without_candidates,
        source_rows: parsed.source_rows,
        encoding: "UTF-8 (OpenDocument XML)".to_owned(),
        delimiter: None,
        sheet_count: 1,
        physical_rows: rows.len(),
        successful_rows: rows.len(),
        malformed_rows: 0,
        failed_sheets: Vec::new(),
    })
}

pub(crate) fn odt_rows(content: &str) -> Result<Vec<Vec<String>>, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(content);
    reader.config_mut().trim_text(true);
    let mut rows = Vec::new();
    let mut current_row: Option<Vec<String>> = None;
    let mut current_cell: Option<String> = None;
    let mut paragraph: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match xml_local_name(event.name().as_ref()) {
                "table-row" => current_row = Some(Vec::new()),
                "table-cell" => current_cell = Some(String::new()),
                "p" | "h" => paragraph = Some(String::new()),
                _ => {}
            },
            Ok(Event::Empty(event)) => {
                if xml_local_name(event.name().as_ref()) == "table-cell"
                    && let Some(row) = current_row.as_mut()
                {
                    row.push(String::new());
                }
            }
            Ok(Event::Text(event)) => {
                let text = quick_xml::escape::unescape(event.as_ref())
                    .map_err(|err| format!("ODT XML non valido: {err}"))?;
                if let Some(value) = paragraph.as_mut() {
                    value.push_str(&text);
                } else if let Some(value) = current_cell.as_mut() {
                    value.push_str(&text);
                }
            }
            Ok(Event::CData(event)) => {
                let text = event.as_ref();
                if let Some(value) = paragraph.as_mut() {
                    value.push_str(text);
                } else if let Some(value) = current_cell.as_mut() {
                    value.push_str(text);
                }
            }
            Ok(Event::End(event)) => match xml_local_name(event.name().as_ref()) {
                "p" | "h" => {
                    if let Some(value) = paragraph.take() {
                        if let Some(cell) = current_cell.as_mut() {
                            if !cell.is_empty() {
                                cell.push(' ');
                            }
                            cell.push_str(value.trim());
                        } else if !value.trim().is_empty() {
                            rows.push(vec![value.trim().to_owned()]);
                        }
                    }
                }
                "table-cell" => {
                    if let Some(row) = current_row.as_mut() {
                        row.push(current_cell.take().unwrap_or_default().trim().to_owned());
                    }
                }
                "table-row" => {
                    if let Some(row) = current_row.take() {
                        rows.push(row);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(err) => return Err(format!("ODT XML non valido: {err}")),
            _ => {}
        }
    }
    Ok(rows)
}

fn xml_local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

pub(crate) struct ParsedFile {
    pub(crate) contacts: Vec<ImportContact>,
    pub(crate) parsed_rows: usize,
    pub(crate) warnings: Vec<String>,
    pub(crate) row_errors: Vec<RowParseError>,
    pub(crate) rows_without_candidates: usize,
    pub(crate) source_rows: Vec<ParsedSourceRow>,
    pub(crate) encoding: String,
    pub(crate) delimiter: Option<String>,
    pub(crate) sheet_count: usize,
    pub(crate) physical_rows: usize,
    pub(crate) successful_rows: usize,
    pub(crate) malformed_rows: usize,
    pub(crate) failed_sheets: Vec<String>,
}

fn constrain_to_western_european(
    guess: &'static encoding_rs::Encoding,
) -> &'static encoding_rs::Encoding {
    const PLAUSIBLE: &[&encoding_rs::Encoding] = &[
        encoding_rs::UTF_8,
        encoding_rs::WINDOWS_1252,
        encoding_rs::ISO_8859_15,
        encoding_rs::ISO_8859_2,
        encoding_rs::MACINTOSH,
    ];
    if PLAUSIBLE.contains(&guess) {
        guess
    } else {
        encoding_rs::WINDOWS_1252
    }
}

pub(crate) fn decode_text(bytes: &[u8]) -> (String, Option<String>) {
    let (encoding, body) = match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => (encoding_rs::UTF_8, rest),
        [0xFF, 0xFE, rest @ ..] => (encoding_rs::UTF_16LE, rest),
        [0xFE, 0xFF, rest @ ..] => (encoding_rs::UTF_16BE, rest),
        _ => match std::str::from_utf8(bytes) {
            Ok(text) => return (text.to_owned(), None),
            Err(_) => {
                let mut detector =
                    chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
                detector.feed(bytes, true);

                let guess = detector.guess(Some(b"it"), chardetng::Utf8Detection::Deny);
                (constrain_to_western_european(guess), bytes)
            }
        },
    };
    let (decoded, used_encoding, had_replacements) = encoding.decode(body);
    let mut warning = (used_encoding != encoding_rs::UTF_8).then(|| {
        format!(
            "File non UTF-8: rilevato e decodificato come {}",
            used_encoding.name()
        )
    });
    if had_replacements {
        warning = Some(format!(
            "{}; alcuni caratteri non decodificabili sono stati sostituiti",
            warning.unwrap_or_else(|| "Decodifica UTF-8".to_owned())
        ));
    }
    (decoded.into_owned(), warning)
}

fn detected_encoding_label(bytes: &[u8]) -> &'static str {
    match bytes {
        [0xEF, 0xBB, 0xBF, ..] => encoding_rs::UTF_8.name(),
        [0xFF, 0xFE, ..] => encoding_rs::UTF_16LE.name(),
        [0xFE, 0xFF, ..] => encoding_rs::UTF_16BE.name(),
        _ if std::str::from_utf8(bytes).is_ok() => encoding_rs::UTF_8.name(),
        _ => {
            let mut detector =
                chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            detector.feed(bytes, true);
            constrain_to_western_european(
                detector.guess(Some(b"it"), chardetng::Utf8Detection::Deny),
            )
            .name()
        }
    }
}

pub(crate) fn delimiter_name(delimiter: u8) -> &'static str {
    match delimiter {
        b'\t' => "tab",
        b';' => "punto_e_virgola",
        b',' => "virgola",
        b'|' => "pipe",
        _ => "sconosciuto",
    }
}

pub(crate) fn detect_delimiter(text: &str, fallback: u8) -> u8 {
    const CANDIDATES: [u8; 4] = *b";,\t|";

    const SAMPLED_RECORDS: usize = 100;
    let mut counts = [0usize; CANDIDATES.len()];
    let mut in_quotes = false;
    let mut records = 0usize;
    let mut record_has_content = false;
    let bytes = text.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() && records < SAMPLED_RECORDS {
        match bytes[index] {
            b'"' => {
                record_has_content = true;
                if in_quotes && bytes.get(index + 1) == Some(&b'"') {
                    index += 1;
                } else {
                    in_quotes = !in_quotes;
                }
            }
            b'\n' if !in_quotes => {
                if record_has_content {
                    records += 1;
                }
                record_has_content = false;
            }
            byte if !in_quotes => {
                if let Some(position) = CANDIDATES.iter().position(|candidate| *candidate == byte) {
                    counts[position] += 1;
                    record_has_content = true;
                } else if !byte.is_ascii_whitespace() {
                    record_has_content = true;
                }
            }
            _ => {}
        }
        index += 1;
    }

    CANDIDATES
        .iter()
        .zip(counts)
        .max_by_key(|(candidate, count)| (*count, **candidate == fallback))
        .filter(|(_, count)| *count > 0)
        .map(|(candidate, _)| *candidate)
        .unwrap_or(fallback)
}

pub(crate) fn parse_workbook(file: &ImportFile) -> Result<ParsedFile, String> {
    let cursor = Cursor::new(file.bytes.as_slice());
    let mut workbook = open_workbook_auto_from_rs(cursor).map_err(|err| {
        format!(
            "{}: impossibile leggere il foglio di calcolo: {err}",
            display_name(file)
        )
    })?;
    let mut contacts = Vec::new();
    let mut parsed_rows = 0;
    let mut warnings = Vec::new();
    let mut row_errors = Vec::new();
    let mut rows_without_candidates = 0;
    let mut source_rows = Vec::new();
    let mut failed_sheets = Vec::new();

    for sheet_name in workbook.sheet_names().to_owned() {
        match workbook.worksheet_range(&sheet_name) {
            Ok(range) => {
                let rows: Vec<Vec<String>> = range
                    .rows()
                    .map(|row| row.iter().map(cell_to_string).collect())
                    .collect();
                parsed_rows += rows.len();
                let parsed = parse_rows_with_diagnostics(file, Some(sheet_name.clone()), &rows);
                contacts.extend(parsed.contacts);
                row_errors.extend(parsed.row_errors);
                rows_without_candidates += parsed.rows_without_candidates;
                source_rows.extend(parsed.source_rows);
                warnings.extend(parsed.warnings);
            }
            Err(err) => {
                row_errors.push(RowParseError {
                    sheet: Some(sheet_name.clone()),

                    source_row: 1,
                    message: err.to_string(),
                    raw_record: None,
                });
                source_rows.push(ParsedSourceRow {
                    sheet: Some(sheet_name.clone()),
                    source_row: 1,
                    raw_context: std::sync::Arc::from(""),
                    parse_status: "failed".to_owned(),
                    error: Some(err.to_string()),
                    candidate_count: 0,
                });
                failed_sheets.push(sheet_name.clone());
                warnings.push(format!("Foglio {sheet_name}: {err}"));
            }
        }
    }

    Ok(ParsedFile {
        contacts,
        parsed_rows,
        warnings,
        row_errors,
        rows_without_candidates,
        source_rows,
        encoding: "workbook".to_owned(),
        delimiter: None,
        sheet_count: workbook.sheet_names().len(),
        physical_rows: parsed_rows,
        successful_rows: parsed_rows,
        malformed_rows: 0,
        failed_sheets,
    })
}

pub(crate) fn parse_delimited(
    file: &ImportFile,
    fallback_delimiter: u8,
) -> Result<ParsedFile, String> {
    parse_delimited_with_quoting(file, fallback_delimiter, true)
}

pub(crate) fn parse_delimited_with_quoting(
    file: &ImportFile,
    fallback_delimiter: u8,
    quoting: bool,
) -> Result<ParsedFile, String> {
    let (text, encoding_warning) = decode_text(&file.bytes);

    let text = protect_html_entities_for_delimiter(&text);
    let delimiter = detect_delimiter(&text, fallback_delimiter);
    #[cfg(debug_assertions)]
    {
        let sample = text.lines().next().unwrap_or_default();
        println!(
            "[parse][delimited] file=\"{}\" delimiter=\"{}\" sample_len={}",
            display_name(file),
            delimiter_name(delimiter),
            sample.len()
        );
    }

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .quoting(quoting)
        .from_reader(text.as_bytes());

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut skipped_records = 0usize;
    let mut row_errors = Vec::new();
    for (record_index, record) in reader.records().enumerate() {
        match record {
            Ok(fields) => rows.push(fields.iter().map(restore_protected_entities).collect()),
            Err(error) => {
                skipped_records += 1;

                rows.push(Vec::new());
                row_errors.push(RowParseError {
                    sheet: None,

                    source_row: record_index + 1,
                    message: error.to_string(),
                    raw_record: text
                        .lines()
                        .nth(record_index)
                        .map(restore_protected_entities),
                });
            }
        }
    }
    if quoting
        && let Some(source_row) = unclosed_quote_start_line(&text)
        && !row_errors
            .iter()
            .any(|error| error.source_row == source_row)
    {
        row_errors.push(RowParseError {
            sheet: None,
            source_row,
            message: "Campo CSV tra virgolette non chiuso a fine file".to_owned(),
            raw_record: text
                .lines()
                .skip(source_row.saturating_sub(1))
                .collect::<Vec<_>>()
                .join("\n")
                .into(),
        });
    }
    let parsed = parse_rows_with_diagnostics(file, None, &rows);
    row_errors.extend(parsed.row_errors);
    let mut source_rows = parsed.source_rows;
    for error in &row_errors {
        if let Some(source_row) = source_rows
            .iter_mut()
            .find(|row| row.sheet == error.sheet && row.source_row == error.source_row)
        {
            source_row.raw_context =
                std::sync::Arc::from(error.raw_record.clone().unwrap_or_default());
            source_row.parse_status = "malformed".to_owned();
            source_row.error = Some(error.message.clone());
            source_row.candidate_count = 0;
        } else if error.raw_record.is_some() {
            source_rows.push(ParsedSourceRow {
                sheet: error.sheet.clone(),
                source_row: error.source_row,
                raw_context: std::sync::Arc::from(error.raw_record.clone().unwrap_or_default()),
                parse_status: "malformed".to_owned(),
                error: Some(error.message.clone()),
                candidate_count: 0,
            });
        }
    }
    let encoding_label = detected_encoding_label(&file.bytes).to_owned();
    let mut warnings = encoding_warning.into_iter().collect::<Vec<_>>();
    if skipped_records > 0 {
        warnings.push(format!(
            "{skipped_records} righe non leggibili ignorate: verifica manuale consigliata."
        ));
    }
    warnings.extend(parsed.warnings);
    let malformed_count = row_errors.len();
    Ok(ParsedFile {
        parsed_rows: rows.len(),
        contacts: parsed.contacts,
        warnings,
        row_errors,
        rows_without_candidates: parsed
            .rows_without_candidates
            .saturating_sub(malformed_count),
        source_rows,
        encoding: encoding_label,
        delimiter: Some(delimiter_name(delimiter).to_owned()),
        sheet_count: 1,
        physical_rows: rows.len(),
        successful_rows: rows.len().saturating_sub(malformed_count),
        malformed_rows: malformed_count,
        failed_sheets: Vec::new(),
    })
}

fn unclosed_quote_start_line(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut in_quotes = false;
    let mut start_line = None;
    let mut line = 1usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' if in_quotes && bytes.get(index + 1) == Some(&b'"') => index += 1,
            b'"' => {
                in_quotes = !in_quotes;
                if in_quotes {
                    start_line = Some(line);
                } else {
                    start_line = None;
                }
            }
            b'\n' => line += 1,
            _ => {}
        }
        index += 1;
    }
    in_quotes.then_some(start_line.unwrap_or(line))
}

pub(crate) fn parse_text(file: &ImportFile) -> Result<ParsedFile, String> {
    let text = String::from_utf8_lossy(&file.bytes);
    let rows: Vec<Vec<String>> = text.lines().map(|line| vec![line.to_owned()]).collect();
    println!(
        "[parse][text] file=\"{}\" rows={}",
        display_name(file),
        rows.len()
    );
    let parsed = parse_rows_with_diagnostics(file, None, &rows);
    Ok(ParsedFile {
        parsed_rows: rows.len(),
        contacts: parsed.contacts,
        warnings: parsed.warnings,
        row_errors: parsed.row_errors,
        rows_without_candidates: parsed.rows_without_candidates,
        source_rows: parsed.source_rows,
        encoding: "UTF-8 (lossy)".to_owned(),
        delimiter: None,
        sheet_count: 1,
        physical_rows: rows.len(),
        successful_rows: rows.len(),
        malformed_rows: 0,
        failed_sheets: Vec::new(),
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_rows(
    file: &ImportFile,
    sheet: Option<String>,
    rows: &[Vec<String>],
) -> Vec<ImportContact> {
    parse_rows_with_diagnostics(file, sheet, rows).contacts
}

pub(crate) fn parse_rows_with_diagnostics(
    file: &ImportFile,
    sheet: Option<String>,
    rows: &[Vec<String>],
) -> ParsedRows {
    let mut warnings = Vec::new();
    let mut headers = detect_headers_detailed(rows);
    if let Some(detected) = headers.as_mut() {
        if let Some(repaired) = repair_school_headers(detected, rows) {
            if repaired {
                let location = sheet.as_deref().unwrap_or("dati delimitati");
                warnings.push(format!(
                    "Intestazione disallineata riallineata automaticamente nel foglio {location}."
                ));
            } else {
                let location = sheet.as_deref().unwrap_or("dati delimitati");
                warnings.push(format!(
                    "Possibile disallineamento delle colonne nel foglio {location}: verifica manuale consigliata."
                ));
            }
        }
        #[cfg(debug_assertions)]
        println!(
            "[parse][header] file=\"{}\" header_row={} columns={}",
            display_name(file),
            detected.index + 1,
            detected.values.len()
        );
    } else {
        #[cfg(debug_assertions)]
        println!(
            "[parse][header] file=\"{}\" header=none",
            display_name(file)
        );
    }
    let mut contacts = Vec::new();
    let mut rows_without_candidates = 0usize;
    let mut source_rows = Vec::with_capacity(rows.len());
    let (file_provincia, file_regione) = infer_geography_from_filename(&file.name);
    let paese = infer_country(file);
    let contact_kind = if normalize_key(&display_name(file)).contains("contattipersonali") {
        "personale"
    } else {
        "istituzionale"
    };
    let source_file: std::sync::Arc<str> = std::sync::Arc::from(display_name(file));
    let paese_shared: std::sync::Arc<str> = std::sync::Arc::from(paese);
    let contact_kind_shared: std::sync::Arc<str> = std::sync::Arc::from(contact_kind);

    for (index, row) in rows.iter().enumerate() {
        let raw_fields = map_raw_fields_detailed(headers.as_ref(), row);
        let raw_context: std::sync::Arc<str> = std::sync::Arc::from(
            serde_json::to_string(&raw_fields).unwrap_or_else(|_| "{}".to_owned()),
        );
        if headers
            .as_ref()
            .is_some_and(|headers| headers.index == index)
        {
            source_rows.push(ParsedSourceRow {
                sheet: sheet.clone(),
                source_row: index + 1,
                raw_context,
                parse_status: "header".to_owned(),
                error: None,
                candidate_count: 0,
            });
            continue;
        }

        let row_contact_start = contacts.len();
        let mut candidates = Vec::new();
        for raw_cell in row {
            let (normalized_cell, cell_method) =
                normalize_obfuscated_email_text_with_method(raw_cell);
            let mut raw_search_cursor = 0usize;
            for matched in EMAIL_RE.find_iter(&normalized_cell) {
                let extracted_email = clean_email(matched.as_str());
                let (raw_email, candidate_method, candidate_end) =
                    raw_candidate_fragment(raw_cell, &extracted_email, raw_search_cursor)
                        .unwrap_or_else(|| {
                            (
                                extracted_email.clone(),
                                cell_method.clone(),
                                raw_search_cursor,
                            )
                        });
                raw_search_cursor = candidate_end;
                let (normalized_source, normalization_method) =
                    if let Some(decoded) = decode_rot13_address(&extracted_email) {
                        (decoded, "rot13".to_owned())
                    } else {
                        (extracted_email, candidate_method)
                    };
                let normalized_email = canonicalize_email(&normalized_source);
                let comparison_key = email_comparison_key(&normalized_email);
                let (email_local_part, email_domain, email_tld) = email_parts(&normalized_email);
                candidates.push((
                    raw_email,
                    normalized_email,
                    comparison_key,
                    normalization_method,
                    email_local_part,
                    email_domain,
                    email_tld,
                ));
            }
        }
        if !candidates.is_empty() {
            let (category, subcategory, method, confidence, evidence) =
                deterministic_classification(file, &raw_fields);
            let raw_email_kind = infer_field(&raw_fields, &["tipo_mail", "tipo mail"]);
            let comune = infer_field(&raw_fields, &["comune", "descrizionecomune"])
                .and_then(|value| normalize_extracted_comune(&value));
            let provincia = infer_field(&raw_fields, &["provincia", "prov"])
                .map(normalize_extracted_province)
                .or_else(|| file_provincia.clone());
            let regione = infer_field(&raw_fields, &["regione"])
                .and_then(|value| normalize_extracted_regione(&value))
                .or_else(|| file_regione.clone());
            let website = infer_website(&raw_fields);
            let source_url = infer_source_url(&raw_fields);
            let macroarea = regione
                .as_deref()
                .and_then(crate::ai_enrichment::macroarea_from_regione)
                .map(String::from);
            for (
                raw_email,
                normalized_email,
                comparison_key,
                normalization_method,
                email_local_part,
                email_domain,
                email_tld,
            ) in candidates
            {
                contacts.push(ImportContact {
                    raw_email,
                    normalized_email: normalized_email.clone(),
                    comparison_key,
                    normalization_method,
                    email_local_part,
                    email_domain,
                    email_tld,
                    status: EmailStatus::DomainUnverified,
                    validation_notes: Vec::new(),
                    source_file: source_file.clone(),
                    category: category.clone(),
                    subcategory: subcategory.clone(),
                    contact_kind: contact_kind_shared.to_string(),
                    paese: paese_shared.to_string(),
                    is_pec: is_pec_from_kind(raw_email_kind.as_deref(), &normalized_email),
                    comune: comune.clone(),
                    provincia: provincia.clone(),
                    regione: regione.clone(),
                    macroarea: macroarea.clone(),
                    website: website.clone(),
                    source_url: source_url.clone(),
                    source_sheet: sheet.clone(),
                    source_row: index + 1,
                    raw_context: raw_context.clone(),
                    classification_method: method.clone(),
                    classification_confidence: confidence,
                    classification_evidence: evidence.clone(),
                });
            }
        }
        if contacts.len() == row_contact_start && row.iter().any(|cell| !cell.trim().is_empty()) {
            rows_without_candidates += 1;
        }
        source_rows.push(ParsedSourceRow {
            sheet: sheet.clone(),
            source_row: index + 1,
            raw_context,
            parse_status: if contacts.len() > row_contact_start {
                "parsed"
            } else {
                "no_candidate"
            }
            .to_owned(),
            error: None,
            candidate_count: contacts.len().saturating_sub(row_contact_start),
        });
    }

    ParsedRows {
        contacts,
        warnings,
        row_errors: Vec::new(),
        rows_without_candidates,
        source_rows,
    }
}

pub(crate) fn deterministic_classification(
    file: &ImportFile,
    fields: &BTreeMap<String, String>,
) -> (String, Option<String>, String, f64, Vec<String>) {
    if let Some(value) = infer_field(
        fields,
        &[
            "tipologia istituto",
            "tipologia",
            "tipo",
            "ordine scuola",
            "grado scuola",
        ],
    ) && let Some(subcategory) = school_subcategory(&value)
    {
        return (
            "Istruzione".to_owned(),
            Some(subcategory),
            "column".to_owned(),
            1.0,
            vec![format!("colonna tipologia: {value}")],
        );
    }
    let file_label = display_name(file);
    let mixed_file = file.name.contains('&') || file.name.contains(',');
    // Same text as `"{file_label} {values joined by ' '}"`, lowercased, without
    // cloning every value first.
    let mut context = if mixed_file {
        String::new()
    } else {
        format!("{file_label} ")
    };
    for (index, value) in fields.values().enumerate() {
        if index > 0 {
            context.push(' ');
        }
        context.push_str(value);
    }
    let context = context.to_lowercase();
    for (needle, category, subcategory) in [
        ("bibliotec", "Cultura", "Biblioteca"),
        ("teatr", "Cultura", "Teatro"),
        ("cinema", "Cultura", "Cinema"),
        ("muse", "Cultura", "Museo"),
        ("pro loco", "Turismo e promozione territoriale", "Pro Loco"),
        ("proloco", "Turismo e promozione territoriale", "Pro Loco"),
        (
            "ufficio turist",
            "Turismo e promozione territoriale",
            "Ufficio turistico",
        ),
        ("apt", "Turismo e promozione territoriale", "APT"),
        (
            "eventi",
            "Turismo e promozione territoriale",
            "Organizzatore di eventi e sagre",
        ),
        (
            "sagre",
            "Turismo e promozione territoriale",
            "Organizzatore di eventi e sagre",
        ),
        ("frati minori", "Enti religiosi", "Frati Minori"),
        ("domenican", "Enti religiosi", "Domenicani"),
        ("agostinian", "Enti religiosi", "Agostiniani"),
        ("salesian", "Enti religiosi", "Salesiani"),
        (
            "caritas",
            "Enti religiosi",
            "Caritas / ente caritativo religioso",
        ),
        ("santuar", "Enti religiosi", "Santuario / basilica"),
        ("basilic", "Enti religiosi", "Santuario / basilica"),
        ("dioces", "Enti religiosi", "Diocesi / curia"),
        ("curia", "Enti religiosi", "Diocesi / curia"),
        ("decanat", "Enti religiosi", "Decanato / vicariato"),
        ("vicariat", "Enti religiosi", "Decanato / vicariato"),
        ("sacerdot", "Enti religiosi", "Clero"),
        ("preti", "Enti religiosi", "Clero"),
        ("curati", "Enti religiosi", "Clero"),
        ("alpin", "Associazioni", "Alpini"),
        ("cai", "Associazioni", "CAI"),
        ("associazione noi", "Associazioni", "NOI"),
        (
            "associazione genitori",
            "Associazioni",
            "Associazione genitori",
        ),
        ("rotary", "Associazioni", "Rotary"),
        ("lions", "Associazioni", "Lions"),
        ("associazioni sportive", "Associazioni", "Sportiva"),
        ("associazioni culturali", "Associazioni", "Culturale"),
        (
            "comunità montan",
            "Pubblica amministrazione",
            "Comunità montana",
        ),
        (
            "comunita montan",
            "Pubblica amministrazione",
            "Comunità montana",
        ),
        ("comune", "Pubblica amministrazione", "Comune"),
        ("comuni", "Pubblica amministrazione", "Comune"),
        ("orator", "Enti religiosi", "Oratorio / centro giovanile"),
        (
            "centro giovanile",
            "Enti religiosi",
            "Oratorio / centro giovanile",
        ),
        ("parroc", "Enti religiosi", "Parrocchia"),
        ("chies", "Enti religiosi", "Chiesa"),
    ] {
        if context.contains(needle) {
            return (
                category.to_owned(),
                Some(subcategory.to_owned()),
                "rule".to_owned(),
                0.9,
                vec![format!("corrispondenza: {needle}")],
            );
        }
    }
    if context.contains("contatti personali") {
        return (
            "Varie".to_owned(),
            Some("Contatto personale".to_owned()),
            "rule".to_owned(),
            0.75,
            vec!["nome file: contatti personali".to_owned()],
        );
    }
    if context.contains("frati") {
        return (
            "Enti religiosi".to_owned(),
            crate::taxonomy::fallback_subcategory("Enti religiosi").map(str::to_owned),
            "rule".to_owned(),
            0.6,
            vec!["ordine religioso aggregato non attribuibile".to_owned()],
        );
    }
    if mixed_file {
        let categories = categories_from_filename(&file.name);
        if categories.len() == 1 {
            return (
                categories[0].to_owned(),
                crate::taxonomy::fallback_subcategory(categories[0]).map(str::to_owned),
                "rule".to_owned(),
                0.6,
                vec!["nome file misto: sottocategoria non assegnata".to_owned()],
            );
        }
        return (
            "Varie".to_owned(),
            Some("Dato misto o da revisionare".to_owned()),
            "rule".to_owned(),
            0.4,
            vec!["nome file con categorie multiple".to_owned()],
        );
    }
    let category = standard_category(category_hints(file).iter().map(String::as_str));
    let candidate = subcategory_from_file(file);
    let safe_subcategory = TAXONOMY
        .iter()
        .find(|(name, _)| *name == category)
        .and_then(|(_, values)| {
            candidate.as_deref().and_then(|candidate| {
                values
                    .iter()
                    .find(|value| normalize_key(value) == normalize_key(candidate))
                    .map(|value| (*value).to_owned())
            })
        })
        .or_else(|| crate::taxonomy::fallback_subcategory(&category).map(str::to_owned));
    (
        category,
        safe_subcategory,
        "rule".to_owned(),
        0.6,
        vec!["nome file o cartella".to_owned()],
    )
}

fn categories_from_filename(file_name: &str) -> Vec<&'static str> {
    let normalized = normalize_key(file_name);
    let mut categories = Vec::new();
    for (category, needles) in [
        ("Istruzione", &["scuol", "istruzion"][..]),
        ("Pubblica amministrazione", &["comun", "comunitamontan"][..]),
        (
            "Enti religiosi",
            &["parroc", "orator", "chies", "dioces", "frat", "salesian"][..],
        ),
        (
            "Associazioni",
            &["associa", "alpin", "rotary", "lions", "cai", "noi"][..],
        ),
        ("Cultura", &["bibliotec", "teatr", "cinema", "muse"][..]),
        (
            "Turismo e promozione territoriale",
            &["turism", "proloco", "apt", "sagr", "event"][..],
        ),
    ] {
        if needles.iter().any(|needle| normalized.contains(needle)) {
            categories.push(category);
        }
    }
    categories
}

fn infer_geography_from_filename(file_name: &str) -> (Option<String>, Option<String>) {
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    let normalized = format!(" {} ", normalize_words(stem));
    let matches = PROVINCE_REGIONS
        .iter()
        .filter(|(province, _)| normalized.contains(&format!(" {} ", normalize_words(province))))
        .collect::<Vec<_>>();

    if matches.len() == 1 {
        return (Some(matches[0].0.to_owned()), Some(matches[0].1.to_owned()));
    }

    let regions = REGIONS
        .iter()
        .filter(|region| normalized.contains(&format!(" {} ", normalize_words(region))))
        .collect::<Vec<_>>();
    if regions.len() == 1 {
        (None, Some((*regions[0]).to_owned()))
    } else {
        (None, None)
    }
}

fn infer_country(file: &ImportFile) -> &'static str {
    let context = normalize_key(&display_name(file));
    if ["svizzera", "suisse", "schweiz", "switzerland"]
        .iter()
        .any(|value| context.contains(value))
    {
        "Svizzera"
    } else {
        "Italia"
    }
}

fn normalize_words(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) const REGIONS: &[&str] = &[
    "Abruzzo",
    "Basilicata",
    "Calabria",
    "Campania",
    "Emilia Romagna",
    "Friuli Venezia Giulia",
    "Lazio",
    "Liguria",
    "Lombardia",
    "Marche",
    "Molise",
    "Piemonte",
    "Puglia",
    "Sardegna",
    "Sicilia",
    "Toscana",
    "Trentino Alto Adige",
    "Umbria",
    "Valle d'Aosta",
    "Veneto",
];

pub(crate) const PROVINCE_REGIONS: &[(&str, &str)] = &[
    ("Agrigento", "Sicilia"),
    ("Alessandria", "Piemonte"),
    ("Ancona", "Marche"),
    ("Aosta", "Valle d'Aosta"),
    ("Ascoli Piceno", "Marche"),
    ("L'Aquila", "Abruzzo"),
    ("Arezzo", "Toscana"),
    ("Asti", "Piemonte"),
    ("Avellino", "Campania"),
    ("Bari", "Puglia"),
    ("Barletta-Andria-Trani", "Puglia"),
    ("Belluno", "Veneto"),
    ("Benevento", "Campania"),
    ("Bergamo", "Lombardia"),
    ("Biella", "Piemonte"),
    ("Bologna", "Emilia Romagna"),
    ("Bolzano", "Trentino Alto Adige"),
    ("Brescia", "Lombardia"),
    ("Brindisi", "Puglia"),
    ("Cagliari", "Sardegna"),
    ("Caltanissetta", "Sicilia"),
    ("Campobasso", "Molise"),
    ("Caserta", "Campania"),
    ("Catania", "Sicilia"),
    ("Catanzaro", "Calabria"),
    ("Chieti", "Abruzzo"),
    ("Como", "Lombardia"),
    ("Cosenza", "Calabria"),
    ("Cremona", "Lombardia"),
    ("Crotone", "Calabria"),
    ("Cuneo", "Piemonte"),
    ("Enna", "Sicilia"),
    ("Fermo", "Marche"),
    ("Ferrara", "Emilia Romagna"),
    ("Firenze", "Toscana"),
    ("Foggia", "Puglia"),
    ("Forlì-Cesena", "Emilia Romagna"),
    ("Frosinone", "Lazio"),
    ("Genova", "Liguria"),
    ("Gorizia", "Friuli Venezia Giulia"),
    ("Grosseto", "Toscana"),
    ("Imperia", "Liguria"),
    ("Isernia", "Molise"),
    ("La Spezia", "Liguria"),
    ("Latina", "Lazio"),
    ("Lecce", "Puglia"),
    ("Lecco", "Lombardia"),
    ("Livorno", "Toscana"),
    ("Lodi", "Lombardia"),
    ("Lucca", "Toscana"),
    ("Macerata", "Marche"),
    ("Mantova", "Lombardia"),
    ("Massa-Carrara", "Toscana"),
    ("Matera", "Basilicata"),
    ("Messina", "Sicilia"),
    ("Milano", "Lombardia"),
    ("Modena", "Emilia Romagna"),
    ("Monza e Brianza", "Lombardia"),
    ("Napoli", "Campania"),
    ("Novara", "Piemonte"),
    ("Nuoro", "Sardegna"),
    ("Oristano", "Sardegna"),
    ("Padova", "Veneto"),
    ("Palermo", "Sicilia"),
    ("Parma", "Emilia Romagna"),
    ("Pavia", "Lombardia"),
    ("Perugia", "Umbria"),
    ("Pesaro e Urbino", "Marche"),
    ("Pescara", "Abruzzo"),
    ("Piacenza", "Emilia Romagna"),
    ("Pisa", "Toscana"),
    ("Pistoia", "Toscana"),
    ("Pordenone", "Friuli Venezia Giulia"),
    ("Potenza", "Basilicata"),
    ("Prato", "Toscana"),
    ("Ragusa", "Sicilia"),
    ("Ravenna", "Emilia Romagna"),
    ("Reggio Calabria", "Calabria"),
    ("Reggio Emilia", "Emilia Romagna"),
    ("Rieti", "Lazio"),
    ("Rimini", "Emilia Romagna"),
    ("Roma", "Lazio"),
    ("Rovigo", "Veneto"),
    ("Salerno", "Campania"),
    ("Sassari", "Sardegna"),
    ("Savona", "Liguria"),
    ("Siena", "Toscana"),
    ("Siracusa", "Sicilia"),
    ("Sondrio", "Lombardia"),
    ("Sud Sardegna", "Sardegna"),
    ("Taranto", "Puglia"),
    ("Teramo", "Abruzzo"),
    ("Terni", "Umbria"),
    ("Torino", "Piemonte"),
    ("Trapani", "Sicilia"),
    ("Trento", "Trentino Alto Adige"),
    ("Treviso", "Veneto"),
    ("Trieste", "Friuli Venezia Giulia"),
    ("Udine", "Friuli Venezia Giulia"),
    ("Varese", "Lombardia"),
    ("Venezia", "Veneto"),
    ("Verbano-Cusio-Ossola", "Piemonte"),
    ("Vercelli", "Piemonte"),
    ("Verona", "Veneto"),
    ("Vibo Valentia", "Calabria"),
    ("Vicenza", "Veneto"),
    ("Viterbo", "Lazio"),
];

pub(crate) fn school_subcategory(value: &str) -> Option<String> {
    let normalized = normalize_key(value);
    if normalized.contains("infanzia") || normalized.contains("materna") {
        Some("Infanzia".to_owned())
    } else if normalized.contains("primaria") || normalized.contains("elementare") {
        Some("Primaria".to_owned())
    } else if normalized.contains("secondariadiprimogrado")
        || normalized.contains("secprimogrado")
        || normalized.contains("scuolamedia")
    {
        Some("Secondaria di primo grado".to_owned())
    } else if normalized.contains("secondariadisecondogrado")
        || normalized.contains("secsecondogrado")
        || normalized.contains("superiore")
    {
        Some("Secondaria di secondo grado".to_owned())
    } else if normalized.contains("istitutocomprensivo") {
        Some("Istituto comprensivo".to_owned())
    } else if normalized.contains("centroterritoriale") || normalized.contains("cpia") {
        Some("Centro territoriale / CPIA".to_owned())
    } else if normalized.contains("universit")
        || normalized.contains("accademia")
        || normalized.contains("conservatorio")
        || normalized.contains("altaformazione")
        || normalized.contains("afam")
    {
        Some("Università / alta formazione".to_owned())
    } else if normalized.contains("formazioneprofessionale")
        || normalized.contains("scuolaprofessionale")
        || normalized.contains("istitutoprofessionale")
    {
        Some("Formazione professionale".to_owned())
    } else {
        None
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn detect_headers(rows: &[Vec<String>]) -> Option<(usize, Vec<String>)> {
    detect_headers_detailed(rows).map(|headers| (headers.index, headers.values))
}

fn detect_headers_detailed(rows: &[Vec<String>]) -> Option<DetectedHeaders> {
    rows.iter()
        .take(50)
        .enumerate()
        .filter_map(|(index, row)| {
            let roles = row.iter().map(|cell| header_role(cell)).collect::<Vec<_>>();
            let distinct = roles.iter().flatten().copied().collect::<HashSet<_>>();
            if distinct.len() < 2 {
                return None;
            }

            let mut score = (distinct.len() as i32) * 10;
            for cell in row.iter().filter(|cell| !cell.trim().is_empty()) {
                let trimmed = cell.trim();
                if EMAIL_RE.is_match(trimmed)
                    || looks_like_url(trimmed)
                    || trimmed.eq_ignore_ascii_case("non disponibile")
                {
                    score -= 15;
                }
                if trimmed.chars().all(|character| {
                    character.is_ascii_digit()
                        || character.is_ascii_whitespace()
                        || matches!(character, '.' | ',' | '-' | '/')
                }) {
                    score -= 8;
                }
                if trimmed.chars().count() > 120 {
                    score -= 5;
                }
            }

            Some((
                score,
                DetectedHeaders {
                    index,
                    values: row.to_owned(),
                    roles,
                },
            ))
        })
        .max_by(|(left_score, left), (right_score, right)| {
            left_score
                .cmp(right_score)
                .then_with(|| right.index.cmp(&left.index))
        })
        .filter(|(score, _)| *score >= 10)
        .map(|(_, headers)| headers)
}

fn map_raw_fields_detailed(
    headers: Option<&DetectedHeaders>,
    row: &[String],
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for (index, value) in row.iter().enumerate() {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = headers
            .and_then(|headers| headers.values.get(index))
            .filter(|header| !header.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("colonna_{}", index + 1));

        let key = if fields.contains_key(&key) {
            format!("{key} ({})", index + 1)
        } else {
            key
        };
        fields.insert(key, trimmed.to_owned());
        if let Some(role) = headers
            .and_then(|headers| headers.roles.get(index))
            .copied()
            .flatten()
        {
            fields.insert(
                format!("__role_{}_{}", role.key(), index + 1),
                trimmed.to_owned(),
            );
        }
    }
    fields
}

fn repair_school_headers(headers: &mut DetectedHeaders, rows: &[Vec<String>]) -> Option<bool> {
    let present = headers
        .roles
        .iter()
        .flatten()
        .copied()
        .collect::<HashSet<_>>();
    let school_layout = present.contains(&HeaderRole::Regione)
        && present.contains(&HeaderRole::Provincia)
        && (present.contains(&HeaderRole::SchoolType)
            || headers
                .values
                .iter()
                .any(|value| normalize_key(value).contains("scuola")))
        && (present.contains(&HeaderRole::Email) || present.contains(&HeaderRole::Pec));
    if !school_layout {
        return None;
    }

    let data_rows = rows
        .iter()
        .skip(headers.index + 1)
        .take(25)
        .filter(|row| row.iter().any(|value| !value.trim().is_empty()))
        .collect::<Vec<_>>();
    if data_rows.len() < 3 {
        return None;
    }

    let signatures = data_rows
        .iter()
        .filter_map(|row| school_core_signature(row))
        .collect::<Vec<_>>();
    let needs_attention = !present.contains(&HeaderRole::Comune)
        || signatures.iter().any(|signature| {
            headers.roles.get(signature.address) != Some(&Some(HeaderRole::Address))
                || headers.roles.get(signature.comune) != Some(&Some(HeaderRole::Comune))
                || headers.roles.get(signature.school_type) != Some(&Some(HeaderRole::SchoolType))
        });
    if !needs_attention {
        return None;
    }
    if signatures.len() < 3 {
        return Some(false);
    }

    let mut counts = HashMap::new();
    for signature in &signatures {
        *counts.entry(*signature).or_insert(0usize) += 1;
    }
    let Some((signature, matching)) = counts.into_iter().max_by_key(|(_, count)| *count) else {
        return Some(false);
    };
    if matching * 100 < signatures.len() * 80 {
        return Some(false);
    }

    let width = data_rows
        .iter()
        .map(|row| row.len())
        .max()
        .unwrap_or(headers.values.len())
        .max(headers.values.len());
    headers.values.resize(width, String::new());
    headers.roles.resize(width, None);
    set_inferred_header(headers, 0, HeaderRole::Regione);
    set_inferred_header(headers, 1, HeaderRole::Provincia);
    set_inferred_header(headers, signature.code, HeaderRole::Denomination);
    if signature.address > signature.code {
        set_inferred_header(headers, signature.address - 1, HeaderRole::Denomination);
    }
    set_inferred_header(headers, signature.address, HeaderRole::Address);
    set_inferred_header(headers, signature.comune, HeaderRole::Comune);
    set_inferred_header(headers, signature.school_type, HeaderRole::SchoolType);

    infer_school_contact_columns(headers, &data_rows, signature.school_type + 1);
    Some(true)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SchoolCoreSignature {
    code: usize,
    address: usize,
    comune: usize,
    school_type: usize,
}

fn school_core_signature(row: &[String]) -> Option<SchoolCoreSignature> {
    let code = row.iter().position(|value| looks_like_school_code(value))?;
    let school_type = row.iter().position(|value| looks_like_school_type(value))?;
    let address = row
        .iter()
        .enumerate()
        .skip(code + 1)
        .take(school_type.saturating_sub(code + 1))
        .find(|(_, value)| looks_like_address(value))
        .map(|(index, _)| index)?;
    let comune = school_type.checked_sub(1)?;
    (address < comune).then_some(SchoolCoreSignature {
        code,
        address,
        comune,
        school_type,
    })
}

fn set_inferred_header(headers: &mut DetectedHeaders, index: usize, role: HeaderRole) {
    if index >= headers.values.len() {
        return;
    }
    headers.values[index] = role.key().to_owned();
    headers.roles[index] = Some(role);
}

fn infer_school_contact_columns(
    headers: &mut DetectedHeaders,
    rows: &[&Vec<String>],
    start: usize,
) {
    for index in start..headers.values.len() {
        let values = rows
            .iter()
            .filter_map(|row| row.get(index))
            .map(|value| value.trim())
            .filter(|value| !is_missing_value(value))
            .collect::<Vec<_>>();
        if values.is_empty() {
            continue;
        }
        let pec_count = values.iter().filter(|value| is_pec_address(value)).count();
        let email_count = values
            .iter()
            .filter(|value| EMAIL_RE.is_match(value))
            .count();
        let website_count = values
            .iter()
            .filter(|value| looks_like_website_value(value))
            .count();
        if pec_count > 0 && pec_count * 2 >= email_count.max(1) {
            set_inferred_header(headers, index, HeaderRole::Pec);
        } else if email_count > 0 {
            set_inferred_header(headers, index, HeaderRole::Email);
        } else if website_count * 2 >= values.len() {
            set_inferred_header(headers, index, HeaderRole::Website);
        }
    }
}

fn looks_like_school_code(value: &str) -> bool {
    let normalized = value.trim();
    normalized.len() == 10
        && normalized
            .chars()
            .take(2)
            .all(|character| character.is_ascii_alphabetic())
        && normalized
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
        && normalized
            .chars()
            .any(|character| character.is_ascii_digit())
}

fn looks_like_address(value: &str) -> bool {
    let normalized = normalize_key(value);
    [
        "via", "viale", "piazza", "corso", "localita", "strada", "largo", "vicolo", "frazione",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}

fn looks_like_school_type(value: &str) -> bool {
    school_subcategory(value).is_some()
        || normalize_key(value).contains("centroterritoriale")
        || normalize_key(value).contains("nonstatale")
}

fn is_pec_address(value: &str) -> bool {
    EMAIL_RE.is_match(value) && is_pec_email(value)
}

fn looks_like_url(value: &str) -> bool {
    let normalized = value.trim().to_lowercase();
    normalized.starts_with("http://")
        || normalized.starts_with("https://")
        || normalized.starts_with("www.")
}

fn looks_like_website_value(value: &str) -> bool {
    let normalized = value.trim();
    looks_like_url(normalized)
        || (!normalized.contains('@')
            && normalized.contains('.')
            && !normalized.chars().any(char::is_whitespace))
}

fn is_missing_value(value: &str) -> bool {
    value.is_empty()
        || value.eq_ignore_ascii_case("null")
        || value.eq_ignore_ascii_case("non disponibile")
        || value == "#N/A"
}

type ImmediateValidation = (usize, EmailStatus, Vec<String>);
type ContactsByDomain = HashMap<String, Vec<usize>>;

pub(crate) fn group_contacts_for_validation(
    contacts: &[ImportContact],
    blacklist: &HashSet<String>,
) -> (Vec<ImmediateValidation>, ContactsByDomain) {
    let mut immediate = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, contact) in contacts.iter().enumerate() {
        let email = contact.normalized_email.as_str();
        if blacklist.contains(&contact.comparison_key) {
            immediate.push((
                index,
                EmailStatus::Blacklisted,
                vec!["Presente in blacklist/cancellami".to_owned()],
            ));
        } else if non_contact_mailbox_reason(email).is_some() {
            immediate.push((
                index,
                EmailStatus::Blacklisted,
                vec!["Casella automatica non contattabile (non_contact_mailbox)".to_owned()],
            ));
        } else if !is_valid_email_syntax(email) {
            immediate.push((
                index,
                EmailStatus::InvalidSyntax,
                vec!["Formato email non valido".to_owned()],
            ));
        } else if let Some((_, domain)) = email.rsplit_once('@') {
            groups
                .entry(domain.trim_end_matches('.').to_ascii_lowercase())
                .or_default()
                .push(index);
        } else {
            immediate.push((
                index,
                EmailStatus::InvalidSyntax,
                vec!["Dominio mancante".to_owned()],
            ));
        }
    }
    (immediate, groups)
}

pub(crate) fn is_valid_email_syntax(email: &str) -> bool {
    if EmailAddress::is_valid(email) {
        return true;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || domain.is_empty()
        || email.matches('@').count() != 1
        || email
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
    {
        return false;
    }
    let local_allowed = |character: char| {
        character.is_alphanumeric()
            || matches!(
                character,
                '.' | '!'
                    | '#'
                    | '$'
                    | '%'
                    | '&'
                    | '\''
                    | '*'
                    | '+'
                    | '-'
                    | '/'
                    | '='
                    | '?'
                    | '^'
                    | '_'
                    | '`'
                    | '{'
                    | '|'
                    | '}'
                    | '~'
            )
    };
    if !local.chars().all(local_allowed) {
        return false;
    }
    let labels = domain.split('.').collect::<Vec<_>>();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '-')
        })
        && labels
            .last()
            .is_some_and(|label| label.chars().count() >= 2)
}

pub(crate) fn address_risk_notes(email: &str) -> Vec<String> {
    let Some((local_part, domain)) = email.rsplit_once('@') else {
        return Vec::new();
    };
    let mut notes = Vec::new();
    if non_contact_mailbox_reason(email).is_some() {
        notes.push("Casella automatica non contattabile (non_contact_mailbox)".to_owned());
    }
    let role_prefixes = [
        "abuse",
        "admin",
        "billing",
        "contact",
        "info",
        "marketing",
        "noreply",
        "no-reply",
        "office",
        "privacy",
        "sales",
        "segreteria",
        "support",
        "webmaster",
    ];
    if role_prefixes.contains(&local_part) {
        notes.push("Casella di ruolo/generica (segnalazione informativa)".to_owned());
    }
    let disposable_domains = [
        "10minutemail.com",
        "guerrillamail.com",
        "maildrop.cc",
        "mailinator.com",
        "sharklasers.com",
        "temp-mail.org",
        "tempmail.com",
        "yopmail.com",
    ];
    if disposable_domains.contains(&domain) {
        notes.push("Dominio noto di email temporanee/disposable".to_owned());
    }
    let placeholder_domains = ["example.com", "example.net", "example.org", "test.com"];
    if placeholder_domains.contains(&domain) {
        notes.push("Dominio di esempio/placeholder".to_owned());
    }
    let likely_typos = [
        ("gmai.com", "gmail.com"),
        ("gmail.co", "gmail.com"),
        ("gmial.com", "gmail.com"),
        ("hotnail.com", "hotmail.com"),
        ("outlok.com", "outlook.com"),
        ("yaho.com", "yahoo.com"),
    ];
    if let Some((_, suggestion)) = likely_typos.iter().find(|(typo, _)| domain == *typo) {
        notes.push(format!("Possibile refuso nel dominio; forse {suggestion}"));
    }
    notes
}

/// Automated sender/bounce local parts. `contacts::exportable_contact_sql` mirrors
/// the same rule in SQL: a prefix followed by nothing or a non-letter.
pub(crate) const NON_CONTACT_LOCAL_PREFIXES: &[&str] = &[
    "noreply",
    "no-reply",
    "no_reply",
    "no.reply",
    "donotreply",
    "do-not-reply",
    "do_not_reply",
    "do.not.reply",
    "mailer-daemon",
    "mailer_daemon",
    "mailer.daemon",
    "mailerdaemon",
    "bounce",
    "bounces",
    "autoresponder",
    "autoreply",
    "auto-reply",
    "auto_reply",
    "auto.reply",
];

pub(crate) fn non_contact_mailbox_reason(email: &str) -> Option<&'static str> {
    let local = email.rsplit_once('@')?.0.trim().to_ascii_lowercase();

    NON_CONTACT_LOCAL_PREFIXES.iter().find_map(|prefix| {
        let suffix = local.strip_prefix(prefix)?;
        if suffix.is_empty()
            || suffix
                .chars()
                .next()
                .is_some_and(|character| !character.is_ascii_alphabetic())
        {
            Some("non_contact_mailbox")
        } else {
            None
        }
    })
}

pub(crate) fn apply_domain_results(
    contacts: &mut [ImportContact],
    groups: &HashMap<String, Vec<usize>>,
    results: &HashMap<String, DomainValidation>,
) {
    for (domain, indexes) in groups {
        let Some(result) = results.get(domain) else {
            continue;
        };
        for index in indexes {
            let risk_notes = address_risk_notes(&contacts[*index].normalized_email);
            contacts[*index].status = result.status;
            contacts[*index].validation_notes = result.notes.clone();
            contacts[*index].validation_notes.extend(risk_notes);
            // Keep a PEC flag set by the source column (`tipo_mail`).
            contacts[*index].is_pec = contacts[*index].is_pec
                || is_pec(&contacts[*index].normalized_email, &result.mx_hosts);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DomainValidation {
    pub(crate) status: EmailStatus,
    pub(crate) notes: Vec<String>,

    pub(crate) observed_ttl_seconds: Option<i64>,
    pub(crate) dns_rcode: Option<String>,
    pub(crate) mx_hosts: Vec<String>,
    pub(crate) dns_checked_at: Option<String>,
    pub(crate) dns_expires_at: Option<String>,
}

fn lookup_ttl_seconds(lookup: &hickory_resolver::lookup::Lookup) -> Option<i64> {
    let seconds = lookup
        .valid_until()
        .saturating_duration_since(Instant::now())
        .as_secs();
    i64::try_from(seconds).ok()
}

fn lookup_rcode(lookup: &hickory_resolver::lookup::Lookup) -> Option<String> {
    Some(lookup.message().metadata.response_code.to_string())
}

fn negative_dns_details(error: &hickory_resolver::net::NetError) -> (Option<i64>, Option<String>) {
    match error {
        hickory_resolver::net::NetError::Dns(hickory_resolver::net::DnsError::NoRecordsFound(
            records,
        )) => (
            records
                .negative_ttl
                .or_else(|| records.soa.as_ref().map(|soa| soa.ttl))
                .map(i64::from),
            Some(records.response_code.to_string()),
        ),
        hickory_resolver::net::NetError::Dns(hickory_resolver::net::DnsError::ResponseCode(
            response_code,
        )) => (None, Some(response_code.to_string())),
        _ => (None, None),
    }
}

fn min_ttl(current: &mut Option<i64>, candidate: Option<i64>) {
    if let Some(candidate) = candidate {
        *current = Some(current.map_or(candidate, |value| value.min(candidate)));
    }
}

pub(crate) async fn validate_domain(resolver: &TokioResolver, domain: &str) -> DomainValidation {
    match resolver.mx_lookup(domain).await {
        Ok(mx) => {
            let dns_rcode = lookup_rcode(&mx);
            let mut observed_ttl_seconds = lookup_ttl_seconds(&mx);
            let mut records = mx
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    hickory_resolver::proto::rr::RData::MX(mx) => Some((
                        mx.preference,
                        mx.exchange.to_utf8().trim_end_matches('.').to_owned(),
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>();
            records.sort_by_key(|(preference, _)| *preference);
            if records.len() == 1 && records[0].1.is_empty() {
                return DomainValidation {
                    status: EmailStatus::DomainMissingMx,
                    notes: vec![
                        "Il dominio pubblica un Null MX e dichiara di non accettare email"
                            .to_owned(),
                    ],
                    observed_ttl_seconds,
                    dns_rcode,
                    mx_hosts: Vec::new(),
                    dns_checked_at: None,
                    dns_expires_at: None,
                };
            }
            let mut usable_hosts = Vec::new();
            let mut transient_host_errors = Vec::new();
            let all_mx_hosts = records
                .iter()
                .map(|(_, host)| host.clone())
                .filter(|host| !host.is_empty())
                .collect::<Vec<_>>();
            let host_lookups = stream::iter(
                records
                    .into_iter()
                    .map(|(_, host)| host)
                    .filter(|host| !host.is_empty()),
            )
            .map(|host| async move {
                let result = resolver.lookup_ip(host.as_str()).await;
                (host, result)
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
            for (host, lookup) in host_lookups {
                match lookup {
                    Ok(ips) if ips.iter().next().is_some() => {
                        min_ttl(
                            &mut observed_ttl_seconds,
                            lookup_ttl_seconds(ips.as_lookup()),
                        );
                        usable_hosts.push(host)
                    }
                    Ok(_) => {}
                    Err(error) if error.is_nx_domain() || error.is_no_records_found() => {}
                    Err(error) => {
                        transient_host_errors.push(format!("{host}: {error}"));
                    }
                }
            }
            if usable_hosts.is_empty() {
                if !transient_host_errors.is_empty() {
                    return DomainValidation {
                        status: EmailStatus::DomainUnverified,
                        notes: vec![format!(
                            "MX presenti ma verifica temporaneamente non riuscita: {}",
                            transient_host_errors.join(", ")
                        )],
                        observed_ttl_seconds,
                        dns_rcode,
                        mx_hosts: all_mx_hosts,
                        dns_checked_at: None,
                        dns_expires_at: None,
                    };
                }
                DomainValidation {
                    status: EmailStatus::DomainMissingMx,
                    notes: vec![
                        "Record MX presenti, ma nessun server MX risolve in A/AAAA".to_owned(),
                    ],
                    observed_ttl_seconds,
                    dns_rcode,
                    mx_hosts: all_mx_hosts,
                    dns_checked_at: None,
                    dns_expires_at: None,
                }
            } else {
                DomainValidation {
                    status: EmailStatus::Valid,
                    notes: vec![format!(
                        "Dominio con record MX e {} server risolvibili",
                        usable_hosts.len()
                    )],
                    observed_ttl_seconds,
                    dns_rcode,
                    mx_hosts: all_mx_hosts,
                    dns_checked_at: None,
                    dns_expires_at: None,
                }
            }
        }
        Err(err) if err.is_nx_domain() => {
            let (observed_ttl_seconds, dns_rcode) = negative_dns_details(&err);
            DomainValidation {
                status: EmailStatus::DomainMissingMx,
                notes: vec!["Il dominio non esiste nel DNS (NXDOMAIN)".to_owned()],
                observed_ttl_seconds,
                dns_rcode,
                mx_hosts: Vec::new(),
                dns_checked_at: None,
                dns_expires_at: None,
            }
        }
        Err(err) if err.is_no_records_found() => {
            let (mut observed_ttl_seconds, mut dns_rcode) = negative_dns_details(&err);
            match resolver.lookup_ip(domain).await {
                Ok(ips) if ips.iter().next().is_some() => {
                    min_ttl(
                        &mut observed_ttl_seconds,
                        lookup_ttl_seconds(ips.as_lookup()),
                    );
                    if dns_rcode.is_none() {
                        dns_rcode = Some(lookup_rcode(ips.as_lookup()).unwrap_or_default());
                    }
                    DomainValidation {
                        status: EmailStatus::Valid,
                        notes: vec![
                            "Nessun MX: dominio instradabile tramite MX implicito su A/AAAA"
                                .to_owned(),
                        ],
                        observed_ttl_seconds,
                        dns_rcode,
                        mx_hosts: Vec::new(),
                        dns_checked_at: None,
                        dns_expires_at: None,
                    }
                }
                Ok(_) => DomainValidation {
                    status: EmailStatus::DomainMissingMx,
                    notes: vec!["Dominio senza MX e senza record A/AAAA".to_owned()],
                    observed_ttl_seconds,
                    dns_rcode,
                    mx_hosts: Vec::new(),
                    dns_checked_at: None,
                    dns_expires_at: None,
                },
                Err(ip_err) if ip_err.is_nx_domain() || ip_err.is_no_records_found() => {
                    let (ip_ttl, ip_rcode) = negative_dns_details(&ip_err);
                    min_ttl(&mut observed_ttl_seconds, ip_ttl);
                    if dns_rcode.is_none() {
                        dns_rcode = ip_rcode;
                    }
                    DomainValidation {
                        status: EmailStatus::DomainMissingMx,
                        notes: vec!["Dominio senza MX e senza record A/AAAA".to_owned()],
                        observed_ttl_seconds,
                        dns_rcode,
                        mx_hosts: Vec::new(),
                        dns_checked_at: None,
                        dns_expires_at: None,
                    }
                }
                Err(ip_err) => DomainValidation {
                    status: EmailStatus::DomainUnverified,
                    notes: vec![format!(
                        "Errore DNS temporaneo durante il fallback A/AAAA: {ip_err}"
                    )],
                    observed_ttl_seconds,
                    dns_rcode,
                    mx_hosts: Vec::new(),
                    dns_checked_at: None,
                    dns_expires_at: None,
                },
            }
        }
        Err(err) => {
            let (observed_ttl_seconds, dns_rcode) = negative_dns_details(&err);
            DomainValidation {
                status: EmailStatus::DomainUnverified,
                notes: vec![format!("Errore DNS temporaneo o SERVFAIL: {err}")],
                observed_ttl_seconds,
                dns_rcode,
                mx_hosts: Vec::new(),
                dns_checked_at: None,
                dns_expires_at: None,
            }
        }
    }
}

pub(crate) fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(value) => value.trim().to_owned(),
        Data::Float(value) => {
            if value.fract() == 0.0 {
                format!("{value:.0}")
            } else {
                value.to_string()
            }
        }
        Data::Int(value) => value.to_string(),
        Data::Bool(value) => value.to_string(),
        Data::DateTime(value) => value.to_string(),
        Data::DateTimeIso(value) | Data::DurationIso(value) => value.to_owned(),
        Data::Error(value) => value.to_string(),
    }
}

const PLAUSIBLE_TLDS: &[&str] = &[
    "at", "be", "biz", "ch", "cloud", "com", "cz", "de", "dk", "edu", "es", "eu", "fi", "fr",
    "gov", "gr", "hr", "hu", "ie", "info", "int", "it", "li", "lu", "me", "mobi", "name", "net",
    "nl", "no", "online", "org", "pl", "pt", "ro", "se", "si", "sk", "sm", "tv", "uk", "us", "va",
];

fn rot13(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            'a'..='z' => (((character as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((character as u8 - b'A' + 13) % 26) + b'A') as char,
            other => other,
        })
        .collect()
}

fn tld_is_plausible(email: &str) -> bool {
    email
        .rsplit_once('.')
        .is_some_and(|(_, tld)| PLAUSIBLE_TLDS.contains(&tld.to_ascii_lowercase().as_str()))
}

pub(crate) fn decode_rot13_address(email: &str) -> Option<String> {
    if tld_is_plausible(email) {
        return None;
    }
    let decoded = rot13(email);
    tld_is_plausible(&decoded).then_some(decoded)
}

pub(crate) fn canonicalize_email(email: &str) -> String {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return email.to_owned();
    };
    format!("{local}@{}", domain.to_lowercase())
}

pub(crate) fn email_comparison_key(email: &str) -> String {
    email.to_lowercase()
}

pub(crate) fn clean_email(value: &str) -> String {
    value
        .trim_matches(|c: char| {
            c.is_ascii_punctuation() && c != '.' && c != '-' && c != '_' && c != '@'
        })
        .trim_end_matches('.')
        .to_owned()
}

pub(crate) fn email_parts(email: &str) -> (String, String, String) {
    let Some((local_part, domain)) = email.split_once('@') else {
        return (email.to_owned(), String::new(), String::new());
    };
    let Some((domain_without_tld, tld)) = domain.rsplit_once('.') else {
        return (local_part.to_owned(), domain.to_owned(), String::new());
    };
    (
        local_part.to_owned(),
        domain_without_tld.to_owned(),
        format!(".{tld}"),
    )
}

pub(crate) fn decode_html_entities(value: &str) -> String {
    let with_at = ENTITY_AT_RE.replace_all(value, "@");
    ENTITY_DOT_RE.replace_all(&with_at, ".").into_owned()
}

fn protect_html_entities_for_delimiter(value: &str) -> Cow<'_, str> {
    if !ENTITY_AT_RE.is_match(value) && !ENTITY_DOT_RE.is_match(value) {
        return Cow::Borrowed(value);
    }
    let with_at = ENTITY_AT_RE.replace_all(value, "__DB_EMAIL_AT_ENTITY__");
    Cow::Owned(
        ENTITY_DOT_RE
            .replace_all(&with_at, "__DB_EMAIL_DOT_ENTITY__")
            .into_owned(),
    )
}

fn restore_protected_entities(value: &str) -> String {
    if !value.contains("__DB_EMAIL_") {
        return value.to_owned();
    }
    value
        .replace("__DB_EMAIL_AT_ENTITY__", "&#64;")
        .replace("__DB_EMAIL_DOT_ENTITY__", "&#46;")
}

fn raw_candidate_fragment(
    original: &str,
    extracted: &str,
    minimum_start: usize,
) -> Option<(String, String, usize)> {
    let (local, domain) = extracted.split_once('@')?;
    let final_label = domain.rsplit('.').next()?;
    let mut cursor = minimum_start.min(original.len());
    while let Some(local_offset) = original.get(cursor..)?.find(local) {
        let start = cursor.saturating_add(local_offset);
        let search_from = start.saturating_add(local.len());
        let mut end_cursor = search_from;
        while let Some(label_offset) = original.get(end_cursor..)?.find(final_label) {
            let end = end_cursor
                .saturating_add(label_offset)
                .saturating_add(final_label.len());
            let fragment = original.get(start..end)?;
            let (normalized_fragment, method) =
                normalize_obfuscated_email_text_with_method(fragment);
            let matches_extracted = EMAIL_RE
                .find_iter(&normalized_fragment)
                .any(|candidate| clean_email(candidate.as_str()) == extracted);
            if matches_extracted {
                return Some((fragment.to_owned(), method, end));
            }
            end_cursor = end;
        }
        cursor = start.saturating_add(local.len());
    }
    None
}

pub(crate) fn normalize_obfuscated_email_text_with_method(value: &str) -> (String, String) {
    if !OBFUSCATION_HINT_RE.is_match(value) {
        return (value.to_owned(), "literal".to_owned());
    }
    let with_entities = decode_html_entities(value);
    let mut method = if with_entities != value {
        "html_entity"
    } else {
        "literal"
    };
    let with_at = OBFUSCATED_AT_RE.replace_all(&with_entities, "@");
    if with_at != with_entities {
        method = "bracket_separator";
    }
    let with_dot = OBFUSCATED_DOT_RE.replace_all(&with_at, ".");
    if with_dot != with_at {
        method = "bracket_separator";
    }
    let with_words = WORD_SEPARATOR_RE.replace_all(&with_dot, |captures: &regex::Captures| {
        format!(
            "{}@{}",
            &captures[1],
            WORD_DOT_RE.replace_all(&captures[2], ".")
        )
    });
    if with_words != with_dot {
        method = "word_separator";
    }
    let normalized = SPACED_AT_RE
        .replace_all(&with_words, "${1}@${2}")
        .into_owned();
    if normalized != with_words {
        method = "spaced_at";
    }
    (normalized, method.to_owned())
}

pub(crate) fn file_stem_category(file: &ImportFile) -> Option<String> {
    let name = if file.relative_path.is_empty() {
        file.name.as_str()
    } else {
        file.relative_path.as_str()
    };
    let stem = std::path::Path::new(name).file_stem()?.to_string_lossy();
    let category = stem
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (!category.is_empty()).then_some(category)
}

pub(crate) fn subcategory_from_file(file: &ImportFile) -> Option<String> {
    let name = if file.relative_path.is_empty() {
        file.name.as_str()
    } else {
        file.relative_path.as_str()
    };
    let stem = std::path::Path::new(name).file_stem()?.to_string_lossy();
    let words: Vec<String> = stem
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .filter(|word| {
            let normalized = word.to_lowercase();
            !matches!(
                normalized.as_str(),
                "comuni"
                    | "comune"
                    | "scuole"
                    | "scuola"
                    | "parrocchie"
                    | "parrocchia"
                    | "associazioni"
                    | "associazione"
                    | "varie"
                    | "altro"
            ) && !normalized.chars().all(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

pub(crate) fn category_hints(file: &ImportFile) -> Vec<String> {
    let mut hints = Vec::new();
    if let Some(file_category) = file_stem_category(file) {
        hints.push(file_category);
    }
    let path = if file.relative_path.is_empty() {
        &file.name
    } else {
        &file.relative_path
    };
    for part in path.split(['/', '\\', '.', '_', '-']) {
        let normalized = part.trim().to_lowercase();
        if normalized.len() < 3 {
            continue;
        }
        for (needle, category) in [
            ("comun", "comuni"),
            ("scuol", "scuola"),
            ("parroc", "parrocchia"),
            ("orator", "oratorio"),
            ("associa", "associazione"),
            ("turism", "turismo"),
            ("proloco", "pro loco"),
            ("bibliotec", "biblioteca"),
            ("alpin", "alpini"),
            ("frati", "frati"),
            ("docent", "docenti"),
            ("insegn", "insegnanti"),
        ] {
            if normalized.contains(needle) && !hints.iter().any(|hint| hint == category) {
                hints.push(category.to_owned());
            }
        }
    }
    hints
}

/// How a header matches one `infer_field` key set; it depends only on the header
/// name, so it is computed once per header instead of once per row.
#[derive(Clone, Copy, PartialEq)]
enum FieldMatch {
    /// A `__role_<role>_` column for one of the requested roles: preferred.
    Role,
    /// A plain column whose normalized name contains a candidate key.
    Name,
    None,
}

struct InferKeys {
    role_prefixes: Vec<String>,
    normalized: Vec<String>,
    headers: std::cell::RefCell<HashMap<String, FieldMatch>>,
}

impl InferKeys {
    fn new(keys: &[&str]) -> Self {
        let roles = keys
            .iter()
            .filter_map(|key| header_role(key))
            .collect::<HashSet<_>>();
        Self {
            role_prefixes: roles
                .iter()
                .map(|role| format!("__role_{}_", role.key()))
                .collect(),
            normalized: keys.iter().map(|key| normalize_key(key)).collect(),
            headers: Default::default(),
        }
    }

    fn classify(&self, header: &str) -> FieldMatch {
        if let Some(found) = self.headers.borrow().get(header) {
            return *found;
        }
        let found = if self
            .role_prefixes
            .iter()
            .any(|prefix| header.starts_with(prefix.as_str()))
        {
            FieldMatch::Role
        } else if header.starts_with("__role_") {
            FieldMatch::None
        } else {
            let normalized = normalize_key(header);
            if self
                .normalized
                .iter()
                .any(|candidate| normalized.contains(candidate.as_str()))
            {
                FieldMatch::Name
            } else {
                FieldMatch::None
            }
        };
        self.headers.borrow_mut().insert(header.to_owned(), found);
        found
    }
}

thread_local! {
    // ponytail: grows with distinct key sets and header names seen by this thread,
    // which stay in the hundreds even for the full compatibility corpus.
    static INFER_KEYS: std::cell::RefCell<HashMap<String, std::rc::Rc<InferKeys>>> =
        Default::default();
}

pub(crate) fn infer_field(raw_fields: &BTreeMap<String, String>, keys: &[&str]) -> Option<String> {
    let cache_key = keys.join("\u{1f}");
    let prepared = INFER_KEYS.with(|cache| {
        std::rc::Rc::clone(
            cache
                .borrow_mut()
                .entry(cache_key)
                .or_insert_with(|| std::rc::Rc::new(InferKeys::new(keys))),
        )
    });
    let pick = |wanted: FieldMatch| {
        raw_fields.iter().find_map(|(key, value)| {
            (prepared.classify(key) == wanted)
                .then(|| value.trim())
                .filter(|value| !is_missing_value(value))
                .map(str::to_owned)
        })
    };
    pick(FieldMatch::Role).or_else(|| pick(FieldMatch::Name))
}

pub(crate) fn infer_source_url(raw_fields: &BTreeMap<String, String>) -> Option<String> {
    infer_field(
        raw_fields,
        &[
            "source_url",
            "source",
            "sorgente",
            "fonte",
            "url/casella di posta",
            "url sorgente",
            "url origine",
            "provenienza",
            "sito",
            "website",
            "url",
            "web",
        ],
    )
    .filter(|value| looks_like_website_value(value))
    .or_else(|| {
        raw_fields.values().find_map(|value| {
            let trimmed = value.trim();
            (trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with("www."))
            .then(|| trimmed.to_owned())
        })
    })
}

fn infer_website(raw_fields: &BTreeMap<String, String>) -> Option<String> {
    infer_field(raw_fields, &["sito web", "sito", "website", "web"])
        .filter(|value| looks_like_website_value(value))
}

/// Whole DNS labels used by PEC domains and hosts (`pec.`, `cert.`, `postacert.`, …).
/// Labels are compared exactly so `concert.it` or `certaldo` never count.
fn is_pec_label(label: &str) -> bool {
    matches!(
        label,
        "pec" | "cert" | "certmail" | "pecmail" | "postacert" | "postecert" | "legalmail"
    ) || label.contains("legalmail")
        || label.contains("postacert")
}

fn has_pec_label(host: &str) -> bool {
    host.trim()
        .trim_end_matches('.')
        .to_ascii_lowercase()
        .split('.')
        .any(is_pec_label)
}

/// Address domain looks like PEC (`@pec.`, `.cert.`, legalmail, postacert, …).
pub(crate) fn is_pec_email(email: &str) -> bool {
    email
        .rsplit_once('@')
        .is_some_and(|(_, domain)| has_pec_label(domain))
}

/// Source column (IndicePA `tipo_mail`, MIUR PEC header) or the address itself.
pub(crate) fn is_pec_from_kind(raw_kind: Option<&str>, email: &str) -> bool {
    let raw = raw_kind.unwrap_or_default().trim().to_ascii_lowercase();
    raw.contains("pec")
        || raw.contains("legalmail")
        || raw.contains("postacert")
        || raw.contains("cert")
        || is_pec_email(email)
}

/// MX hosts of Italian PEC providers. Plain `mx.aruba.it` is ordinary mail;
/// only `mx.pec.aruba.it` (and the other hosts below) are PEC.
const PEC_MX_SUFFIXES: &[&str] = &[
    "pec.aruba.it",
    "pec.actalis.it",
    "cert.legalmail.it",
    "sicurezzapostale.it",
    "postecert.it",
    "telecompost.it",
    "telecompec.it",
    "pec-email.com",
    "postacert.it.net",
    "asmecal.it",
    "asmecam.it",
    "asmez.it",
    "gespec.it",
    "twtcert.it",
    "pec.intred.it",
    "pec.basilicatanet.it",
    "mypec.eu",
    "asmepec.it",
];

pub(crate) fn is_pec_mx_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if has_pec_label(&host) {
        return true;
    }
    PEC_MX_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

pub(crate) fn is_pec_mx_hosts(hosts: &[String]) -> bool {
    hosts.iter().any(|host| is_pec_mx_host(host))
}

/// True when the address pattern matches or any MX host is a known PEC provider.
pub(crate) fn is_pec(email: &str, mx_hosts: &[String]) -> bool {
    is_pec_email(email) || is_pec_mx_hosts(mx_hosts)
}

pub(crate) fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn header_role(value: &str) -> Option<HeaderRole> {
    let normalized = normalize_key(value);
    if normalized.is_empty() {
        return None;
    }

    if normalized.contains("tipomail") || normalized.contains("tipoemail") {
        return Some(HeaderRole::EmailKind);
    }
    if normalized.contains("pec")
        || normalized.contains("postaelettronicacertificata")
        || normalized.contains("emailcertificata")
    {
        return Some(HeaderRole::Pec);
    }
    if [
        "source",
        "sorgente",
        "fonte",
        "url",
        "urlsorgente",
        "urlorigine",
        "urlcaselladiposta",
        "provenienza",
    ]
    .contains(&normalized.as_str())
    {
        return Some(HeaderRole::Source);
    }
    if normalized == "prov" || normalized.contains("provincia") {
        return Some(HeaderRole::Provincia);
    }
    if normalized.contains("regione") {
        return Some(HeaderRole::Regione);
    }
    if normalized.contains("comune") {
        return Some(HeaderRole::Comune);
    }
    if normalized.contains("email")
        || normalized == "mail"
        || normalized.starts_with("mailresp")
        || normalized
            .strip_prefix("mail")
            .is_some_and(|suffix| suffix.chars().all(|character| character.is_ascii_digit()))
        || normalized == "indirizzopostaelettronica"
    {
        return Some(HeaderRole::Email);
    }
    if normalized.contains("sitoweb")
        || normalized == "sito"
        || normalized == "website"
        || normalized == "web"
    {
        return Some(HeaderRole::Website);
    }
    if normalized == "tipo"
        || normalized.contains("tipologia")
        || normalized.contains("ordinescuola")
        || normalized.contains("gradoscuola")
        || normalized.contains("caratteristicascuola")
    {
        return Some(HeaderRole::SchoolType);
    }
    if normalized.contains("denominazione")
        || normalized == "descrizione"
        || normalized == "nomeresp"
        || normalized == "cognresp"
    {
        return Some(HeaderRole::Denomination);
    }
    if normalized.contains("indirizzo") {
        return Some(HeaderRole::Address);
    }
    if normalized.contains("telefono") || normalized == "tel" || normalized.starts_with("telresp") {
        return Some(HeaderRole::Phone);
    }
    None
}

pub(crate) fn is_blacklist_file(file: &ImportFile) -> bool {
    std::path::Path::new(&file.name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().trim().to_lowercase())
        .is_some_and(|stem| stem == "blacklist" || stem == "cancellami")
}

pub(crate) fn display_name(file: &ImportFile) -> String {
    if file.relative_path.is_empty() {
        file.name.clone()
    } else {
        file.relative_path.clone()
    }
}
