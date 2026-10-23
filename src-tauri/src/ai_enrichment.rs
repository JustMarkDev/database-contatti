use crate::ContactRecord;
use crate::contacts::row_to_contact;
use rusqlite::{Connection, params};

const AI_CONTACT_COLUMNS: &str = "id,email,email_local_part,email_domain,email_tld,status,validation_notes,source_file,category,subcategory,contact_kind,paese,is_pec,comune,provincia,regione,website,source_url,category_id,subcategory_id,classification_method,classification_confidence,classification_evidence,needs_review,classification_hash,macroarea,taxonomy_version_id,(SELECT semantic_version FROM taxonomy_versions WHERE id=contacts.taxonomy_version_id),classification_state,classification_state_reason";

/// Contact ids queued by an enrichment mode. Only exportable contacts: paying for
/// suppressed, blacklisted-pending or unroutable addresses buys nothing.
pub(crate) fn load_ai_candidate_ids(db: &Connection, mode: &str) -> Result<Vec<String>, String> {
    let (join, filter) = match mode {
        "geography" => (
            "",
            "AND ((comune IS NULL OR trim(comune)='') OR (provincia IS NULL OR trim(provincia)='')
                  OR (regione IS NULL OR trim(regione)=''))",
        ),
        "audit" => (
            "",
            "AND lower(trim(category)) NOT IN ('', 'varie', 'unknown', 'sconosciuto')",
        ),
        "full" => ("", ""),
        "deterministic_audit" => (
            "",
            "AND classification_method='deterministic'
             AND classification_hash LIKE 'deterministic:%'
             AND classification_state='classified'
             AND subcategory IS NOT NULL AND trim(subcategory)<>''",
        ),
        "follow_up" => (
            "JOIN classification_follow_up_queue q ON q.contact_id=contacts.id",
            "",
        ),
        _ => (
            "",
            "AND (lower(trim(category)) IN ('', 'varie', 'unknown', 'sconosciuto')
                  OR subcategory IS NULL OR lower(trim(subcategory)) IN ('', 'varie', 'unknown', 'sconosciuto', 'dato misto o da revisionare')
                  OR lower(trim(subcategory)) IN (
                      'altro ente formativo', 'altro ente pubblico', 'altro ente sanitario',
                      'associazione generica', 'altro ente religioso', 'altro ente culturale',
                      'altro ente turistico'))",
        ),
    };
    let mut stmt = db
        .prepare(&format!(
            "SELECT contacts.id FROM contacts {join}
             WHERE contacts.is_exportable=1 AND contacts.contact_kind <> 'personale' {filter}
             ORDER BY contacts.email"
        ))
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

pub(crate) fn load_ai_candidate(
    db: &Connection,
    contact_id: &str,
) -> Result<ContactRecord, String> {
    db.query_row(
        &format!("SELECT {AI_CONTACT_COLUMNS} FROM contacts WHERE id=?1"),
        params![contact_id],
        row_to_contact,
    )
    .map_err(|err| err.to_string())
}

#[cfg(test)]
pub(crate) fn clean_ai_option(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_owned()).filter(|value| {
        !value.is_empty()
            && !value.eq_ignore_ascii_case("null")
            && !value.eq_ignore_ascii_case("sconosciuto")
    })
}

#[cfg(test)]
pub(crate) fn normalize_ai_field(field: &str, value: String) -> String {
    if field == "provincia"
        && let Some(name) = expand_province(&value)
    {
        return name.to_owned();
    }
    if matches!(field, "comune" | "provincia" | "regione") {
        to_title_case(&value)
    } else {
        value
    }
}

#[cfg(test)]
pub(crate) fn to_title_case(value: &str) -> String {
    to_italian_title_case(value)
}

#[cfg(test)]
pub(crate) fn to_italian_title_case(value: &str) -> String {
    let value = value.trim().replace(['’', '‘', '´', '`'], "'");
    if value.is_empty() {
        return String::new();
    }
    let words: Vec<&str> = value.split_whitespace().collect();
    let mut result_words = Vec::new();

    for word in words.iter() {
        let word_lower = word.to_lowercase();

        if let Some((prefix, suffix)) = word_lower.split_once('\'') {
            let clean_prefix = if matches!(
                prefix,
                "d" | "dell" | "dall" | "nell" | "all" | "coll" | "sull" | "un"
            ) {
                prefix.to_lowercase()
            } else {
                capitalize_token(prefix)
            };
            let clean_suffix = capitalize_token(suffix);
            result_words.push(format!("{clean_prefix}'{clean_suffix}"));
            continue;
        }

        if word_lower.contains('-') {
            let hyphen_parts: Vec<String> = word_lower.split('-').map(capitalize_token).collect();
            result_words.push(hyphen_parts.join("-"));
            continue;
        }

        result_words.push(capitalize_token(&word_lower));
    }

    result_words.join(" ")
}

#[cfg(test)]
fn capitalize_token(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

pub(crate) fn is_cadastral_code(value: &str) -> bool {
    let trimmed = value.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    chars.len() == 4
        && chars[0].is_ascii_alphabetic()
        && chars[1..].iter().all(|c| c.is_ascii_digit())
}

pub(crate) fn is_literal_comune(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() < 2 || is_cadastral_code(trimmed) {
        return false;
    }
    if trimmed
        .chars()
        .all(|c| c.is_ascii_digit() || c.is_ascii_whitespace() || c == '-')
    {
        return false;
    }
    trimmed.chars().any(|c| c.is_alphabetic())
}

pub(crate) fn normalize_extracted_comune(value: &str) -> Option<String> {
    let s = value.trim().replace(['’', '‘', '´', '`'], "'");
    if !is_literal_comune(&s) {
        return None;
    }
    let s_fixed = if s.contains('?') {
        s.replace('?', "o")
    } else {
        s
    };
    let mut lower = s_fixed.to_lowercase();
    for prefix in [
        "comune di ",
        "comune d'",
        "municipio di ",
        "citta' di ",
        "città di ",
        "città metropolitana di ",
        "citta' metropolitana di ",
        "unione di comuni ",
        "unione dei comuni ",
    ] {
        if lower.starts_with(prefix) {
            lower = lower[prefix.len()..].trim().to_owned();
            break;
        }
    }

    if let Some((first, _)) = lower.split_once('*') {
        lower = first.trim().to_owned();
    }
    if let Some((first, _)) = lower.split_once('/') {
        lower = first.trim().to_owned();
    }

    let mut s_clean = lower.as_str();

    if let Some(idx) = s_clean.rfind('(')
        && s_clean.ends_with(')')
    {
        let head = s_clean[..idx].trim();
        if head.len() >= 2 {
            s_clean = head;
        }
    }
    if let Some(idx) = s_clean.rfind('[')
        && s_clean.ends_with(']')
    {
        let head = s_clean[..idx].trim();
        if head.len() >= 2 {
            s_clean = head;
        }
    }

    if let Some((head, suffix)) = s_clean.rsplit_once('-') {
        let trimmed_suffix = suffix.trim();
        if trimmed_suffix.len() == 2 && trimmed_suffix.chars().all(|c| c.is_ascii_alphabetic()) {
            s_clean = head.trim();
        }
    }

    if s_clean.len() < 2 || !s_clean.chars().any(|c| c.is_alphabetic()) {
        return None;
    }

    let norm_key = crate::gazetteer::norm(s_clean);
    if let Some(canonical) = crate::gazetteer::GAZETTEER_INDEX
        .municipalities
        .get(&norm_key)
    {
        return Some((*canonical).to_owned());
    }

    if norm_key.len() >= 14 {
        for (k, canonical) in &crate::gazetteer::GAZETTEER_INDEX.municipalities {
            if k.starts_with(&norm_key) && k.len() > norm_key.len() {
                return Some((*canonical).to_owned());
            }
        }
    }

    None
}

pub(crate) fn normalize_extracted_regione(value: &str) -> Option<String> {
    let s = value.trim().replace(['’', '‘', '´', '`'], "'");
    if s.is_empty() || s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let lower = s.to_lowercase();
    let cleaned = lower.strip_prefix("regione ").unwrap_or(&lower).trim();

    let norm_key = crate::gazetteer::norm(cleaned);
    if let Some(canonical) = crate::gazetteer::GAZETTEER_INDEX.regions.get(&norm_key) {
        return Some((*canonical).to_owned());
    }

    if norm_key.contains("valle") || norm_key.contains("aosta") || norm_key == "vda" {
        return Some("Valle d'Aosta".to_owned());
    }
    if norm_key.contains("trentino")
        || norm_key.contains("alto adige")
        || norm_key.contains("sudtirol")
    {
        return Some("Trentino Alto Adige".to_owned());
    }
    if norm_key.contains("friuli") || norm_key == "fvg" {
        return Some("Friuli Venezia Giulia".to_owned());
    }
    if norm_key.contains("emilia") || norm_key.contains("romagna") {
        return Some("Emilia Romagna".to_owned());
    }
    if norm_key.contains("abruzzo") || norm_key.contains("abruzzi") {
        return Some("Abruzzo".to_owned());
    }
    if norm_key.contains("puglia") || norm_key.contains("puglie") {
        return Some("Puglia".to_owned());
    }
    if norm_key.contains("basilicata") || norm_key.contains("lucania") {
        return Some("Basilicata".to_owned());
    }
    if norm_key.contains("lombardia") {
        return Some("Lombardia".to_owned());
    }
    if norm_key.contains("piemonte") {
        return Some("Piemonte".to_owned());
    }
    if norm_key.contains("veneto") {
        return Some("Veneto".to_owned());
    }
    if norm_key.contains("liguria") {
        return Some("Liguria".to_owned());
    }
    if norm_key.contains("toscana") {
        return Some("Toscana".to_owned());
    }
    if norm_key.contains("umbria") {
        return Some("Umbria".to_owned());
    }
    if norm_key.contains("marche") {
        return Some("Marche".to_owned());
    }
    if norm_key.contains("lazio") {
        return Some("Lazio".to_owned());
    }
    if norm_key.contains("campania") {
        return Some("Campania".to_owned());
    }
    if norm_key.contains("molise") {
        return Some("Molise".to_owned());
    }
    if norm_key.contains("calabria") {
        return Some("Calabria".to_owned());
    }
    if norm_key.contains("sicilia") {
        return Some("Sicilia".to_owned());
    }
    if norm_key.contains("sardegna") {
        return Some("Sardegna".to_owned());
    }

    None
}

pub(crate) fn normalize_extracted_province(value: String) -> String {
    let s = value.trim().replace(['’', '‘', '´', '`'], "'");
    if s.is_empty() || s.chars().all(|c| c.is_ascii_digit()) {
        return String::new();
    }
    let lower_s = s.to_lowercase();
    let cleaned = if let Some(rest) = lower_s.strip_prefix("provincia di ") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("provincia d'") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("provincia de ") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("provincia del ") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("provincia dell'") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("provincia ") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("province of ") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("province de ") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("province d'") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("prov. di ") {
        rest
    } else if let Some(rest) = lower_s.strip_prefix("prov. ") {
        rest
    } else {
        lower_s.as_str()
    }
    .trim();

    let norm_key = crate::gazetteer::norm(cleaned);

    if matches!(
        norm_key.as_str(),
        "centro" | "isole" | "nord est" | "nord ovest" | "sud"
    ) {
        return String::new();
    }

    if let Some(exp) = expand_province(cleaned) {
        return exp.to_owned();
    }
    if let Some(exp) = expand_province(&s) {
        return exp.to_owned();
    }

    if let Some(canonical) = crate::gazetteer::GAZETTEER_INDEX.provinces.get(&norm_key) {
        return (*canonical).to_owned();
    }

    if norm_key.starts_with("pesaro") || norm_key.contains("urbin") {
        return "Pesaro e Urbino".to_owned();
    }
    if norm_key.starts_with("reggio cal")
        || norm_key.starts_with("reggio di cal")
        || norm_key.starts_with("reggio kal")
    {
        return "Reggio Calabria".to_owned();
    }
    if norm_key.starts_with("reggio em") || norm_key.starts_with("reggio nell em") {
        return "Reggio Emilia".to_owned();
    }
    if norm_key.starts_with("barletta")
        || norm_key.contains("andria")
        || norm_key.contains("andr")
        || (norm_key.contains("trani") && !norm_key.contains("strani"))
    {
        return "Barletta-Andria-Trani".to_owned();
    }
    if norm_key.starts_with("massa") || norm_key.contains("carrar") {
        return "Massa-Carrara".to_owned();
    }
    if norm_key.starts_with("verban") || norm_key.contains("cusio") || norm_key.contains("ossola") {
        return "Verbano-Cusio-Ossola".to_owned();
    }
    if norm_key.starts_with("monza") || norm_key.contains("brianz") {
        return "Monza e Brianza".to_owned();
    }
    if norm_key.starts_with("forl") || norm_key.contains("cesen") {
        return "Forlì-Cesena".to_owned();
    }
    if norm_key.contains("fermo") {
        return "Fermo".to_owned();
    }
    if norm_key.contains("ascoli") {
        return "Ascoli Piceno".to_owned();
    }
    if norm_key.contains("spezia") {
        return "La Spezia".to_owned();
    }
    if norm_key.contains("vibo") {
        return "Vibo Valentia".to_owned();
    }
    if norm_key.contains("aquila") {
        return "L'Aquila".to_owned();
    }
    if norm_key.contains("bolzano") || norm_key.contains("bozen") || norm_key.contains("alto adige")
    {
        return "Bolzano".to_owned();
    }
    if norm_key.contains("aosta")
        || norm_key.contains("valle d aosta")
        || norm_key.contains("valle daosta")
    {
        return "Aosta".to_owned();
    }
    if norm_key.contains("carbonia")
        || norm_key.contains("iglesias")
        || norm_key.contains("medio campidano")
        || norm_key.contains("ogliastra")
        || norm_key.contains("sud sardegna")
    {
        return "Sud Sardegna".to_owned();
    }
    if norm_key.contains("olbia") || norm_key.contains("tempio") {
        return "Sassari".to_owned();
    }

    if let Some(canonical_comune) = crate::gazetteer::GAZETTEER_INDEX
        .municipalities
        .get(&norm_key)
    {
        for (prov, _) in crate::parsing::PROVINCE_REGIONS {
            if crate::gazetteer::norm(canonical_comune) == crate::gazetteer::norm(prov) {
                return (*prov).to_owned();
            }
        }
    }

    String::new()
}

pub(crate) fn expand_province(value: &str) -> Option<&'static str> {
    match value.trim().to_uppercase().as_str() {
        "AG" => Some("Agrigento"),
        "AL" => Some("Alessandria"),
        "AN" => Some("Ancona"),
        "AO" => Some("Aosta"),
        "AP" => Some("Ascoli Piceno"),
        "AQ" => Some("L'Aquila"),
        "AR" => Some("Arezzo"),
        "AT" => Some("Asti"),
        "AV" => Some("Avellino"),
        "BA" => Some("Bari"),
        "BG" => Some("Bergamo"),
        "BI" => Some("Biella"),
        "BL" => Some("Belluno"),
        "BN" => Some("Benevento"),
        "BO" => Some("Bologna"),
        "BR" => Some("Brindisi"),
        "BS" => Some("Brescia"),
        "BT" => Some("Barletta-Andria-Trani"),
        "BZ" => Some("Bolzano"),
        "CA" => Some("Cagliari"),
        "CB" => Some("Campobasso"),
        "CE" => Some("Caserta"),
        "CH" => Some("Chieti"),
        "CI" => Some("Sud Sardegna"),
        "CL" => Some("Caltanissetta"),
        "CN" => Some("Cuneo"),
        "CO" => Some("Como"),
        "CR" => Some("Cremona"),
        "CS" => Some("Cosenza"),
        "CT" => Some("Catania"),
        "CZ" => Some("Catanzaro"),
        "EN" => Some("Enna"),
        "FC" => Some("Forlì-Cesena"),
        "FE" => Some("Ferrara"),
        "FG" => Some("Foggia"),
        "FI" => Some("Firenze"),
        "FM" => Some("Fermo"),
        "FR" => Some("Frosinone"),
        "GE" => Some("Genova"),
        "GO" => Some("Gorizia"),
        "GR" => Some("Grosseto"),
        "IM" => Some("Imperia"),
        "IS" => Some("Isernia"),
        "KR" => Some("Crotone"),
        "LC" => Some("Lecco"),
        "LE" => Some("Lecce"),
        "LI" => Some("Livorno"),
        "LO" => Some("Lodi"),
        "LT" => Some("Latina"),
        "LU" => Some("Lucca"),
        "MB" => Some("Monza e Brianza"),
        "MC" => Some("Macerata"),
        "ME" => Some("Messina"),
        "MI" => Some("Milano"),
        "MN" => Some("Mantova"),
        "MO" => Some("Modena"),
        "MS" => Some("Massa-Carrara"),
        "MT" => Some("Matera"),
        "NA" => Some("Napoli"),
        "NO" => Some("Novara"),
        "NU" => Some("Nuoro"),
        "OG" => Some("Sud Sardegna"),
        "OR" => Some("Oristano"),
        "OT" => Some("Sassari"),
        "PA" => Some("Palermo"),
        "PC" => Some("Piacenza"),
        "PD" => Some("Padova"),
        "PE" => Some("Pescara"),
        "PG" => Some("Perugia"),
        "PI" => Some("Pisa"),
        "PN" => Some("Pordenone"),
        "PO" => Some("Prato"),
        "PR" => Some("Parma"),
        "PT" => Some("Pistoia"),
        "PU" => Some("Pesaro e Urbino"),
        "PV" => Some("Pavia"),
        "PZ" => Some("Potenza"),
        "RA" => Some("Ravenna"),
        "RC" => Some("Reggio Calabria"),
        "RE" => Some("Reggio Emilia"),
        "RG" => Some("Ragusa"),
        "RI" => Some("Rieti"),
        "RM" => Some("Roma"),
        "RN" => Some("Rimini"),
        "RO" => Some("Rovigo"),
        "SA" => Some("Salerno"),
        "SI" => Some("Siena"),
        "SO" => Some("Sondrio"),
        "SP" => Some("La Spezia"),
        "SR" => Some("Siracusa"),
        "SS" => Some("Sassari"),
        "SU" => Some("Sud Sardegna"),
        "SV" => Some("Savona"),
        "TA" => Some("Taranto"),
        "TE" => Some("Teramo"),
        "TN" => Some("Trento"),
        "TO" => Some("Torino"),
        "TP" => Some("Trapani"),
        "TR" => Some("Terni"),
        "TS" => Some("Trieste"),
        "TV" => Some("Treviso"),
        "UD" => Some("Udine"),
        "VA" => Some("Varese"),
        "VB" => Some("Verbano-Cusio-Ossola"),
        "VC" => Some("Vercelli"),
        "VE" => Some("Venezia"),
        "VI" => Some("Vicenza"),
        "VR" => Some("Verona"),
        "VS" => Some("Sud Sardegna"),
        "VT" => Some("Viterbo"),
        "VV" => Some("Vibo Valentia"),
        _ => None,
    }
}

pub(crate) fn macroarea_from_regione(regione: &str) -> Option<&'static str> {
    let norm_key = crate::gazetteer::norm(regione);
    match norm_key.as_str() {
        "liguria" | "lombardia" | "piemonte" | "valle d aosta" | "valle daosta" | "vda" => {
            Some("Nord-Ovest")
        }
        "emilia romagna" | "friuli venezia giulia" | "trentino alto adige" | "veneto" | "fvg" => {
            Some("Nord-Est")
        }
        "lazio" | "marche" | "toscana" | "umbria" => Some("Centro"),
        "abruzzo" | "abruzzi" | "basilicata" | "calabria" | "campania" | "molise" | "puglia" => {
            Some("Sud")
        }
        "sardegna" | "sicilia" => Some("Isole"),
        _ => None,
    }
}

pub(crate) fn normalize_extracted_macroarea(value: &str) -> Option<String> {
    if let Some(from_reg) = macroarea_from_regione(value) {
        return Some(from_reg.to_owned());
    }
    let norm_key = crate::gazetteer::norm(value);
    match norm_key.as_str() {
        "nord ovest" | "nordovest" | "nord west" | "north west" => Some("Nord-Ovest".to_owned()),
        "nord est" | "nordest" | "nord east" | "north east" => Some("Nord-Est".to_owned()),
        "centro" | "center" | "central" | "italia centrale" => Some("Centro".to_owned()),
        "sud" | "south" | "meridione" | "italia meridionale" => Some("Sud".to_owned()),
        "isole" | "islands" | "insulare" | "italia insulare" => Some("Isole".to_owned()),
        _ => None,
    }
}
