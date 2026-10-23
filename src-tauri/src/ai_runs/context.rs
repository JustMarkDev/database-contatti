//! Classification context building and model result parsing/validation.

use super::*;

pub(crate) fn classification_hash(
    _contact: &ContactRecord,
    _model: &str,
    _thinking: bool,
    _context_size: usize,
    mode: &str,
    catalog_version: &str,
    normalized_input: &str,
) -> String {
    let policy = if mode == "geography" {
        "geography-v2"
    } else {
        "taxonomy-v9"
    };
    let digest = simple_hash(
        &serde_json::json!({
            "policy": policy,
            "input": normalized_input,
            "catalog": catalog_version
        })
        .to_string(),
    );
    format!("ai:{digest}")
}

pub(crate) fn adaptive_context_size(requested: usize, source_context: &str) -> usize {
    let requested = requested.clamp(2048, 8192);
    if requested >= 8192 || source_context.chars().count() > 7000 {
        8192
    } else {
        4096.min(requested.max(2048))
    }
}

pub(crate) fn clean_source_path(raw_path: &str) -> String {
    let trimmed = raw_path.trim().replace('\\', "/");
    let path = std::path::Path::new(&trimmed);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned();
    let components: Vec<&str> = trimmed
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if let Some(root_index) = components
        .iter()
        .rposition(|c| c.eq_ignore_ascii_case("email"))
    {
        let relative = components[root_index + 1..].join("/");
        if !relative.is_empty() {
            return relative;
        }
    }
    if components.len() >= 2 {
        components[components.len() - 2..].join("/")
    } else if !name.is_empty() {
        name
    } else {
        trimmed
    }
}

fn is_internal_or_technical_field(key: &str) -> bool {
    let trimmed = key.trim();
    if trimmed.is_empty() || trimmed.starts_with('_') {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "id" | "import_id"
            | "source_row_id"
            | "source_row"
            | "row_id"
            | "parser_id"
            | "contact_id"
            | "row_number"
            | "source_file"
            | "source_path"
            | "source_sheet"
            | "source_url"
            | "sheet_name"
            | "sheet"
            | "row"
            | "line"
            | "raw_context"
            | "category"
            | "subcategory"
            | "categoria"
            | "sottocategoria"
            | "category_id"
            | "subcategory_id"
            | "status"
            | "stato"
            | "validation_notes"
            | "parse_status"
            | "error_message"
            | "confidence"
            | "classification_confidence"
            | "method"
            | "classification_method"
            | "evidence"
            | "classification_evidence"
            | "contact_kind"
            | "tipo_contatto"
            | "is_pec"
            | "pec"
            | "tipo_email"
            | "needs_review"
            | "classification_hash"
            | "candidate_count"
            | "created_at"
            | "updated_at"
            | "resolved_at"
            | "imported_at"
    )
}

pub(crate) fn canonicalize_field_key(key: &str) -> String {
    let lower = key.trim().to_ascii_lowercase();
    match lower.as_str() {
        "denominazione dell'istituto"
        | "denominazione istituto"
        | "denominazione"
        | "ragione sociale"
        | "ragione_sociale"
        | "nome ente"
        | "nome scuola"
        | "struttura"
        | "istituto"
        | "des_ou"
        | "descrizione"
        | "intestazione" => "Denominazione".to_string(),

        "indirizzo (via + numero civico)"
        | "indirizzo"
        | "via"
        | "ubicazione"
        | "sede"
        | "indirizzo_sede" => "Indirizzo".to_string(),

        "codice postale" | "cap" | "c.a.p." | "c.a.p" | "codice_postale" => "CAP".to_string(),

        "comune" | "descrizione comune" | "descrizionecomune" | "comune sede" | "città"
        | "citta" | "luogo" => "Comune".to_string(),

        "provincia" | "prov" | "sigla provincia" | "sigla_provincia" | "provincia_sede" => {
            "Provincia".to_string()
        }

        "regione" | "regione_sede" => "Regione".to_string(),
        "telefono" | "tel" | "recapito telefonico" => "Telefono".to_string(),
        "pec" | "posta certificata" | "posta_certificata" => "PEC".to_string(),
        "email" | "e-mail" | "mail" | "mail1" | "mail2" | "e_mail" => "Email".to_string(),
        "sito" | "sito web" | "sito_web" | "sito internet" | "url" | "website" => {
            "Sito Web".to_string()
        }
        "codice scuola" | "codice meccanografico" | "codicemeccanografico" | "cod_ou" => {
            "Codice Meccanografico".to_string()
        }
        "tipologia istituto - denominazione"
        | "tipologia"
        | "tipologia istat"
        | "tipo scuola"
        | "tipo" => "Tipologia".to_string(),
        "statale/paritaria" | "statale paritaria" => "Statale o Paritaria".to_string(),
        _ => {
            if lower.starts_with("colonna_")
                || lower.starts_with("col_")
                || lower.starts_with("field_")
                || lower.starts_with("campo_")
                || lower.starts_with("f_")
            {
                "Dettaglio".to_string()
            } else {
                key.trim().to_string()
            }
        }
    }
}

pub(crate) fn normalize_field_value(key: &str, text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if key == "Email"
        || key == "PEC"
        || key == "CAP"
        || key == "Codice Meccanografico"
        || key == "Telefono"
        || trimmed.contains('@')
    {
        return trimmed.to_string();
    }
    let has_lower = trimmed.chars().any(|c| c.is_lowercase());
    let upper_count = trimmed.chars().filter(|c| c.is_uppercase()).count();
    if !has_lower && upper_count >= 3 {
        title_case_italian(trimmed)
    } else {
        trimmed.to_string()
    }
}

fn title_case_italian(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len() + 8);
    let chars: Vec<char> = text.chars().collect();
    for (idx, &c) in chars.iter().enumerate() {
        normalized.push(c);
        if c == '.' && idx + 1 < chars.len() && chars[idx + 1].is_alphabetic() {
            normalized.push(' ');
        }
    }
    let words: Vec<&str> = normalized.split_whitespace().collect();
    let mut result = Vec::with_capacity(words.len());
    for (i, word) in words.iter().enumerate() {
        let clean = word.trim();
        if clean.is_empty() {
            continue;
        }
        let upper = clean.to_ascii_uppercase();
        if matches!(
            upper.as_str(),
            "ITIS"
                | "CFP"
                | "CPIA"
                | "ASL"
                | "ASST"
                | "IRCCS"
                | "UNEP"
                | "APT"
                | "IAT"
                | "CAI"
                | "ANA"
                | "NOI"
                | "AVIS"
                | "APS"
                | "ONLUS"
                | "RM"
                | "MI"
                | "TO"
                | "NA"
                | "FI"
                | "PA"
                | "BA"
                | "BO"
                | "GE"
                | "VR"
                | "PD"
                | "BS"
                | "I"
                | "II"
                | "III"
                | "IV"
                | "V"
        ) {
            result.push(upper);
            continue;
        }
        let lower = clean.to_ascii_lowercase();
        if i > 0
            && matches!(
                lower.as_str(),
                "di" | "a"
                    | "da"
                    | "in"
                    | "con"
                    | "su"
                    | "per"
                    | "tra"
                    | "fra"
                    | "il"
                    | "lo"
                    | "la"
                    | "i"
                    | "gli"
                    | "le"
                    | "un"
                    | "uno"
                    | "una"
                    | "del"
                    | "dello"
                    | "della"
                    | "dei"
                    | "degli"
                    | "delle"
                    | "e"
                    | "ed"
            )
        {
            result.push(lower);
            continue;
        }
        if let Some((prefix, suffix)) = clean.split_once('\'') {
            let pref_lower = prefix.to_ascii_lowercase();
            let mut suff_chars = suffix.chars();
            let suff_title = match suff_chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(suff_chars.flat_map(|c| c.to_lowercase()))
                    .collect::<String>(),
                None => String::new(),
            };
            if i > 0
                && matches!(
                    pref_lower.as_str(),
                    "dell" | "dall" | "all" | "nell" | "sull" | "l" | "un"
                )
            {
                result.push(format!("{pref_lower}'{suff_title}"));
            } else {
                let mut pref_chars = prefix.chars();
                let pref_title = match pref_chars.next() {
                    Some(first) => first
                        .to_uppercase()
                        .chain(pref_chars.flat_map(|c| c.to_lowercase()))
                        .collect::<String>(),
                    None => String::new(),
                };
                result.push(format!("{pref_title}'{suff_title}"));
            }
            continue;
        }
        let mut chars = clean.chars();
        let title = match chars.next() {
            Some(first) => first
                .to_uppercase()
                .chain(chars.flat_map(|c| c.to_lowercase()))
                .collect::<String>(),
            None => String::new(),
        };
        result.push(title);
    }
    result.join(" ")
}

pub fn classification_source_context(db: &Connection, contact_id: &str) -> Result<String, String> {
    let mut records = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let canonical_query = db.query_row(
        "SELECT email, denomination, address, postal_code, comune, provincia, regione, origin_url, is_pec FROM contacts WHERE id = ?1",
        params![contact_id],
        |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<i64>>(8)?.unwrap_or(0),
        )),
    );

    if let Ok((email, denom, addr, cap, comune, prov, reg, origin_url, is_pec)) = canonical_query {
        let mut canon_fields = serde_json::Map::new();
        if !email.trim().is_empty() {
            let email_key = if is_pec == 1 { "PEC" } else { "Email" };
            canon_fields.insert(
                email_key.to_string(),
                serde_json::Value::String(email.trim().to_string()),
            );
        }
        if let Some(denom) = denom.filter(|s| !s.trim().is_empty()) {
            canon_fields.insert(
                "Denominazione".to_string(),
                serde_json::Value::String(normalize_field_value("Denominazione", &denom)),
            );
        }
        if let Some(addr) = addr.filter(|s| !s.trim().is_empty()) {
            canon_fields.insert(
                "Indirizzo".to_string(),
                serde_json::Value::String(normalize_field_value("Indirizzo", &addr)),
            );
        }
        if let Some(cap) = cap.filter(|s| !s.trim().is_empty()) {
            canon_fields.insert(
                "CAP".to_string(),
                serde_json::Value::String(cap.trim().to_string()),
            );
        }
        if let Some(comune) = comune.filter(|s| !s.trim().is_empty()) {
            canon_fields.insert(
                "Comune".to_string(),
                serde_json::Value::String(normalize_field_value("Comune", &comune)),
            );
        }
        if let Some(prov) = prov.filter(|s| !s.trim().is_empty()) {
            canon_fields.insert(
                "Provincia".to_string(),
                serde_json::Value::String(prov.trim().to_uppercase()),
            );
        }
        if let Some(reg) = reg.filter(|s| !s.trim().is_empty()) {
            canon_fields.insert(
                "Regione".to_string(),
                serde_json::Value::String(normalize_field_value("Regione", &reg)),
            );
        }
        if let Some(url) = origin_url.as_ref().filter(|s| !s.trim().is_empty()) {
            canon_fields.insert(
                "Sito Web".to_string(),
                serde_json::Value::String(url.trim().to_string()),
            );
        }
        if !canon_fields.is_empty() {
            let key = serde_json::to_string(&canon_fields).unwrap_or_default();
            seen.insert(key);
            records.push(serde_json::json!({
                "source_id": "canonical_record",
                "source_path": "Database/canonical_contact",
                "fields": canon_fields,
                "source_url": origin_url.filter(|v| !v.trim().is_empty()),
            }));
        }
    }

    let mut stmt = db
        .prepare(
            "SELECT cs.id,cs.source_file,cs.source_url,
                    coalesce(sr.raw_context,cs.raw_context),cs.source_row_id
             FROM contact_sources cs
             LEFT JOIN source_rows sr ON sr.id=cs.source_row_id
             WHERE cs.contact_id=?1
             ORDER BY cs.source_file,cs.source_sheet,cs.source_row,cs.id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![contact_id], |r| {
            let raw: serde_json::Value = serde_json::from_str(&r.get::<_, String>(3)?)
                .unwrap_or_else(|_| serde_json::json!({}));
            let mut fields = serde_json::Map::new();
            if let Some(map) = raw.as_object() {
                for (key, value) in map {
                    let mut name = canonicalize_field_key(key);
                    let text = match value {
                        serde_json::Value::String(s) => normalize_field_value(&name, s),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        _ => String::new(),
                    };
                    if name == "Dettaglio" {
                        if text.contains('@') && !text.contains(' ') {
                            name = "Email".to_string();
                        } else if text.starts_with("http://") || text.starts_with("https://") {
                            name = "Sito Web".to_string();
                        } else if text.len() == 5 && text.chars().all(|c| c.is_ascii_digit()) {
                            name = "CAP".to_string();
                        }
                    }
                    if !text.is_empty() && !is_internal_or_technical_field(&name) {
                        fields.insert(name, serde_json::Value::String(text));
                    }
                }
            }
            let raw_source_path = r.get::<_, String>(1)?;
            let source_path = clean_source_path(&raw_source_path);
            Ok(serde_json::json!({
                "source_id": r.get::<_, String>(0)?,
                "source_path": source_path,
                "fields": fields,
                "source_url": r.get::<_, Option<String>>(2)?.filter(|v| !v.trim().is_empty()),
            }))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let value = row.map_err(|e| e.to_string())?;
        let fields = &value["fields"];
        let has_fields = fields.as_object().is_some_and(|m| !m.is_empty());
        let key = serde_json::to_string(fields).unwrap_or_default();
        if has_fields && seen.insert(key) {
            records.push(value);
        }
    }
    serde_json::to_string(&records).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GeographicPlaceClaim {
    pub observed_place: String,
    #[serde(default)]
    pub level_hint: Option<String>,
    #[serde(default)]
    pub evidence: Vec<crate::GroundedSourceReference>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct GeographicResult {
    pub outcome: String,
    #[serde(default)]
    pub claims: Vec<GeographicPlaceClaim>,
    #[serde(default)]
    pub summary: Option<String>,
}

pub(crate) fn parse_geography_result(raw: &str) -> Result<GeographicResult, String> {
    serde_json::from_str(raw).map_err(|err| format!("JSON geografia non valido: {err}"))
}

pub(crate) fn validate_geography_result(
    result: &GeographicResult,
    source_context: &str,
) -> Result<(), String> {
    let sources: serde_json::Value =
        serde_json::from_str(source_context).map_err(|_| "input.sources.invalid".to_owned())?;
    let mut errors = Vec::new();
    if result.outcome == "insufficient" && !result.claims.is_empty() {
        errors.push("geography.insufficient_claims".to_owned());
    }
    if result.outcome != "insufficient" && result.claims.is_empty() {
        errors.push("geography.place.missing".to_owned());
    }
    if result.outcome == "conflict" && result.claims.len() < 2 {
        errors.push("geography.conflict.claims.count".to_owned());
    }
    for claim in &result.claims {
        if claim.observed_place.trim().is_empty() {
            errors.push("geography.place.empty".to_owned());
        }
        for evidence in &claim.evidence {
            let grounded = sources.as_array().into_iter().flatten().any(|source| {
                source["source_id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(evidence.source_id.trim()))
                    && source["fields"]
                        .as_object()
                        .and_then(|f| {
                            f.iter()
                                .find(|(k, _)| k.eq_ignore_ascii_case(evidence.field.trim()))
                        })
                        .is_some_and(|(_, v)| {
                            normalize_grounding(v.as_str().unwrap_or(""))
                                .contains(&normalize_grounding(&evidence.quote))
                        })
            });
            if !grounded {
                errors.push("evidence.quote.not_found".to_owned());
            }
        }
        if claim.evidence.is_empty() {
            errors.push("evidence.required".to_owned());
        }
    }
    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(","))
    }
}

pub(crate) fn parse_taxonomy_result(raw: &str) -> Result<TaxonomyResult, String> {
    serde_json::from_str(raw).map_err(|err| format!("JSON tassonomia non valido: {err}"))
}

pub(super) fn taxonomy_slug(s: &str) -> String {
    let slug: String = s
        .chars()
        .filter_map(|c| match c {
            'à' | 'á' | 'â' | 'ä' => Some('a'),
            'è' | 'é' | 'ê' | 'ë' => Some('e'),
            'ì' | 'í' | 'î' | 'ï' => Some('i'),
            'ò' | 'ó' | 'ô' | 'ö' => Some('o'),
            'ù' | 'ú' | 'û' | 'ü' => Some('u'),
            c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    match slug.as_str() {
        "forzedellordine" | "lawenforcement" => "law_enforcement".to_owned(),
        other => other.to_owned(),
    }
}

pub(crate) fn normalize_grounding(value: &str) -> String {
    value
        .chars()
        .filter_map(|c| match c {
            'à' | 'á' | 'â' | 'ä' => Some('a'),
            'è' | 'é' | 'ê' | 'ë' => Some('e'),
            'ì' | 'í' | 'î' | 'ï' => Some('i'),
            'ò' | 'ó' | 'ô' | 'ö' => Some('o'),
            'ù' | 'ú' | 'û' | 'ü' => Some('u'),
            c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

pub(crate) fn validate_taxonomy_result(
    result: &TaxonomyResult,
    taxonomy: &[(String, Vec<String>)],
    source_context: &str,
) -> Result<(), String> {
    let sources: serde_json::Value =
        serde_json::from_str(source_context).map_err(|_| "input.sources.invalid".to_owned())?;
    let mut errors = Vec::new();
    if result.candidates.len() > 3 {
        errors.push("candidates.count.max".to_owned());
    }
    match result.outcome {
        crate::TaxonomyOutcome::Proposal if result.candidates.len() != 1 => {
            errors.push("proposal.candidates.exactly_one".to_owned())
        }
        crate::TaxonomyOutcome::Conflict if !(2..=3).contains(&result.candidates.len()) => {
            errors.push("conflict.candidates.count".to_owned())
        }
        crate::TaxonomyOutcome::Insufficient if !result.candidates.is_empty() => {
            errors.push("insufficient.candidates.empty".to_owned())
        }
        crate::TaxonomyOutcome::TaxonomyGap if result.gap.is_none() => {
            errors.push("taxonomy_gap.details.required".to_owned())
        }
        _ => {}
    }
    if !matches!(result.outcome, crate::TaxonomyOutcome::TaxonomyGap) && result.gap.is_some() {
        errors.push("gap.unexpected".to_owned());
    }
    for candidate in &result.candidates {
        let category = taxonomy
            .iter()
            .find(|(key, _)| taxonomy_slug(key) == taxonomy_slug(&candidate.category_key));
        if category.is_none() {
            errors.push("taxonomy.category.invalid".to_owned());
        }
        if let Some(sub) = &candidate.subcategory_key
            && category
                .map(|(_, subs)| !subs.iter().any(|x| taxonomy_slug(x) == taxonomy_slug(sub)))
                .unwrap_or(true)
        {
            errors.push("taxonomy.subcategory.invalid".to_owned());
        }
        if candidate.category_evidence.is_empty() {
            errors.push("evidence.category.required".to_owned());
        }
        if candidate.subcategory_key.is_some() {
            if candidate.subcategory_grade.is_none() {
                errors.push("grade.subcategory.required".to_owned());
            }
            if candidate.subcategory_evidence.is_empty() {
                errors.push("evidence.subcategory.required".to_owned());
            }
        }
        for evidence in candidate
            .category_evidence
            .iter()
            .chain(candidate.subcategory_evidence.iter())
        {
            if normalize_grounding(&evidence.quote).is_empty() {
                errors.push("evidence.quote.empty".to_owned());
                continue;
            }
            let grounded = sources.as_array().into_iter().flatten().any(|source| {
                source["source_id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(evidence.source_id.trim()))
                    && source["fields"]
                        .as_object()
                        .and_then(|fields| {
                            fields.iter().find(|(field, _)| {
                                field.trim().eq_ignore_ascii_case(evidence.field.trim())
                            })
                        })
                        .is_some_and(|(_, value)| {
                            normalize_grounding(value.as_str().unwrap_or(""))
                                .contains(&normalize_grounding(&evidence.quote))
                        })
            });
            if !grounded {
                errors.push("evidence.quote.not_found".to_owned());
            }
        }
    }
    if let Some(gap) = &result.gap {
        if gap.proposed_display_name.trim().is_empty() && gap.proposed_definition.trim().is_empty()
        {
            errors.push("taxonomy_gap.proposal.required".to_owned());
        }
        let proposed_slug = taxonomy_slug(&gap.proposed_display_name);
        let duplicate = taxonomy.iter().any(|(category, subcategories)| {
            taxonomy_slug(category) == proposed_slug
                || subcategories
                    .iter()
                    .any(|subcategory| taxonomy_slug(subcategory) == proposed_slug)
        });
        if duplicate {
            errors.push("taxonomy_gap.proposal.already_exists".to_owned());
        }
        if !matches!(gap.level.as_str(), "category" | "subcategory") {
            errors.push("taxonomy_gap.level.invalid".to_owned());
        }
        let nearest_category = gap.nearest_category_key.as_deref().and_then(|key| {
            taxonomy
                .iter()
                .find(|(active, _)| taxonomy_slug(active) == taxonomy_slug(key))
        });
        if gap.nearest_category_key.is_some() && nearest_category.is_none() {
            errors.push("taxonomy_gap.nearest_category.invalid".to_owned());
        }
        if let Some(nearest_subcategory) = gap.nearest_subcategory_key.as_deref()
            && nearest_category.is_none_or(|(_, subs)| {
                !subs
                    .iter()
                    .any(|key| taxonomy_slug(key) == taxonomy_slug(nearest_subcategory))
            })
        {
            errors.push("taxonomy_gap.nearest_subcategory.invalid".to_owned());
        }
        if gap.level == "subcategory" {
            match gap.parent_category_key.as_deref() {
                Some(parent)
                    if taxonomy
                        .iter()
                        .any(|(key, _)| taxonomy_slug(key) == taxonomy_slug(parent)) => {}
                Some(_) => errors.push("taxonomy_gap.parent.invalid".to_owned()),
                None => {}
            }
        } else if gap.parent_category_key.is_some() {
            errors.push("taxonomy_gap.parent.unexpected".to_owned());
        }
    }
    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(","))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeterministicResult {
    pub category: &'static str,
    pub subcategory: Option<&'static str>,
    pub comune: Option<String>,
    pub provincia: Option<String>,
    pub regione: Option<String>,
    pub reason: String,
}
