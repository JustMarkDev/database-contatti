use crate::ai_runs::DeterministicResult;
use crate::open_db;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const BUILTIN_CATALOG_JSON: &str = include_str!("../resources/deterministic-rules-v1.json");

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RuleCatalog {
    pub id: String,
    pub semantic_version: String,
    pub rules: Vec<ExactRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExactRule {
    pub id: String,
    pub version: i64,
    pub kind: String,
    pub priority: i64,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub category: String,
    pub subcategory: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogMatch {
    pub result: DeterministicResult,
    pub rule_id: String,
    pub rule_version: i64,
    pub overridden_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DeterministicEvidence<'a> {
    pub email: &'a str,
    pub denomination: Option<&'a str>,
    pub source_url: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct DeterministicRuleSummary {
    pub id: String,
    pub kind: String,
    pub evidence_type: String,
    pub specificity_level: String,
    pub priority: i64,
    pub output: String,
    pub overridden_rules: Vec<String>,
    pub match_count: usize,
    pub overridden_count: usize,
    pub conflict_count: usize,
}

#[derive(Debug, Serialize)]
pub struct DeterministicCatalogSummary {
    pub id: String,
    pub semantic_version: String,
    pub status: String,
    pub match_count: usize,
    pub conflict_count: usize,
    pub unresolved_semantic_keys: Vec<String>,
    pub examples: Vec<String>,
    pub rules: Vec<DeterministicRuleSummary>,
    pub active_run: bool,
    pub recheck_count: usize,
}

fn evidence_type(kind: &str) -> &'static str {
    match kind {
        "school_mechanographic" => "identificativo ufficiale",
        "municipality_unit" => "unità nominata",
        "domain_exact" => "dominio esatto",
        "domain_suffix" => "dominio istituzionale",
        "domain_prefix" => "prefisso di dominio",
        "token_evidence" => "token esatto",
        "municipality_domain" => "dominio comunale",
        _ => "evidenza esatta",
    }
}

fn specificity_level(kind: &str) -> &'static str {
    match kind {
        "school_mechanographic" => "official_identifier",
        "municipality_unit" => "named_unit",
        "domain_exact" => "exact_domain",
        "domain_suffix" => "institutional_domain",
        "domain_prefix" => "domain_prefix",
        "token_evidence" => "exact_token",
        "municipality_domain" => "generic_parent_domain",
        _ => "unknown",
    }
}

fn normalized(value: &str) -> String {
    value
        .trim()
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
        .join("")
}

fn exact_segments(
    local: &str,
    denomination: Option<&str>,
    source_url: Option<&str>,
) -> BTreeSet<String> {
    let mut segments = BTreeSet::new();
    for raw in [Some(local), denomination, source_url]
        .into_iter()
        .flatten()
    {
        let parts = raw
            .split(|c: char| !c.is_alphanumeric())
            .map(normalized)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        for part in &parts {
            segments.insert(part.clone());
        }
        // "Pro Loco" → proloco without substring scanning "xprolocoy".
        for window in parts.windows(2) {
            segments.insert(format!("{}{}", window[0], window[1]));
        }
        let whole = normalized(raw);
        if !whole.is_empty() {
            segments.insert(whole);
        }
    }
    segments
}

fn domain_labels(domain: &str) -> Vec<String> {
    domain
        .split('.')
        .map(normalized)
        .filter(|label| !label.is_empty())
        .collect()
}

/// Safe free-signal match: exact segments, exact DNS labels, long aliases at the
/// start of a local-part, denomination or URL segment (`parrocchiasanmarco`). Never a substring
/// in the middle of a word: `laboratorio` must not match `oratorio`. Short aliases
/// (`cai`) match the local part only as its first segment, so a surname such as
/// `mario.cai@` is not evidence.
fn token_evidence_hit(
    aliases: &[String],
    local: &str,
    domain: &str,
    denomination: Option<&str>,
    source_url: Option<&str>,
) -> bool {
    let named = exact_segments("", denomination, source_url);
    let local_segments = exact_segments(local, None, None);
    let first_local = local
        .split(|c: char| !c.is_alphanumeric())
        .map(normalized)
        .find(|part| !part.is_empty());
    let labels = domain_labels(domain);
    aliases.iter().any(|alias| {
        let needle = normalized(alias);
        if needle.is_empty() {
            return false;
        }
        let in_local = if needle.len() <= 3 {
            first_local.as_deref() == Some(needle.as_str())
        } else {
            local_segments.contains(&needle)
        };
        if in_local || named.contains(&needle) || labels.iter().any(|label| label == &needle) {
            return true;
        }
        // `prolocomilo@`, `parrocchiasanmarco@`: long aliases at the start of a word.
        needle.len() >= 7
            && named
                .iter()
                .chain(local_segments.iter())
                .any(|segment| segment.starts_with(&needle))
    })
}

/// DNS-label prefix for compact brands (`prolocomelle` ← `proloco`). Exact label
/// also matches. Never scans local-part or denomination substrings.
fn domain_prefix_hit(aliases: &[String], domain: &str) -> bool {
    let labels = domain_labels(domain);
    aliases.iter().any(|alias| {
        let needle = normalized(alias);
        needle.len() >= 6
            && labels.iter().any(|label| {
                label == &needle || (label.starts_with(&needle) && label.len() > needle.len())
            })
    })
}

/// Returns the municipality name and, when the domain carries one, its
/// province code: `comune.<nome>.<pv>.it` or `comune.<nome>.it`.
fn parse_municipality_domain(domain: &str) -> Option<(String, Option<String>)> {
    let parts = domain.split('.').collect::<Vec<_>>();
    if parts.len() < 3 || parts.last() != Some(&"it") {
        return None;
    }
    let marker = parts.iter().position(|part| *part == "comune")?;
    let labels = &parts[marker + 1..parts.len() - 1];
    match labels {
        [] => None,
        [name @ .., province]
            if !name.is_empty()
                && province.len() == 2
                && province.chars().all(|c| c.is_ascii_alphabetic()) =>
        {
            Some((name.join(" "), Some(province.to_uppercase())))
        }
        // `comune.montemarciano.ancona.it`: the province spelled out.
        [name @ .., province]
            if !name.is_empty()
                && crate::gazetteer::GAZETTEER_INDEX
                    .provinces
                    .contains_key(&crate::gazetteer::norm(province)) =>
        {
            Some((name.join(" "), Some((*province).to_owned())))
        }
        _ => Some((labels.join(" "), None)),
    }
}

/// Gazetteer keys for a DNS label: exact first, else the space-free spelling
/// (`massalombarda` → Massa Lombarda, `sanbellino` → San Bellino).
fn municipality_keys(raw: &str) -> Vec<String> {
    static COMPACT: std::sync::LazyLock<HashMap<String, BTreeSet<&'static str>>> =
        std::sync::LazyLock::new(|| {
            let mut compact = HashMap::<String, BTreeSet<&'static str>>::new();
            for name in crate::gazetteer::GAZETTEER_INDEX
                .municipality_names
                .values()
            {
                compact.entry(normalized(name)).or_default().insert(*name);
            }
            compact
        });
    let index = &*crate::gazetteer::GAZETTEER_INDEX;
    let key = crate::gazetteer::norm(raw);
    if index.municipality_locations.contains_key(&key) || index.municipalities.contains_key(&key) {
        return vec![key];
    }
    COMPACT
        .get(&normalized(raw))
        .map(|names| {
            names
                .iter()
                .map(|name| crate::gazetteer::norm(name))
                .collect()
        })
        .unwrap_or_default()
}

/// Without a province code, a bare sigla (`comune.fi.it`) is the capoluogo;
/// any other name must match exactly one municipality.
fn resolve_domain_geo(
    name: &str,
    province_code: Option<&str>,
) -> Option<(String, String, Option<String>)> {
    if let Some(code) = province_code {
        return resolve_geo(name, code);
    }
    if name.len() == 2
        && let Some(province) = crate::gazetteer::GAZETTEER_INDEX
            .provinces
            .get(&crate::gazetteer::norm(name))
    {
        return resolve_geo(province, name);
    }
    // The province index is keyed by name as well as sigla.
    let locations = municipality_keys(name)
        .iter()
        .filter_map(|key| {
            crate::gazetteer::GAZETTEER_INDEX
                .municipality_locations
                .get(key)
        })
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>();
    match locations.into_iter().collect::<Vec<_>>().as_slice() {
        [(municipality, province)] => resolve_geo(municipality, province),
        _ => None,
    }
}

fn resolve_geo(
    raw_municipality: &str,
    province_code: &str,
) -> Option<(String, String, Option<String>)> {
    let province = crate::gazetteer::GAZETTEER_INDEX
        .provinces
        .get(&crate::gazetteer::norm(province_code))?
        .to_string();
    let municipality = municipality_keys(raw_municipality).iter().find_map(|key| {
        match crate::gazetteer::GAZETTEER_INDEX
            .municipality_locations
            .get(key)
        {
            Some(locations) => locations
                .iter()
                .find(|(_, location_province)| {
                    crate::gazetteer::norm(location_province) == crate::gazetteer::norm(&province)
                })
                .map(|(name, _)| (*name).to_owned()),
            None => crate::gazetteer::GAZETTEER_INDEX
                .municipalities
                .get(key)
                .map(|name| (*name).to_owned()),
        }
    })?;
    let region = crate::parsing::PROVINCE_REGIONS
        .iter()
        .find(|(name, _)| crate::gazetteer::norm(name) == crate::gazetteer::norm(&province))
        .map(|(_, region)| region.to_string());
    Some((municipality, province, region))
}

fn school_result(local: &str, domain: &str) -> Option<DeterministicResult> {
    if !(domain == "istruzione.it" || domain == "pec.istruzione.it") {
        return None;
    }
    let upper = local.to_ascii_uppercase();
    let chars = upper.chars().collect::<Vec<_>>();
    if chars.len() != 10
        || !chars.iter().all(|c| c.is_ascii_alphanumeric())
        || !chars[..4].iter().all(|c| c.is_ascii_alphabetic())
        || chars[4..].iter().filter(|c| c.is_ascii_digit()).count() < 3
    {
        return None;
    }
    let school_type = &upper[2..4];
    let subcategory = match school_type {
        "EE" => "Primaria",
        "MM" => "Secondaria di primo grado",
        "IC" => "Istituto comprensivo",
        "AA" => "Infanzia",
        "CT" => "Centro territoriale / CPIA",
        "IS" | "IT" | "IP" | "PS" | "PC" | "PM" | "TF" | "TL" | "TN" | "TP" | "TA" | "TB"
        | "TE" | "TH" | "RA" | "RC" | "RF" | "RH" | "RI" | "SD" | "SL" | "TD" => {
            "Secondaria di secondo grado"
        }
        _ => return None,
    };
    let province = crate::gazetteer::GAZETTEER_INDEX
        .provinces
        .get(&crate::gazetteer::norm(&upper[..2]))?
        .to_string();
    let region = crate::parsing::PROVINCE_REGIONS
        .iter()
        .find(|(name, _)| crate::gazetteer::norm(name) == crate::gazetteer::norm(&province))
        .map(|(_, region)| region.to_string());
    Some(DeterministicResult {
        category: "Istruzione",
        subcategory: Some(subcategory),
        comune: None,
        provincia: Some(province),
        regione: region,
        reason: format!("Codice meccanografico esatto '{upper}'"),
    })
}

fn canonical_label(value: &str) -> Option<&'static str> {
    Some(match value {
        "Istruzione" | "istruzione" => "Istruzione",
        "Pubblica amministrazione" | "pubblica_amministrazione" | "pubblicaamministrazione" => {
            "Pubblica amministrazione"
        }
        "Cultura" | "cultura" => "Cultura",
        "Sanità" | "Sanita" | "sanita" | "sanità" => "Sanità",
        "Turismo e promozione territoriale"
        | "turismo_e_promozione_territoriale"
        | "turismoepromozioneterritoriale" => "Turismo e promozione territoriale",
        "Associazioni" | "associazioni" => "Associazioni",
        "Biblioteca" | "biblioteca" => "Biblioteca",
        "Museo" | "museo" => "Museo",
        "Teatro" | "teatro" => "Teatro",
        "Cinema" | "cinema" => "Cinema",
        "Forze dell'ordine" | "forze_dell_ordine" | "forzedellordine" | "law_enforcement" => {
            "Forze dell'ordine"
        }
        "Ufficio turistico" | "ufficio_turistico" | "ufficioturistico" => "Ufficio turistico",
        "Comune" | "comune" => "Comune",
        "Regione / Ente regionale" | "regione_ente_regionale" | "regioneenteregionale" => {
            "Regione / Ente regionale"
        }
        "Ministero / Ente statale" | "ministero_ente_statale" | "ministeroentestatale" => {
            "Ministero / Ente statale"
        }
        "Tribunale / Ufficio giudiziario"
        | "tribunale_ufficio_giudiziario"
        | "tribunaleufficiogiudiziario" => "Tribunale / Ufficio giudiziario",
        "Università / alta formazione"
        | "universita_alta_formazione"
        | "universitaaltaformazione" => "Università / alta formazione",
        "Altro ente sanitario" | "altro_ente_sanitario" | "altroentesanitario" => {
            "Altro ente sanitario"
        }
        "ASL / ASST / Distretto sanitario" | "aslasstdistrettosanitario" => {
            "ASL / ASST / Distretto sanitario"
        }
        "Ospedale / Azienda ospedaliera" | "ospedaleaziendaospedaliera" => {
            "Ospedale / Azienda ospedaliera"
        }
        "Clinica / IRCCS / Struttura sanitaria" | "clinicairccsstrutturasanitaria" => {
            "Clinica / IRCCS / Struttura sanitaria"
        }
        "Altro ente pubblico" | "altro_ente_pubblico" | "altroentepubblico" => {
            "Altro ente pubblico"
        }
        "Alpini" | "alpini" => "Alpini",
        "CAI" | "cai" => "CAI",
        "NOI" | "noi" => "NOI",
        "Associazione genitori" | "associazione_genitori" | "associazionegenitori" => {
            "Associazione genitori"
        }
        "Lions" | "lions" => "Lions",
        "Rotary" | "rotary" => "Rotary",
        "Volontariato / solidarietà" | "volontariato_solidarieta" | "volontariatosolidarieta" => {
            "Volontariato / solidarietà"
        }
        "Enti religiosi" | "enti_religiosi" | "entireligiosi" => "Enti religiosi",
        "Parrocchia" | "parrocchia" => "Parrocchia",
        "Oratorio / centro giovanile" | "oratorio_centro_giovanile" | "oratoriocentrogovanile" => {
            "Oratorio / centro giovanile"
        }
        "Pro Loco" | "pro_loco" | "proloco" => "Pro Loco",
        "Altro ente formativo" | "altro_ente_formativo" | "altroenteformativo" => {
            "Altro ente formativo"
        }
        _ => return None,
    })
}

fn taxonomy_key(value: &str) -> String {
    match value {
        "Forze dell'ordine" | "forze_dell_ordine" | "forzedellordine" | "law_enforcement" => {
            "law_enforcement".to_owned()
        }
        _ => crate::normalize_key(value),
    }
}

fn rule_matches_taxonomy(rule: &ExactRule, taxonomy: &[(String, Vec<String>)]) -> bool {
    let category_key = taxonomy_key(&rule.category);
    let subcategory_key = rule.subcategory.as_deref().map(taxonomy_key);
    taxonomy.iter().any(|(category, subcategories)| {
        category == &category_key
            && subcategory_key
                .as_ref()
                .is_none_or(|subcategory| subcategories.contains(subcategory))
    })
}

fn rule_result(
    rule: &ExactRule,
    comune: Option<String>,
    provincia: Option<String>,
    regione: Option<String>,
    reason: String,
) -> Option<DeterministicResult> {
    let subcategory = match rule.subcategory.as_deref() {
        Some(value) => Some(canonical_label(value)?),
        None => None,
    };
    Some(DeterministicResult {
        category: canonical_label(&rule.category)?,
        subcategory,
        comune,
        provincia,
        regione,
        reason,
    })
}

pub(crate) struct CompiledCatalog<'a> {
    rules: &'a [ExactRule],
    domain_exact: HashMap<&'a str, Vec<usize>>,
    domain_suffix: HashMap<&'a str, Vec<usize>>,
    domain_prefix: Vec<usize>,
    token_evidence: Vec<usize>,
    municipality_unit: Vec<usize>,
    municipality_domain: Vec<usize>,
    school: Option<usize>,
}

impl RuleCatalog {
    pub(crate) fn compile(&self) -> CompiledCatalog<'_> {
        let mut domain_exact = HashMap::<&str, Vec<usize>>::new();
        let mut domain_suffix = HashMap::<&str, Vec<usize>>::new();
        let mut domain_prefix = Vec::new();
        let mut token_evidence = Vec::new();
        let mut municipality_unit = Vec::new();
        let mut municipality_domain = Vec::new();
        let mut school = None;
        for (index, rule) in self.rules.iter().enumerate() {
            match rule.kind.as_str() {
                "school_mechanographic" => school = Some(index),
                "domain_exact" => {
                    for domain in &rule.domains {
                        domain_exact.entry(domain.as_str()).or_default().push(index);
                    }
                }
                "domain_suffix" => {
                    for domain in &rule.domains {
                        domain_suffix
                            .entry(domain.as_str())
                            .or_default()
                            .push(index);
                    }
                }
                "domain_prefix" => domain_prefix.push(index),
                "token_evidence" => token_evidence.push(index),
                "municipality_unit" => municipality_unit.push(index),
                "municipality_domain" => municipality_domain.push(index),
                _ => {}
            }
        }
        CompiledCatalog {
            rules: &self.rules,
            domain_exact,
            domain_suffix,
            domain_prefix,
            token_evidence,
            municipality_unit,
            municipality_domain,
            school,
        }
    }
}

#[allow(dead_code)]
pub(crate) fn evaluate(
    catalog: &RuleCatalog,
    evidence: DeterministicEvidence<'_>,
) -> Result<Option<CatalogMatch>, Vec<String>> {
    evaluate_compiled(&catalog.compile(), evidence)
}

pub(crate) fn evaluate_compiled(
    catalog: &CompiledCatalog<'_>,
    evidence: DeterministicEvidence<'_>,
) -> Result<Option<CatalogMatch>, Vec<String>> {
    let email = evidence.email.trim().to_lowercase();
    let Some((local, domain)) = email.split_once('@') else {
        return Ok(None);
    };
    let mut matches = Vec::new();
    if let Some(index) = catalog.school
        && let Some(result) = school_result(local, domain)
    {
        matches.push((&catalog.rules[index], result));
    }
    if let Some(indices) = catalog.domain_exact.get(domain) {
        for &index in indices {
            let rule = &catalog.rules[index];
            if let Some(result) = rule_result(
                rule,
                None,
                None,
                None,
                format!("Dominio esatto '{domain}' · {}", rule.id),
            ) {
                matches.push((rule, result));
            }
        }
    }
    // An institution's own domain and every subdomain (`pec.`, `dipartimento.`).
    let suffix_rules = std::iter::successors(Some(domain), |rest| {
        rest.split_once('.').map(|(_, tail)| tail)
    })
    .filter_map(|suffix| catalog.domain_suffix.get(suffix))
    .flatten()
    .copied()
    .collect::<BTreeSet<_>>();
    for index in suffix_rules {
        let rule = &catalog.rules[index];
        if let Some(result) = rule_result(
            rule,
            None,
            None,
            None,
            format!("Dominio istituzionale '{domain}' · {}", rule.id),
        ) {
            matches.push((rule, result));
        }
    }
    for &index in &catalog.domain_prefix {
        let rule = &catalog.rules[index];
        if domain_prefix_hit(&rule.aliases, domain)
            && let Some(result) = rule_result(
                rule,
                None,
                None,
                None,
                format!("Prefisso di dominio · {}", rule.id),
            )
        {
            matches.push((rule, result));
        }
    }
    for &index in &catalog.token_evidence {
        let rule = &catalog.rules[index];
        if token_evidence_hit(
            &rule.aliases,
            local,
            domain,
            evidence.denomination,
            evidence.source_url,
        ) && let Some(result) = rule_result(
            rule,
            None,
            None,
            None,
            format!("Token esatto · {}", rule.id),
        ) {
            matches.push((rule, result));
        }
    }
    // `comune.<nome>…it` is a municipal body even when the town name does not
    // resolve (homonyms, unknown spelling): category without geography then.
    if let Some((name, code)) = parse_municipality_domain(domain) {
        let (comune, provincia, regione) = match resolve_domain_geo(&name, code.as_deref()) {
            Some((comune, provincia, regione)) => (Some(comune), Some(provincia), regione),
            None => (None, None, None),
        };
        let geo_note = if comune.is_some() {
            "risolto esattamente"
        } else {
            "senza geografia univoca"
        };
        if !catalog.municipality_unit.is_empty() {
            let segments = exact_segments(local, evidence.denomination, evidence.source_url);
            for &index in &catalog.municipality_unit {
                let rule = &catalog.rules[index];
                if rule
                    .aliases
                    .iter()
                    .any(|alias| segments.contains(&normalized(alias)))
                    && let Some(result) = rule_result(
                        rule,
                        comune.clone(),
                        provincia.clone(),
                        regione.clone(),
                        format!("Unità nominata esatta su dominio comunale · {}", rule.id),
                    )
                {
                    matches.push((rule, result));
                }
            }
        }
        for &index in &catalog.municipality_domain {
            let rule = &catalog.rules[index];
            if let Some(result) = rule_result(
                rule,
                comune.clone(),
                provincia.clone(),
                regione.clone(),
                format!("Dominio comunale {geo_note} · {}", rule.id),
            ) {
                matches.push((rule, result));
            }
        }
    }
    let Some(highest) = matches.iter().map(|(rule, _)| rule.priority).max() else {
        return Ok(None);
    };
    let all_match_ids = matches
        .iter()
        .map(|(rule, _)| rule.id.clone())
        .collect::<Vec<_>>();
    let winners = matches
        .into_iter()
        .filter(|(rule, _)| rule.priority == highest)
        .collect::<Vec<_>>();
    let outcomes = winners
        .iter()
        .map(|(_, result)| (result.category, result.subcategory))
        .collect::<BTreeSet<_>>();
    if outcomes.len() > 1 {
        return Err(winners
            .into_iter()
            .map(|(rule, _)| rule.id.clone())
            .collect());
    }
    let winner_ids = winners
        .iter()
        .map(|(rule, _)| rule.id.clone())
        .collect::<BTreeSet<_>>();
    let (rule, result) = winners.into_iter().next().expect("non-empty winners");
    Ok(Some(CatalogMatch {
        result,
        rule_id: rule.id.clone(),
        rule_version: rule.version,
        overridden_rule_ids: all_match_ids
            .into_iter()
            .filter(|id| !winner_ids.contains(id))
            .collect(),
    }))
}

pub(crate) fn catalog_by_id(db: &Connection, id: &str) -> Result<RuleCatalog, String> {
    let raw: String = db
        .query_row(
            "SELECT rules_json FROM deterministic_rule_catalogs WHERE id=?1",
            params![id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

fn preview(
    db: &Connection,
    catalog: &RuleCatalog,
    status: String,
) -> Result<DeterministicCatalogSummary, String> {
    let compiled = catalog.compile();
    let mut statement = db
        .prepare("SELECT email,denomination,source_url FROM contacts ORDER BY id")
        .map_err(|error| error.to_string())?;
    let contacts = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut match_count = 0;
    let mut conflict_count = 0;
    let mut rule_matches = BTreeMap::<String, usize>::new();
    let mut rule_overrides = BTreeMap::<String, usize>::new();
    let mut overridden_rules = BTreeMap::<String, BTreeSet<String>>::new();
    let mut rule_conflicts = BTreeMap::<String, usize>::new();
    let mut examples = Vec::new();
    for row in contacts {
        let (email, denomination, source_url) = row.map_err(|error| error.to_string())?;
        match evaluate_compiled(
            &compiled,
            DeterministicEvidence {
                email: &email,
                denomination: denomination.as_deref(),
                source_url: source_url.as_deref(),
            },
        ) {
            Ok(Some(result)) => {
                match_count += 1;
                *rule_matches.entry(result.rule_id.clone()).or_default() += 1;
                for rule_id in &result.overridden_rule_ids {
                    *rule_overrides.entry(rule_id.clone()).or_default() += 1;
                    overridden_rules
                        .entry(result.rule_id.clone())
                        .or_default()
                        .insert(rule_id.clone());
                }
                if examples.len() < 12 {
                    examples.push(format!(
                        "{} → {}{} · {}",
                        email,
                        result.result.category,
                        result
                            .result
                            .subcategory
                            .as_ref()
                            .map(|value| format!(" / {value}"))
                            .unwrap_or_default(),
                        result.rule_id
                    ));
                }
            }
            Err(rule_ids) => {
                conflict_count += 1;
                for rule_id in rule_ids {
                    *rule_conflicts.entry(rule_id).or_default() += 1;
                }
            }
            Ok(None) => {}
        }
    }
    let taxonomy = crate::taxonomy::load_active_taxonomy(db)?;
    let mut unresolved = BTreeSet::new();
    for rule in &catalog.rules {
        if !rule_matches_taxonomy(rule, &taxonomy.categories) {
            unresolved.insert(format!(
                "{}{}",
                rule.category,
                rule.subcategory
                    .as_ref()
                    .map(|value| format!(" / {value}"))
                    .unwrap_or_default()
            ));
        }
    }
    let active_run = db
        .query_row("SELECT EXISTS(SELECT 1 FROM ai_classification_runs WHERE status IN ('running','pausing'))", [], |row| row.get::<_, i64>(0))
        .map_err(|error| error.to_string())? != 0;
    let recheck_count = db
        .query_row(
            "SELECT COUNT(*) FROM deterministic_recheck_queue WHERE catalog_id=?1",
            params![catalog.id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())? as usize;
    let rules = catalog
        .rules
        .iter()
        .map(|rule| DeterministicRuleSummary {
            id: rule.id.clone(),
            kind: rule.kind.clone(),
            evidence_type: evidence_type(&rule.kind).to_owned(),
            specificity_level: specificity_level(&rule.kind).to_owned(),
            priority: rule.priority,
            output: format!(
                "{}{}",
                canonical_label(&rule.category).unwrap_or(rule.category.as_str()),
                rule.subcategory
                    .as_deref()
                    .map(|value| format!(" / {}", canonical_label(value).unwrap_or(value)))
                    .unwrap_or_default()
            ),
            overridden_rules: overridden_rules
                .remove(&rule.id)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            match_count: rule_matches.get(&rule.id).copied().unwrap_or_default(),
            overridden_count: rule_overrides.get(&rule.id).copied().unwrap_or_default(),
            conflict_count: rule_conflicts.get(&rule.id).copied().unwrap_or_default(),
        })
        .collect();
    Ok(DeterministicCatalogSummary {
        id: catalog.id.clone(),
        semantic_version: catalog.semantic_version.clone(),
        status,
        match_count,
        conflict_count,
        unresolved_semantic_keys: unresolved.into_iter().collect(),
        examples,
        rules,
        active_run,
        recheck_count,
    })
}

#[tauri::command]
pub async fn list_deterministic_catalogs(
    app: tauri::AppHandle,
) -> Result<Vec<DeterministicCatalogSummary>, String> {
    crate::database::blocking(move || {
        let db = open_db(&app)?;
        let mut statement = db
            .prepare(
                "SELECT rules_json,status FROM deterministic_rule_catalogs ORDER BY created_at DESC",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?;
        rows.map(|row| {
            let (raw, status) = row.map_err(|error| error.to_string())?;
            let catalog = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
            preview(&db, &catalog, status)
        })
        .collect()
    })
    .await
}

#[tauri::command]
pub async fn activate_deterministic_catalog(
    app: tauri::AppHandle,
    catalog_id: String,
) -> Result<DeterministicCatalogSummary, String> {
    crate::database::blocking(move || {
        let mut db = open_db(&app)?;
        let active_run: i64 = db.query_row("SELECT EXISTS(SELECT 1 FROM ai_classification_runs WHERE status IN ('running','pausing'))", [], |row| row.get(0)).map_err(|error| error.to_string())?;
        if active_run != 0 {
            return Err(
                "Impossibile attivare regole deterministiche durante un'esecuzione attiva".to_owned(),
            );
        }
        let raw: String = db
            .query_row(
                "SELECT rules_json FROM deterministic_rule_catalogs WHERE id=?1",
                params![catalog_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let catalog: RuleCatalog = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let previous: Option<RuleCatalog> = db
            .query_row(
                "SELECT rules_json FROM deterministic_rule_catalogs WHERE status='active'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .map(|value| serde_json::from_str(&value).map_err(|error| error.to_string()))
            .transpose()?;
        let report = preview(&db, &catalog, "active".to_owned())?;
        if !report.unresolved_semantic_keys.is_empty() {
            return Err(format!(
                "Chiavi tassonomiche non risolte: {}",
                report.unresolved_semantic_keys.join(", ")
            ));
        }
        if report.conflict_count > 0 {
            return Err(format!(
                "Conflitti deterministici non risolti: {}",
                report.conflict_count
            ));
        }
        let tx = db.transaction().map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE deterministic_rule_catalogs SET status='retired' WHERE status='active'",
            [],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE deterministic_rule_catalogs SET status='active',activated_at=?1 WHERE id=?2",
            params![chrono::Utc::now().to_rfc3339(), catalog_id],
        )
        .map_err(|error| error.to_string())?;

        if let Some(previous) = previous {
            let changed = previous
                .rules
                .iter()
                .filter(|old| {
                    catalog
                        .rules
                        .iter()
                        .find(|new| new.id == old.id)
                        .is_none_or(|new| new.version != old.version)
                })
                .map(|rule| rule.id.clone())
                .collect::<Vec<_>>();
            for rule_id in changed {
                tx.execute(
                    "INSERT OR REPLACE INTO deterministic_recheck_queue(contact_id,catalog_id,reason,created_at)
                 SELECT contact_id,?1,?2,?3 FROM deterministic_rule_attributions WHERE rule_id=?2",
                    params![catalog_id, rule_id, chrono::Utc::now().to_rfc3339()],
                ).map_err(|error| error.to_string())?;
                tx.execute(
                    "UPDATE contacts SET classification_hash=NULL,taxonomy_version_id=NULL
                 WHERE classification_hash LIKE 'deterministic:%' AND id IN (SELECT contact_id FROM deterministic_rule_attributions WHERE rule_id=?1)",
                    params![rule_id],
                ).map_err(|error| error.to_string())?;
            }
        }
        tx.commit().map_err(|error| error.to_string())?;
        Ok(report)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> RuleCatalog {
        serde_json::from_str(BUILTIN_CATALOG_JSON).unwrap()
    }

    #[test]
    fn exact_domain_catalog_covers_pec_variants_regions_ministries_and_universities() {
        let cases = [
            (
                "protocollo@pec.carabinieri.it",
                "Pubblica amministrazione",
                "Forze dell'ordine",
            ),
            (
                "urp@regione.lazio.it",
                "Pubblica amministrazione",
                "Regione / Ente regionale",
            ),
            (
                "info@mef.gov.it",
                "Pubblica amministrazione",
                "Ministero / Ente statale",
            ),
            (
                "segreteria@unina.it",
                "Istruzione",
                "Università / alta formazione",
            ),
            (
                "protocollo@sanita.it",
                "Pubblica amministrazione",
                "Ministero / Ente statale",
            ),
        ];
        for (email, category, subcategory) in cases {
            let matched = evaluate(
                &catalog(),
                DeterministicEvidence {
                    email,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(matched.result.category, category, "{email}");
            assert_eq!(matched.result.subcategory, Some(subcategory), "{email}");
        }
        assert!(
            evaluate(
                &catalog(),
                DeterministicEvidence {
                    email: "info@asl-inventata.example.test",
                    ..Default::default()
                }
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn named_municipality_unit_overrides_parent_and_substrings_do_not_match() {
        let library = evaluate(
            &catalog(),
            DeterministicEvidence {
                email: "biblioteca@comune.moncalieri.to.it",
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(library.result.category, "Cultura");
        assert_eq!(library.result.subcategory, Some("Biblioteca"));
        let parent = evaluate(
            &catalog(),
            DeterministicEvidence {
                email: "protocollo@comune.moncalieri.to.it",
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(parent.result.subcategory, Some("Comune"));
        assert!(
            evaluate(
                &catalog(),
                DeterministicEvidence {
                    email: "xprolocoy@example.test",
                    ..Default::default()
                }
            )
            .unwrap()
            .is_none()
        );
        let proloco = evaluate(
            &catalog(),
            DeterministicEvidence {
                email: "info@prolocomelle.it",
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(proloco.result.category, "Turismo e promozione territoriale");
        assert_eq!(proloco.result.subcategory, Some("Pro Loco"));
        let denom_proloco = evaluate(
            &catalog(),
            DeterministicEvidence {
                email: "info@example.test",
                denomination: Some("Pro Loco di Melle"),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(denom_proloco.result.subcategory, Some("Pro Loco"));
    }

    #[test]
    fn token_evidence_and_association_domains_match_without_false_prefixes() {
        let cases = [
            ("segreteria@cai.it", None, "Associazioni", "CAI"),
            ("club@rotary.it", None, "Associazioni", "Rotary"),
            ("info@lions.it", None, "Associazioni", "Lions"),
            (
                "segreteria@example.test",
                Some("Parrocchia Santa Maria"),
                "Enti religiosi",
                "Parrocchia",
            ),
            (
                "info@example.test",
                Some("Conservatorio Statale di Musica"),
                "Istruzione",
                "Altro ente formativo",
            ),
            (
                "info@example.test",
                Some("Associazione Genitori Scuola Esempio"),
                "Associazioni",
                "Associazione genitori",
            ),
        ];
        for (email, denomination, category, subcategory) in cases {
            let matched = evaluate(
                &catalog(),
                DeterministicEvidence {
                    email,
                    denomination,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(matched.result.category, category, "{email}");
            assert_eq!(matched.result.subcategory, Some(subcategory), "{email}");
        }
    }

    #[test]
    fn long_tokens_match_word_starts_and_short_tokens_skip_surnames() {
        let hit = |email: &str, denomination: Option<&str>, source_url: Option<&str>| {
            evaluate(
                &catalog(),
                DeterministicEvidence {
                    email,
                    denomination,
                    source_url,
                },
            )
            .unwrap()
            .map(|matched| matched.result.subcategory.map(str::to_owned))
        };
        assert_eq!(
            hit("info@example.test", Some("Laboratorio teatrale"), None),
            None
        );
        assert_eq!(
            hit(
                "info@example.test",
                None,
                Some("https://www.laboratorioarte.example.test")
            ),
            None
        );
        assert_eq!(
            hit(
                "info@example.test",
                None,
                Some("https://www.parrocchiasanmarco.example.test")
            ),
            Some(Some("Parrocchia".to_owned()))
        );
        assert_eq!(
            hit("info@example.test", Some("Oratorio San Luigi"), None),
            Some(Some("Oratorio / centro giovanile".to_owned()))
        );
        assert_eq!(hit("mario.cai@example.test", None, None), None);
        assert_eq!(
            hit("cai.bergamo@example.test", None, None),
            Some(Some("CAI".to_owned()))
        );
    }

    #[test]
    fn state_bodies_are_not_law_enforcement() {
        for email in [
            "comando@vigilfuoco.it",
            "info@esercito.difesa.it",
            "urp@sanita.it",
        ] {
            let matched = evaluate(
                &catalog(),
                DeterministicEvidence {
                    email,
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                matched.result.category, "Pubblica amministrazione",
                "{email}"
            );
            assert_eq!(
                matched.result.subcategory,
                Some("Ministero / Ente statale"),
                "{email}"
            );
        }
    }

    #[test]
    fn broad_health_and_religious_prefixes_are_not_rules() {
        assert!(
            evaluate(
                &catalog(),
                DeterministicEvidence {
                    email: "info@asl-example.test",
                    ..Default::default()
                }
            )
            .unwrap()
            .is_none()
        );
        assert!(
            evaluate(
                &catalog(),
                DeterministicEvidence {
                    email: "info@diocesi-example.test",
                    ..Default::default()
                }
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn conflicting_equal_priority_rules_abstain() {
        let mut catalog = catalog();
        let mut conflict = catalog
            .rules
            .iter()
            .find(|rule| rule.id == "it.municipality.named-unit.library")
            .unwrap()
            .clone();
        conflict.id = "test.conflict".to_owned();
        conflict.category = "Associazioni".to_owned();
        conflict.subcategory = Some("Alpini".to_owned());
        catalog.rules.push(conflict);
        let error = evaluate(
            &catalog,
            DeterministicEvidence {
                email: "biblioteca@comune.moncalieri.to.it",
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.contains(&"it.municipality.named-unit.library".to_owned()));
        assert!(error.contains(&"test.conflict".to_owned()));
    }

    #[test]
    fn every_mechanographic_school_type_uses_the_canonical_taxonomy_label() {
        let istruzione = crate::schema::TAXONOMY
            .iter()
            .find(|(category, _)| *category == "Istruzione")
            .unwrap()
            .1;
        for school_type in [
            "EE", "MM", "IC", "AA", "CT", "IS", "IT", "IP", "PS", "PC", "PM", "TF", "TL", "TN",
            "TP", "TA", "TB", "TE", "TH", "RA", "RC", "RF", "RH", "RI", "SD", "SL", "TD",
        ] {
            let local = format!("TO{school_type}123456");
            let result = school_result(&local, "istruzione.it").unwrap();
            let subcategory = result.subcategory.unwrap();
            assert!(
                istruzione.contains(&subcategory),
                "school type {school_type} produced non-canonical subcategory {subcategory}"
            );
        }
    }

    #[test]
    fn display_labels_resolve_against_semantic_taxonomy_keys() {
        let category_rule = catalog()
            .rules
            .into_iter()
            .find(|rule| rule.id == "it.school.mechanographic")
            .unwrap();
        assert!(rule_matches_taxonomy(
            &category_rule,
            &[("istruzione".to_owned(), Vec::new())]
        ));

        let subcategory_rule = ExactRule {
            id: "test".to_owned(),
            version: 1,
            kind: "domain_exact".to_owned(),
            priority: 1,
            domains: Vec::new(),
            aliases: Vec::new(),
            category: "Pubblica amministrazione".to_owned(),
            subcategory: Some("Forze dell'ordine".to_owned()),
        };
        assert!(rule_matches_taxonomy(
            &subcategory_rule,
            &[(
                "pubblicaamministrazione".to_owned(),
                vec!["law_enforcement".to_owned()]
            )]
        ));
    }

    #[test]
    fn municipality_domains_without_province_code_resolve_only_when_unambiguous() {
        let geo = |email: &str| {
            evaluate(
                &catalog(),
                DeterministicEvidence {
                    email,
                    ..Default::default()
                },
            )
            .unwrap()
            .map(|matched| {
                (
                    matched.result.subcategory,
                    matched.result.comune,
                    matched.result.provincia,
                )
            })
        };
        assert_eq!(
            geo("protocollo@cert.comune.milano.it"),
            Some((
                Some("Comune"),
                Some("Milano".to_owned()),
                Some("Milano".to_owned())
            ))
        );
        assert_eq!(
            geo("urp@comune.fi.it").and_then(|(_, comune, _)| comune),
            Some("Firenze".to_owned())
        );
        assert_eq!(
            geo("biblioteca@comune.moncalieri.to.it").and_then(|(sub, _, _)| sub),
            Some("Biblioteca")
        );
        // Castro exists in both Bergamo and Lecce: still a Comune, without geography.
        assert_eq!(
            geo("info@comune.castro.it"),
            Some((Some("Comune"), None, None))
        );
        assert_eq!(geo("info@comune.it"), None);
        // Space-free names and spelled-out provinces resolve.
        assert_eq!(
            geo("pubblico@comune.massalombarda.ra.it"),
            Some((
                Some("Comune"),
                Some("Massa Lombarda".to_owned()),
                Some("Ravenna".to_owned())
            ))
        );
        assert_eq!(
            geo("lav.pubblici@comune.montemarciano.ancona.it").and_then(|(_, comune, _)| comune),
            Some("Montemarciano".to_owned())
        );
    }

    #[test]
    fn audit_gaps_resolve_without_jev() {
        let hit = |email: &str| {
            evaluate(
                &catalog(),
                DeterministicEvidence {
                    email,
                    ..Default::default()
                },
            )
            .unwrap()
            .map(|matched| (matched.result.category, matched.result.subcategory))
        };
        let cases = [
            (
                "prolocomilo@gmail.com",
                "Turismo e promozione territoriale",
                Some("Pro Loco"),
            ),
            (
                "prolocodecimo@tiscali.it",
                "Turismo e promozione territoriale",
                Some("Pro Loco"),
            ),
            (
                "parrocchiasacrocuore@example.test",
                "Enti religiosi",
                Some("Parrocchia"),
            ),
            (
                "biblioteca.samassi@example.test",
                "Cultura",
                Some("Biblioteca"),
            ),
            (
                "dip.mdsmco@pec.unicampania.it",
                "Istruzione",
                Some("Università / alta formazione"),
            ),
            (
                "utente@bibfs.unipi.it",
                "Istruzione",
                Some("Università / alta formazione"),
            ),
            (
                "dir.amm.ds31@aslnapoli1centro.it",
                "Sanità",
                Some("ASL / ASST / Distretto sanitario"),
            ),
            (
                "utente@pec.asst-lariana.it",
                "Sanità",
                Some("ASL / ASST / Distretto sanitario"),
            ),
            (
                "utente@ospedalesancarlo.it",
                "Sanità",
                Some("Ospedale / Azienda ospedaliera"),
            ),
            (
                "prodotti@tn.camcom.it",
                "Pubblica amministrazione",
                Some("Altro ente pubblico"),
            ),
            ("utente.cognome@istruzione.it", "Istruzione", None),
        ];
        for (email, category, subcategory) in cases {
            assert_eq!(hit(email), Some((category, subcategory)), "{email}");
        }
        // School codes still win over the generic ministry mailbox.
        assert_eq!(
            hit("toic123456@istruzione.it").unwrap().1,
            Some("Istituto comprensivo")
        );
        // Prefixes stay anchored at word starts; look-alike domains stay out.
        assert_eq!(hit("infoproloco@example.test"), None);
        assert_eq!(hit("utente@unipolsai.it"), None);
        assert_eq!(hit("utente@fakeunipi.it"), None);
    }
}
