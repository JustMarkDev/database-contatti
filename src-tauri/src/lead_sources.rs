//! Open-data source catalog for the sandboxed **Cerca lead** page.
//!
//! A source import downloads a public registry (MIUR, IndicePA), maps each row
//! to the app taxonomy and to canonical `comune / provincia / regione`, and
//! stores the result as a discovery run in `search_runs` / `search_leads`. Like
//! web discovery it never reads or writes `contacts` or `blacklist`.

use crate::discovery::{
    LeadAccumulator, LeadContext, RunRecord, get_ok, html_to_text, insert_run, polite_client,
    read_limited, read_text,
};
use crate::gazetteer::{norm, place_display_name};
use crate::parsing::{
    PROVINCE_REGIONS, REGIONS, clean_email, is_pec_email, is_pec_from_kind, is_valid_email_syntax,
};
use crate::{SearchRunSummary, open_db};
use regex::Regex;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;
use std::time::Duration;

mod ana;
mod catalog;
mod geography;

use ana::*;
pub(crate) use catalog::*;
pub(crate) use geography::*;

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_SOURCE_BYTES: usize = 120_000_000;
const MIUR_BASE: &str = "https://dati.istruzione.it/opendata/opendata/catalogo/elements1/";
const IPA_PACKAGE_API: &str = "https://indicepa.gov.it/ipa-dati/api/3/action/package_show?id=";
const ANA_STORES_API: &str =
    "https://www.ana.it/wp-json/wp/v2/wpsl_stores?per_page=100&_fields=title,link,content&page=";
const ANA_PAGE_SIZE: usize = 100;
const ANA_MAX_PAGES: usize = 5;
/// One email found in a source row, before geography resolution.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SourceRow {
    pub(crate) email: String,
    pub(crate) is_pec: bool,
    pub(crate) role: Option<String>,
    pub(crate) person_name: Option<String>,
    pub(crate) category: Option<&'static str>,
    pub(crate) subcategory: Option<&'static str>,
    pub(crate) organization: Option<String>,
    pub(crate) comune: String,
    pub(crate) provincia: String,
    pub(crate) regione: String,
    pub(crate) website: Option<String>,
    /// Same meaning as `contacts.official_code`: CODICESCUOLA, cod_amm, cod_uni_ou.
    pub(crate) official_code: Option<String>,
    /// Same meaning as `contacts.department`: the office or group inside the organization.
    pub(crate) department: Option<String>,
    /// Page the row was read from, when more specific than the source URL.
    pub(crate) source_url: Option<String>,
    /// Preferred over an earlier row with the same address (MIUR head schools).
    pub(crate) primary: bool,
    pub(crate) address: Option<String>,
    pub(crate) postal_code: Option<String>,
}

fn present(value: &str) -> Option<&str> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if value.is_empty() || lower == "null" || lower == "non disponibile" {
        None
    } else {
        Some(value)
    }
}

fn source_email(value: &str) -> Option<String> {
    let cleaned = clean_email(present(value)?);
    is_valid_email_syntax(&cleaned).then_some(cleaned)
}

/// MIUR writes sites as `www.x.it`, `https//x.it` or with stray casing.
pub(crate) fn normalize_website(value: &str) -> Option<String> {
    let value = present(value)?.trim_end_matches('/');
    let lower = value.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .or_else(|| lower.strip_prefix("https//"))
        .or_else(|| lower.strip_prefix("http//"))
        .unwrap_or(&lower);
    (rest.contains('.') && !rest.contains(' ')).then(|| format!("https://{rest}"))
}

fn miur_subcategory(tipo: &str) -> &'static str {
    let tipo = tipo.to_ascii_uppercase();
    if tipo.contains("INFANZIA") {
        "Infanzia"
    } else if tipo.contains("PRIMARIA") {
        "Primaria"
    } else if tipo.contains("PRIMO GRADO") {
        "Secondaria di primo grado"
    } else if tipo == "ISTITUTO COMPRENSIVO" {
        "Istituto comprensivo"
    } else if tipo == "CENTRO TERRITORIALE" {
        "Centro territoriale / CPIA"
    } else {
        "Secondaria di secondo grado"
    }
}

const PA: &str = "Pubblica amministrazione";

/// `tipologia_istat` prefix -> taxonomy. Order matters: specific prefixes
/// ("Agenzie Regionali Sanitarie") come before generic ones ("Consorzi").
/// Unlisted types (public-service companies, contracting stations) stay
/// without a category.
const ISTAT_TAXONOMY: &[(&str, &str, Option<&str>)] = &[
    ("Istituti di Istruzione Statale", "Istruzione", None),
    ("Comuni e loro", PA, Some("Comune")),
    ("Unioni di Comuni", PA, Some("Consorzio / Ente pubblico")),
    ("Comunita' Montane", PA, Some("Comunità montana")),
    (
        "Province e loro",
        PA,
        Some("Provincia / Città metropolitana"),
    ),
    (
        "Citta' Metropolitane",
        PA,
        Some("Provincia / Città metropolitana"),
    ),
    (
        "Regioni, Province Autonome",
        PA,
        Some("Regione / Ente regionale"),
    ),
    (
        "Agenzie Regionali Sanitarie",
        "Sanità",
        Some("ASL / ASST / Distretto sanitario"),
    ),
    (
        "Agenzie ed Enti Regionali",
        PA,
        Some("Regione / Ente regionale"),
    ),
    ("Agenzie Regionali", PA, Some("Regione / Ente regionale")),
    (
        "Presidenza del Consiglio",
        PA,
        Some("Ministero / Ente statale"),
    ),
    ("Agenzie Fiscali", PA, Some("Ministero / Ente statale")),
    (
        "Organi Costituzionali",
        PA,
        Some("Ministero / Ente statale"),
    ),
    (
        "Autorita' Amministrative",
        PA,
        Some("Ministero / Ente statale"),
    ),
    (
        "Aziende ed Amministrazioni dello Stato",
        PA,
        Some("Ministero / Ente statale"),
    ),
    ("Forze di Polizia", PA, Some("Forze dell'ordine")),
    (
        "Aziende Sanitarie Locali",
        "Sanità",
        Some("ASL / ASST / Distretto sanitario"),
    ),
    (
        "Aziende Ospedaliere",
        "Sanità",
        Some("Ospedale / Azienda ospedaliera"),
    ),
    (
        "Istituti Zooprofilattici",
        "Sanità",
        Some("Altro ente sanitario"),
    ),
    (
        "Aziende Pubbliche di Servizi alla Persona",
        "Sanità",
        Some("Altro ente sanitario"),
    ),
    (
        "Universita'",
        "Istruzione",
        Some("Università / alta formazione"),
    ),
    (
        "Istituzioni per l'Alta Formazione",
        "Istruzione",
        Some("Università / alta formazione"),
    ),
    (
        "Consorzi Interuniversitari",
        "Istruzione",
        Some("Università / alta formazione"),
    ),
    (
        "Agenzie, Enti e Consorzi Pubblici per il Diritto allo Studio",
        "Istruzione",
        Some("Università / alta formazione"),
    ),
    (
        "Enti e Istituzioni di Ricerca",
        "Istruzione",
        Some("Università / alta formazione"),
    ),
    ("Teatri Stabili", "Cultura", Some("Teatro")),
    ("Fondazioni Lirico", "Cultura", Some("Teatro")),
    (
        "Enti Pubblici Produttori di Servizi Assistenziali, Ricreativi e Culturali",
        "Cultura",
        Some("Altro ente culturale"),
    ),
    (
        "Agenzie ed Enti per il Turismo",
        "Turismo e promozione territoriale",
        Some("APT"),
    ),
    ("Parchi Nazionali", PA, Some("Consorzio / Ente pubblico")),
    ("Consorzi", PA, Some("Consorzio / Ente pubblico")),
    ("Enti di Regolazione", PA, Some("Consorzio / Ente pubblico")),
    ("Autorita' di Bacino", PA, Some("Consorzio / Ente pubblico")),
    ("Autorita' Portuali", PA, Some("Consorzio / Ente pubblico")),
    (
        "Aziende e Consorzi Pubblici Territoriali",
        PA,
        Some("Consorzio / Ente pubblico"),
    ),
    ("Camere di Commercio", PA, Some("Altro ente pubblico")),
    (
        "Federazioni Nazionali, Ordini",
        PA,
        Some("Altro ente pubblico"),
    ),
    ("Automobile Club", PA, Some("Altro ente pubblico")),
    (
        "Enti Pubblici Non Economici",
        PA,
        Some("Altro ente pubblico"),
    ),
    ("Altri Enti Locali", PA, Some("Altro ente pubblico")),
    ("Commissari Straordinari", PA, Some("Altro ente pubblico")),
];

fn istat_taxonomy(tipologia: &str) -> (Option<&'static str>, Option<&'static str>) {
    ISTAT_TAXONOMY
        .iter()
        .find(|(prefix, _, _)| tipologia.starts_with(prefix))
        .map_or((None, None), |(_, category, subcategory)| {
            (Some(*category), *subcategory)
        })
}

/// Header-indexed view over a CSV/TSV record, tolerant of a UTF-8 BOM.
struct Columns(HashMap<String, usize>);

impl Columns {
    fn new(headers: &csv::StringRecord) -> Self {
        Self(
            headers
                .iter()
                .enumerate()
                .map(|(index, name)| (name.trim_start_matches('\u{feff}').trim().to_owned(), index))
                .collect(),
        )
    }

    fn get<'a>(&self, record: &'a csv::StringRecord, name: &str) -> &'a str {
        self.0
            .get(name)
            .and_then(|index| record.get(*index))
            .unwrap_or("")
    }

    fn require(&self, names: &[&str]) -> Result<(), String> {
        match names.iter().find(|name| !self.0.contains_key(**name)) {
            Some(missing) => Err(format!(
                "colonna {missing} assente: formato della fonte cambiato"
            )),
            None => Ok(()),
        }
    }
}

fn delimited_reader(text: &str, delimiter: u8) -> csv::Reader<&[u8]> {
    csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .quoting(delimiter != b'\t')
        .flexible(true)
        .from_reader(text.as_bytes())
}

pub(crate) fn parse_miur(text: &str) -> Result<Vec<SourceRow>, String> {
    let mut reader = delimited_reader(text, b',');
    let columns = Columns::new(reader.headers().map_err(|error| error.to_string())?);
    columns.require(&[
        "CODICESCUOLA",
        "DENOMINAZIONESCUOLA",
        "DESCRIZIONECOMUNE",
        "PROVINCIA",
        "REGIONE",
        "DESCRIZIONETIPOLOGIAGRADOISTRUZIONESCUOLA",
        "INDIRIZZOEMAILSCUOLA",
        "INDIRIZZOPECSCUOLA",
        "SITOWEBSCUOLA",
    ])?;
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|error| error.to_string())?;
        let get = |name: &str| columns.get(&record, name);
        // State schools list every building (plesso) with the institute's
        // shared mailbox: the institute row names the organization.
        let head_code = present(get("CODICEISTITUTORIFERIMENTO"));
        let primary = head_code.is_none_or(|code| code == get("CODICESCUOLA"));
        let organization = present(get("DENOMINAZIONEISTITUTORIFERIMENTO"))
            .or_else(|| present(get("DENOMINAZIONESCUOLA")))
            .map(str::to_owned);
        let subcategory = miur_subcategory(get("DESCRIZIONETIPOLOGIAGRADOISTRUZIONESCUOLA"));
        for (column, is_pec) in [
            ("INDIRIZZOEMAILSCUOLA", false),
            ("INDIRIZZOPECSCUOLA", true),
        ] {
            let Some(email) = source_email(get(column)) else {
                continue;
            };
            rows.push(SourceRow {
                is_pec: is_pec || is_pec_email(&email),
                email,
                role: None,
                person_name: None,
                category: Some("Istruzione"),
                subcategory: Some(subcategory),
                organization: organization.clone(),
                comune: get("DESCRIZIONECOMUNE").to_owned(),
                provincia: get("PROVINCIA").to_owned(),
                regione: get("REGIONE").to_owned(),
                website: normalize_website(get("SITOWEBSCUOLA")),
                official_code: present(get("CODICESCUOLA")).map(str::to_owned),
                department: None,
                source_url: None,
                primary,
                address: present(get("INDIRIZZOSCUOLA")).map(title_case),
                postal_code: present(get("CAPSCUOLA")).map(str::to_owned),
            });
        }
    }
    Ok(rows)
}

/// True when the local part carries a name or surname token (`anna.bianchi`,
/// `abianchi`), so the address belongs to that person.
fn names_person(email: &str, person: &str) -> bool {
    let local: String = norm(email.split('@').next().unwrap_or(""))
        .split_whitespace()
        .collect();
    norm(person)
        .split_whitespace()
        .any(|token| token.len() >= 3 && local.contains(token))
}

fn is_pec_mail(email: &str, tipo: &str) -> bool {
    is_pec_from_kind(Some(tipo), email)
}

/// `enti` is `amministrazioni.txt`, `uffici` is `ou.txt` (tab separated).
pub(crate) fn parse_indicepa(enti: &str, uffici: &str) -> Result<Vec<SourceRow>, String> {
    struct Ente {
        name: String,
        category: Option<&'static str>,
        subcategory: Option<&'static str>,
        website: Option<String>,
    }
    let mut rows = Vec::new();
    let mut by_code: HashMap<String, Ente> = HashMap::new();

    let mut reader = delimited_reader(enti, b'\t');
    let columns = Columns::new(reader.headers().map_err(|error| error.to_string())?);
    columns.require(&[
        "cod_amm",
        "des_amm",
        "Comune",
        "Provincia",
        "Regione",
        "tipologia_istat",
        "mail1",
    ])?;
    for record in reader.records() {
        let record = record.map_err(|error| error.to_string())?;
        let get = |name: &str| columns.get(&record, name);
        let (category, subcategory) = istat_taxonomy(get("tipologia_istat"));
        let ente = Ente {
            name: get("des_amm").trim().to_owned(),
            category,
            subcategory,
            website: normalize_website(get("sito_istituzionale")),
        };
        // The head of the ente (sindaco, legale rappresentante): the
        // referente of every general mailbox of the ente.
        let head = Some(
            [get("nome_resp"), get("cogn_resp")]
                .into_iter()
                .filter_map(present)
                .map(tidy_case)
                .collect::<Vec<_>>()
                .join(" "),
        )
        .filter(|name| !name.is_empty());
        for index in 1..=5 {
            let Some(email) = source_email(get(&format!("mail{index}"))) else {
                continue;
            };
            rows.push(SourceRow {
                is_pec: is_pec_mail(&email, get(&format!("tipo_mail{index}"))),
                email,
                role: present(get("titolo_resp")).map(tidy_case),
                person_name: head.clone(),
                category,
                subcategory,
                organization: Some(ente.name.clone()),
                comune: get("Comune").to_owned(),
                provincia: get("Provincia").to_owned(),
                regione: get("Regione").to_owned(),
                website: ente.website.clone(),
                official_code: present(get("cod_amm")).map(str::to_owned),
                department: None,
                source_url: None,
                primary: false,
                address: present(get("Indirizzo")).map(str::to_owned),
                postal_code: present(get("Cap")).map(str::to_owned),
            });
        }
        by_code.insert(get("cod_amm").to_owned(), ente);
    }

    let mut reader = delimited_reader(uffici, b'\t');
    let columns = Columns::new(reader.headers().map_err(|error| error.to_string())?);
    columns.require(&[
        "cod_amm",
        "des_ou",
        "comune",
        "provincia",
        "Regione",
        "mail1",
        "mail_resp",
    ])?;
    for record in reader.records() {
        let record = record.map_err(|error| error.to_string())?;
        let get = |name: &str| columns.get(&record, name);
        let ente = by_code.get(get("cod_amm"));
        let office = present(get("des_ou")).map(str::to_owned);
        let base = |email: String,
                    is_pec: bool,
                    role: Option<String>,
                    person_name: Option<String>| SourceRow {
            email,
            is_pec,
            role,
            person_name,
            official_code: present(get("cod_uni_ou")).map(str::to_owned),
            department: office.clone(),
            source_url: None,
            category: ente.and_then(|ente| ente.category),
            subcategory: ente.and_then(|ente| ente.subcategory),
            organization: ente.map(|ente| ente.name.clone()),
            comune: get("comune").to_owned(),
            provincia: get("provincia").to_owned(),
            regione: get("Regione").to_owned(),
            website: ente.and_then(|ente| ente.website.clone()),
            primary: false,
            address: present(get("Indirizzo")).map(str::to_owned),
            postal_code: present(get("Cap")).map(str::to_owned),
        };
        for index in 1..=3 {
            if let Some(email) = source_email(get(&format!("mail{index}"))) {
                let is_pec = is_pec_mail(&email, get(&format!("tipo_mail{index}")));
                rows.push(base(email, is_pec, None, None));
            }
        }
        if let Some(email) = source_email(get("mail_resp")) {
            let person = [get("nome_resp"), get("cogn_resp")]
                .into_iter()
                .filter_map(present)
                .collect::<Vec<_>>()
                .join(" ");
            let is_pec = is_pec_email(&email);
            // A third of mail_resp values are functional mailboxes
            // (tributi@...): the head's name only belongs on a personal one.
            let person_name = names_person(&email, &person).then_some(person);
            rows.push(base(
                email,
                is_pec,
                Some("Responsabile".to_owned()),
                person_name,
            ));
        }
    }
    Ok(rows)
}

const ICCU_BASE: &str = "https://opendata.anagrafe.iccu.sbn.it/";
const OVERPASS_MIRRORS: [&str; 2] = [
    "https://overpass-api.de/api/interpreter",
    "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
];
const WIKIDATA_SPARQL: &str = "https://query.wikidata.org/sparql";
const CKAN_SEARCH: &str = "https://www.dati.gov.it/opendata/api/3/action/package_search";

static XML_BIBLIOTECA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<biblioteca\s([^>]*?)>(.*?)</biblioteca>"#).unwrap());
static XML_ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"([\w-]+)="([^"]*)""#).unwrap());
static XML_CONTATTO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<contatto\s+tipo="([^"]*)"[^>]*>(.*?)</contatto>"#).unwrap()
});

fn xml_text(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}

/// ICCU publishes contacts (`contatti.xml`) and addresses (`territorio.csv`)
/// as separate files keyed by ISIL code. Libraries without an email but with a
/// website become seeds.
pub(crate) fn parse_iccu(contatti_xml: &str, territorio_csv: &str) -> Result<SourceData, String> {
    struct Place {
        address: Option<String>,
        cap: Option<String>,
        comune: String,
        provincia: String,
        regione: String,
    }
    let mut places: HashMap<String, Place> = HashMap::new();
    let mut reader = delimited_reader(territorio_csv, b';');
    let columns = Columns::new(reader.headers().map_err(|error| error.to_string())?);
    columns.require(&[
        "codice-isil",
        "indirizzo",
        "cap",
        "comune",
        "provincia",
        "regione",
    ])?;
    for record in reader.records() {
        let record = record.map_err(|error| error.to_string())?;
        let get = |name: &str| columns.get(&record, name);
        places.insert(
            get("codice-isil").to_owned(),
            Place {
                address: present(get("indirizzo")).map(str::to_owned),
                cap: present(get("cap")).map(str::to_owned),
                comune: get("comune").to_owned(),
                provincia: get("provincia").to_owned(),
                regione: get("regione").to_owned(),
            },
        );
    }
    let mut data = SourceData::default();
    for captures in XML_BIBLIOTECA.captures_iter(contatti_xml) {
        let attributes: HashMap<&str, String> = XML_ATTRIBUTE
            .captures_iter(&captures[1])
            .map(|attribute| (attribute.get(1).unwrap().as_str(), xml_text(&attribute[2])))
            .collect();
        let isil = attributes.get("codice-isil").cloned().unwrap_or_default();
        let name = attributes.get("denominazione").cloned();
        let place = places.get(&isil);
        let mut website = None;
        let mut emails = Vec::new();
        for contact in XML_CONTATTO.captures_iter(&captures[2]) {
            let value = xml_text(&contact[2]);
            match &contact[1] {
                "e-mail" | "pec" => {
                    for found in crate::EMAIL_RE.find_iter(&value) {
                        if let Some(email) = source_email(found.as_str()) {
                            emails.push((email, &contact[1] == "pec"));
                        }
                    }
                }
                "url" if website.is_none() => website = normalize_website(&value),
                _ => {}
            }
        }
        let geo = |place: Option<&Place>| {
            place.map_or((String::new(), String::new(), String::new()), |place| {
                (
                    place.comune.clone(),
                    place.provincia.clone(),
                    place.regione.clone(),
                )
            })
        };
        let (comune, provincia, regione) = geo(place);
        if emails.is_empty() {
            if website.is_some() {
                data.seeds.push(SourceSeed {
                    name,
                    url: website,
                    category: Some("Cultura"),
                    subcategory: Some("Biblioteca"),
                    comune,
                    provincia,
                    regione,
                    address: place.and_then(|place| place.address.clone()),
                    postal_code: place.and_then(|place| place.cap.clone()),
                    official_code: present(&isil).map(str::to_owned),
                });
            }
            continue;
        }
        for (email, pec) in emails {
            data.rows.push(SourceRow {
                is_pec: pec || is_pec_email(&email),
                email,
                role: None,
                person_name: None,
                category: Some("Cultura"),
                subcategory: Some("Biblioteca"),
                organization: name.clone(),
                comune: comune.clone(),
                provincia: provincia.clone(),
                regione: regione.clone(),
                website: website.clone(),
                official_code: present(&isil).map(str::to_owned),
                department: None,
                source_url: None,
                primary: false,
                address: place.and_then(|place| place.address.clone()),
                postal_code: place.and_then(|place| place.cap.clone()),
            });
        }
    }
    Ok(data)
}

/// First file of a zip archive as text.
fn unzip_first(bytes: &[u8]) -> Result<String, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("archivio non valido: {error}"))?;
    let mut file = archive
        .by_index(0)
        .map_err(|error| format!("archivio vuoto: {error}"))?;
    if file.size() > MAX_SOURCE_BYTES as u64 {
        return Err("file troppo grande".to_owned());
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text).map_err(|error| error.to_string())?;
    Ok(text)
}

/// Overpass tag selectors per OpenStreetMap catalog entry.
fn osm_selectors(source_id: &str) -> &'static [&'static str] {
    match source_id {
        "osm-religiosi" => &[
            r#"["amenity"="place_of_worship"]"#,
            r#"["amenity"="monastery"]"#,
        ],
        "osm-cultura" => &[
            r#"["amenity"~"^(library|theatre|cinema|arts_centre)$"]"#,
            r#"["tourism"="museum"]"#,
        ],
        "osm-pa" => &[r#"["amenity"~"^(townhall|police|fire_station|courthouse)$"]"#],
        "osm-istruzione" => &[r#"["amenity"~"^(school|kindergarten|college|university)$"]"#],
        "osm-associazioni" => &[
            r#"["office"~"^(association|ngo)$"]"#,
            r#"["club"]"#,
            r#"["amenity"~"^(community_centre|social_centre)$"]"#,
        ],
        "osm-turismo-sanita" => &[
            r#"["tourism"="information"]["information"="office"]"#,
            r#"["amenity"~"^(hospital|clinic)$"]"#,
        ],
        _ => &[],
    }
}

pub(crate) fn overpass_query(selectors: &[&str]) -> String {
    let parts: String = selectors
        .iter()
        .map(|selector| format!(r#"nwr(area.it){selector}[~"^(contact:)?(email|website)$"~"."];"#))
        .collect();
    format!(
        r#"[out:json][timeout:180];area["ISO3166-1"="IT"][admin_level=2]->.it;({parts});out tags center;"#
    )
}

/// App taxonomy for an OpenStreetMap feature, or `None` when the tags do not
/// describe one of the organizations the app collects.
pub(crate) fn osm_taxonomy(
    tags: &serde_json::Map<String, serde_json::Value>,
) -> Option<(&'static str, Option<&'static str>)> {
    let tag = |key: &str| tags.get(key).and_then(|value| value.as_str()).unwrap_or("");
    let name = tag("name").to_lowercase();
    Some(
        match (tag("amenity"), tag("tourism"), tag("office"), tag("club")) {
            ("place_of_worship", _, _, _) if tag("religion") == "christian" => {
                let subcategory = if name.contains("parrocchia") {
                    "Parrocchia"
                } else if name.contains("santuario") || name.contains("basilica") {
                    "Santuario / basilica"
                } else if name.contains("oratorio") {
                    "Oratorio / centro giovanile"
                } else {
                    "Chiesa"
                };
                (RELIGIOSI, Some(subcategory))
            }
            ("place_of_worship" | "monastery", _, _, _) => {
                (RELIGIOSI, Some("Altro ente religioso"))
            }
            ("library", _, _, _) => ("Cultura", Some("Biblioteca")),
            ("theatre", _, _, _) => ("Cultura", Some("Teatro")),
            ("cinema", _, _, _) => ("Cultura", Some("Cinema")),
            ("arts_centre", _, _, _) => ("Cultura", Some("Centro culturale")),
            (_, "museum", _, _) => ("Cultura", Some("Museo")),
            ("townhall", _, _, _) => (PA, Some("Comune")),
            ("police", _, _, _) => (PA, Some("Forze dell'ordine")),
            ("fire_station", _, _, _) => (PA, Some("Ministero / Ente statale")),
            ("courthouse", _, _, _) => (PA, Some("Tribunale / Ufficio giudiziario")),
            ("kindergarten", _, _, _) => ("Istruzione", Some("Infanzia")),
            ("university", _, _, _) => ("Istruzione", Some("Università / alta formazione")),
            ("school" | "college", _, _, _) => ("Istruzione", None),
            ("hospital", _, _, _) => ("Sanità", Some("Ospedale / Azienda ospedaliera")),
            ("clinic", _, _, _) => ("Sanità", Some("Clinica / IRCCS / Struttura sanitaria")),
            (_, "information", _, _) => (
                "Turismo e promozione territoriale",
                Some("Ufficio turistico"),
            ),
            (_, _, _, "sport") => ("Associazioni", Some("Sportiva")),
            (_, _, _, club) if !club.is_empty() => ("Associazioni", Some("Associazione generica")),
            (_, _, "association" | "ngo", _) | ("community_centre" | "social_centre", _, _, _) => {
                ("Associazioni", Some("Associazione generica"))
            }
            _ => return None,
        },
    )
}

pub(crate) fn parse_osm(body: &serde_json::Value) -> SourceData {
    let mut data = SourceData::default();
    for element in body["elements"].as_array().into_iter().flatten() {
        let Some(tags) = element["tags"].as_object() else {
            continue;
        };
        let Some((category, subcategory)) = osm_taxonomy(tags) else {
            continue;
        };
        let tag = |key: &str| {
            tags.get(key)
                .and_then(|value| value.as_str())
                .and_then(present)
        };
        let name = tag("name").map(str::to_owned);
        let website = tag("website")
            .or_else(|| tag("contact:website"))
            .or_else(|| tag("url"));
        let address = tag("addr:street").map(|street| match tag("addr:housenumber") {
            Some(number) => format!("{street} {number}"),
            None => street.to_owned(),
        });
        let comune = tag("addr:city").unwrap_or_default().to_owned();
        let provincia = tag("addr:province").unwrap_or_default().to_owned();
        let postal_code = tag("addr:postcode").map(str::to_owned);
        let official_code = element["id"]
            .as_i64()
            .map(|id| format!("osm:{}/{id}", element["type"].as_str().unwrap_or("node")));
        let emails: Vec<String> = [tag("email"), tag("contact:email")]
            .into_iter()
            .flatten()
            .flat_map(|value| {
                crate::EMAIL_RE
                    .find_iter(value)
                    .map(|found| found.as_str().to_owned())
                    .collect::<Vec<_>>()
            })
            .filter_map(|value| source_email(&value))
            .collect();
        if emails.is_empty() {
            data.seeds.push(SourceSeed {
                name,
                url: website.map(str::to_owned),
                category: Some(category),
                subcategory,
                comune,
                provincia,
                regione: String::new(),
                address,
                postal_code,
                official_code,
            });
            continue;
        }
        for email in emails {
            data.rows.push(SourceRow {
                is_pec: is_pec_email(&email),
                email,
                role: None,
                person_name: None,
                category: Some(category),
                subcategory,
                organization: name.clone(),
                comune: comune.clone(),
                provincia: provincia.clone(),
                regione: String::new(),
                website: website.and_then(normalize_website),
                official_code: official_code.clone(),
                department: None,
                source_url: None,
                primary: false,
                address: address.clone(),
                postal_code: postal_code.clone(),
            });
        }
    }
    data
}

async fn fetch_overpass(
    client: &reqwest::Client,
    query: &str,
) -> Result<serde_json::Value, String> {
    let mut last_error = String::new();
    // ponytail: two public mirrors in order; a paid or self-hosted Overpass
    // instance is the upgrade when both are busy.
    for mirror in OVERPASS_MIRRORS {
        let response = client.post(mirror).form(&[("data", query)]).send().await;
        match response {
            Ok(response) if response.status().is_success() => {
                let text = read_text(response, MAX_SOURCE_BYTES).await?;
                match serde_json::from_str(&text) {
                    Ok(value) => return Ok(value),
                    Err(_) => last_error = format!("{mirror}: risposta non JSON (server occupato)"),
                }
            }
            Ok(response) => last_error = format!("{mirror}: HTTP {}", response.status().as_u16()),
            Err(error) => last_error = format!("{mirror}: {error}"),
        }
    }
    Err(format!(
        "OpenStreetMap non disponibile, riprova più tardi ({last_error})"
    ))
}

/// `(Wikidata class, category, subcategory)` per Wikidata catalog entry.
fn wikidata_classes(source_id: &str) -> &'static [(&'static str, &'static str, &'static str)] {
    match source_id {
        "wikidata-cultura" => &[
            ("Q33506", "Cultura", "Museo"),
            ("Q166118", "Cultura", "Altro ente culturale"),
            ("Q24354", "Cultura", "Teatro"),
            ("Q41253", "Cultura", "Cinema"),
        ],
        "wikidata-religiosi" => &[
            ("Q3146899", RELIGIOSI, "Diocesi / curia"),
            ("Q102496", RELIGIOSI, "Parrocchia"),
            ("Q16970", RELIGIOSI, "Chiesa"),
            ("Q44613", RELIGIOSI, "Altro ente religioso"),
        ],
        _ => &[],
    }
}

pub(crate) fn wikidata_query(classes: &[(&str, &str, &str)]) -> String {
    let values: String = classes
        .iter()
        .map(|(class, _, _)| format!(" wd:{class}"))
        .collect();
    format!(
        "SELECT ?item ?itemLabel ?type ?email ?site ?placeLabel ?address ?cap WHERE {{ VALUES ?type {{{values} }} \
         ?item wdt:P31 ?type ; wdt:P17 wd:Q38 . OPTIONAL {{ ?item wdt:P968 ?email }} \
         OPTIONAL {{ ?item wdt:P856 ?site }} FILTER(BOUND(?email) || BOUND(?site)) \
         OPTIONAL {{ ?item wdt:P131 ?place }} OPTIONAL {{ ?item wdt:P6375 ?address }} \
         OPTIONAL {{ ?item wdt:P281 ?cap }} \
         SERVICE wikibase:label {{ bd:serviceParam wikibase:language \"it,en\". }} }}"
    )
}

pub(crate) fn parse_wikidata(
    body: &serde_json::Value,
    classes: &[(&str, &'static str, &'static str)],
) -> SourceData {
    let mut data = SourceData::default();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for binding in body["results"]["bindings"].as_array().into_iter().flatten() {
        let value = |key: &str| binding[key]["value"].as_str().and_then(present);
        let item = value("item").unwrap_or_default().to_owned();
        let class = value("type")
            .and_then(|uri| uri.rsplit('/').next())
            .unwrap_or("");
        let Some((_, category, subcategory)) = classes.iter().find(|(id, _, _)| *id == class)
        else {
            continue;
        };
        let name = value("itemLabel")
            .filter(|label| !label.starts_with('Q') || label.contains(' '))
            .map(str::to_owned);
        let comune = value("placeLabel").unwrap_or_default().to_owned();
        let website = value("site");
        let official_code = item.rsplit('/').next().map(|id| format!("wikidata:{id}"));
        let email = value("email")
            .map(|raw| raw.trim_start_matches("mailto:"))
            .and_then(source_email);
        let key = (item.clone(), email.clone().unwrap_or_default());
        if !seen.insert(key) {
            continue;
        }
        match email {
            Some(email) => data.rows.push(SourceRow {
                is_pec: is_pec_email(&email),
                email,
                role: None,
                person_name: None,
                category: Some(category),
                subcategory: Some(subcategory),
                organization: name,
                comune,
                provincia: String::new(),
                regione: String::new(),
                website: website.and_then(normalize_website),
                official_code,
                department: None,
                source_url: None,
                primary: false,
                address: value("address").map(str::to_owned),
                postal_code: value("cap").map(str::to_owned),
            }),
            None => data.seeds.push(SourceSeed {
                name,
                url: website.map(str::to_owned),
                category: Some(category),
                subcategory: Some(subcategory),
                comune,
                official_code,
                ..SourceSeed::default()
            }),
        }
    }
    data
}

/// The public SPARQL endpoint times out now and then: one retry after a pause.
async fn fetch_wikidata(
    client: &reqwest::Client,
    query: &str,
) -> Result<serde_json::Value, String> {
    match fetch_wikidata_once(client, query).await {
        Ok(value) => Ok(value),
        Err(_) => {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            fetch_wikidata_once(client, query).await
        }
    }
}

async fn fetch_wikidata_once(
    client: &reqwest::Client,
    query: &str,
) -> Result<serde_json::Value, String> {
    let response = client
        .post(WIKIDATA_SPARQL)
        .header(reqwest::header::ACCEPT, "application/sparql-results+json")
        .form(&[("query", query)])
        .send()
        .await
        .map_err(|error| format!("Wikidata: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("Wikidata: HTTP {}", response.status().as_u16()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Wikidata: {error}"))
}

/// One downloadable table of a dati.gov.it dataset.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct OpenDatasetResource {
    pub(crate) url: String,
    pub(crate) name: String,
    pub(crate) format: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct OpenDataset {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) organization: String,
    pub(crate) resources: Vec<OpenDatasetResource>,
}

/// CKAN `package_search` result -> datasets with at least one CSV or Excel table.
pub(crate) fn parse_ckan_search(body: &serde_json::Value) -> Vec<OpenDataset> {
    body["result"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|package| {
            let resources: Vec<OpenDatasetResource> = package["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|resource| {
                    let url = resource["url"].as_str()?.trim();
                    let format = resource["format"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_uppercase();
                    let lower = url.to_lowercase();
                    let tabular = ["CSV", "XLSX", "XLS"].contains(&format.as_str())
                        || lower.ends_with(".csv")
                        || lower.ends_with(".xlsx");
                    (tabular && lower.starts_with("http")).then(|| OpenDatasetResource {
                        url: url.to_owned(),
                        name: resource["name"].as_str().unwrap_or("").trim().to_owned(),
                        format: if format.is_empty() {
                            "CSV".to_owned()
                        } else {
                            format
                        },
                    })
                })
                .collect();
            (!resources.is_empty()).then(|| OpenDataset {
                id: package["name"].as_str().unwrap_or("").to_owned(),
                title: package["title"].as_str().unwrap_or("").trim().to_owned(),
                organization: package["organization"]["title"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
                resources,
            })
        })
        .collect()
}

/// Header positions a generic open-data table maps onto a contact.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct DatasetColumns {
    pub(crate) emails: Vec<(usize, bool)>,
    pub(crate) name: Option<usize>,
    pub(crate) comune: Option<usize>,
    pub(crate) provincia: Option<usize>,
    pub(crate) regione: Option<usize>,
    pub(crate) cap: Option<usize>,
    pub(crate) address: Option<usize>,
    pub(crate) website: Option<usize>,
}

pub(crate) fn detect_dataset_columns(headers: &[String], sample: &[Vec<String>]) -> DatasetColumns {
    let keys: Vec<String> = headers
        .iter()
        .map(|header| norm(header.trim_start_matches('\u{feff}')))
        .collect();
    let find = |wanted: &[&str], avoid: &[&str]| {
        wanted.iter().find_map(|word| {
            keys.iter().position(|key| {
                key.split_whitespace()
                    .any(|token| token == *word || (word.len() > 3 && token.starts_with(word)))
                    && !avoid.iter().any(|bad| key.contains(bad))
            })
        })
    };
    let mut columns = DatasetColumns {
        name: find(
            &[
                "denominazione",
                "ragione",
                "nome",
                "associazione",
                "ente",
                "struttura",
                "titolo",
            ],
            &[
                "cod",
                "comune",
                "presidente",
                "legale",
                "file",
                "luogo",
                "numero",
                "categoria",
                "id ",
            ],
        ),
        comune: find(&["comune", "citta", "localita"], &["cod", "istat"]),
        provincia: find(&["provincia", "prov", "pr", "sigla"], &["cod", "istat"]),
        regione: find(&["regione"], &["cod", "istat"]),
        cap: find(&["cap"], &[]),
        address: find(&["indirizzo", "via", "sede"], &["mail", "web", "legale"]),
        website: find(
            &["sito", "web", "url", "www"],
            &["mail", "facebook", "instagram", "twitter"],
        ),
        ..DatasetColumns::default()
    };
    for (index, key) in keys.iter().enumerate() {
        if key.contains("mail")
            || key.contains("posta elettronica")
            || key == "pec"
            || key.starts_with("pec ")
        {
            columns.emails.push((index, key.contains("pec")));
        }
    }
    if columns.emails.is_empty() {
        // No email header: keep columns where most sampled values are addresses.
        for index in 0..headers.len() {
            let values: Vec<&str> = sample
                .iter()
                .filter_map(|row| row.get(index).map(String::as_str).and_then(present))
                .collect();
            let hits = values.iter().filter(|value| value.contains('@')).count();
            if !values.is_empty() && hits * 2 > values.len() {
                columns.emails.push((index, false));
            }
        }
    }
    if columns.website == columns.name {
        columns.website = None;
    }
    columns
}

/// Rows of a CSV (any common delimiter, UTF-8 or Windows-1252) or Excel file.
pub(crate) fn read_table(
    bytes: &[u8],
    url: &str,
) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let lower = url.to_lowercase();
    if bytes.starts_with(b"PK") || lower.ends_with(".xlsx") || lower.ends_with(".xls") {
        use calamine::Reader;
        let mut workbook =
            calamine::open_workbook_auto_from_rs(std::io::Cursor::new(bytes.to_vec()))
                .map_err(|error| format!("foglio non leggibile: {error}"))?;
        let range = workbook
            .worksheet_range_at(0)
            .ok_or_else(|| "foglio vuoto".to_owned())?
            .map_err(|error| error.to_string())?;
        let mut rows = range.rows().map(|row| {
            row.iter()
                .map(|cell| cell.to_string().trim().to_owned())
                .collect::<Vec<_>>()
        });
        let headers = rows.next().ok_or_else(|| "foglio vuoto".to_owned())?;
        return Ok((headers, rows.collect()));
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    };
    let first_line = text.lines().next().unwrap_or("");
    let delimiter = b";,\t|"
        .iter()
        .copied()
        .max_by_key(|delimiter| first_line.matches(*delimiter as char).count())
        .unwrap_or(b',');
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(text.as_bytes());
    let headers = reader
        .headers()
        .map_err(|error| error.to_string())?
        .iter()
        .map(str::to_owned)
        .collect();
    let rows = reader
        .records()
        .filter_map(Result::ok)
        .map(|record| record.iter().map(|cell| cell.trim().to_owned()).collect())
        .collect();
    Ok((headers, rows))
}

/// Map a generic open-data table onto leads (rows with emails) and seeds
/// (rows with only a website, or a name and a comune to search for).
pub(crate) fn parse_open_table(
    headers: &[String],
    rows: &[Vec<String>],
) -> Result<SourceData, String> {
    let columns = detect_dataset_columns(headers, &rows[..rows.len().min(50)]);
    if columns.emails.is_empty() && columns.website.is_none() && columns.name.is_none() {
        return Err("nessuna colonna con email, sito o denominazione".to_owned());
    }
    let mut data = SourceData::default();
    for row in rows {
        let cell = |index: Option<usize>| {
            index
                .and_then(|index| row.get(index))
                .map(String::as_str)
                .and_then(present)
        };
        let name = cell(columns.name).map(str::to_owned);
        let comune = cell(columns.comune).unwrap_or_default().to_owned();
        let provincia = cell(columns.provincia).unwrap_or_default().to_owned();
        let regione = cell(columns.regione).unwrap_or_default().to_owned();
        let website = cell(columns.website).and_then(normalize_website);
        let address = cell(columns.address).map(str::to_owned);
        let postal_code = cell(columns.cap).map(str::to_owned);
        let mut emails: Vec<(String, bool)> = Vec::new();
        for (index, pec) in &columns.emails {
            for found in crate::EMAIL_RE.find_iter(cell(Some(*index)).unwrap_or("")) {
                if let Some(email) = source_email(found.as_str())
                    && !emails
                        .iter()
                        .any(|(known, _)| known.eq_ignore_ascii_case(&email))
                {
                    emails.push((email, *pec));
                }
            }
        }
        if emails.is_empty() {
            if website.is_some() || (name.is_some() && !comune.is_empty()) {
                data.seeds.push(SourceSeed {
                    name,
                    url: website,
                    comune,
                    provincia,
                    regione,
                    address,
                    postal_code,
                    ..SourceSeed::default()
                });
            }
            continue;
        }
        for (email, pec) in emails {
            data.rows.push(SourceRow {
                is_pec: pec || is_pec_email(&email),
                email,
                role: None,
                person_name: None,
                category: None,
                subcategory: None,
                organization: name.clone(),
                comune: comune.clone(),
                provincia: provincia.clone(),
                regione: regione.clone(),
                website: website.clone(),
                official_code: None,
                department: None,
                source_url: None,
                primary: false,
                address: address.clone(),
                postal_code: postal_code.clone(),
            });
        }
    }
    Ok(data)
}

async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = get_ok(client, url)
        .await
        .map_err(|error| format!("{url}: {}", error.message))?;
    read_limited(response, MAX_SOURCE_BYTES)
        .await
        .map_err(|error| format!("{url}: {error}"))
}

/// Deduplicate case-insensitively (a primary row replaces an earlier
/// non-primary one), resolve geography and build the leads.
pub(crate) fn source_leads(
    rows: Vec<SourceRow>,
    geography: &Geography,
    source_url: &str,
    now: &str,
) -> Vec<LeadAccumulator> {
    let mut unique: BTreeMap<String, SourceRow> = BTreeMap::new();
    for row in rows {
        let key = crate::parsing::email_comparison_key(&row.email);
        match unique.get(&key) {
            Some(existing) if existing.primary || !row.primary => {}
            _ => {
                unique.insert(key, row);
            }
        }
    }
    type PlaceCache =
        HashMap<(String, String, String), (Option<String>, Option<String>, Option<String>)>;
    let mut cache: PlaceCache = HashMap::new();
    unique
        .into_values()
        .map(|row| {
            let (comune, provincia, regione) = cache
                .entry((
                    row.comune.clone(),
                    row.provincia.clone(),
                    row.regione.clone(),
                ))
                .or_insert_with(|| geography.resolve(&row.comune, &row.provincia, &row.regione))
                .clone();
            // No municipality from the source's own field (a hamlet, a
            // district, nothing): the town written after the CAP in the address.
            let town = row.address.as_deref().and_then(address_town);
            let (comune, provincia, regione) = match town {
                Some(town)
                    if !comune
                        .as_deref()
                        .is_some_and(|name| geography.is_municipality(name))
                        && geography.is_municipality(&town) =>
                {
                    let from_address = geography.resolve(&town, &row.provincia, &row.regione);
                    if from_address.2.is_some() {
                        from_address
                    } else {
                        (comune, provincia, regione)
                    }
                }
                _ => (comune, provincia, regione),
            };
            LeadAccumulator::from_source(
                row.email,
                row.is_pec,
                row.source_url.as_deref().unwrap_or(source_url),
                now,
                row.role,
                row.person_name,
                LeadContext {
                    category: row.category.map(str::to_owned),
                    subcategory: row.subcategory.map(str::to_owned),
                    organization: row.organization,
                    comune,
                    provincia,
                    regione,
                    website: row.website,
                    official_code: row.official_code,
                    department: row.department,
                    address: row.address,
                    postal_code: row.postal_code,
                    paese: None,
                },
            )
        })
        .collect()
}

/// A place a source lists without a usable address: its website (or only its
/// name) becomes a seed for the crawl, with everything the source knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SourceSeed {
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) category: Option<&'static str>,
    pub(crate) subcategory: Option<&'static str>,
    pub(crate) comune: String,
    pub(crate) provincia: String,
    pub(crate) regione: String,
    pub(crate) address: Option<String>,
    pub(crate) postal_code: Option<String>,
    pub(crate) official_code: Option<String>,
}

/// Everything one source import produced.
#[derive(Debug, Default)]
pub(crate) struct SourceData {
    pub(crate) rows: Vec<SourceRow>,
    pub(crate) seeds: Vec<SourceSeed>,
    pub(crate) url: String,
    /// Tables of the source that could not be read, kept in the run and shown to the user.
    pub(crate) errors: Vec<String>,
}

/// ISTAT municipalities with their CAP (2020 edition; CAP rarely change).
const CAP_TABLE_URL: &str =
    "https://raw.githubusercontent.com/matteocontrini/comuni-json/master/comuni.json";
static CAP_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\d{5})\b").unwrap());

/// CAP -> `(comune, provincia, regione)` of every municipality it serves.
#[derive(Default)]
pub(crate) struct CapTable(HashMap<String, Vec<(String, String, String)>>);

impl CapTable {
    pub(crate) fn parse(json: &str) -> Result<Self, String> {
        let items: Vec<serde_json::Value> =
            serde_json::from_str(json).map_err(|error| format!("tabella CAP: {error}"))?;
        // "Bolzano/Bozen", "Valle d'Aosta/Vallée d'Aoste": the Italian half.
        let italian = |value: &serde_json::Value| {
            value
                .as_str()
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let mut table: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        for item in &items {
            let place = (
                italian(&item["nome"]),
                place_display_name("province", &italian(&item["provincia"]["nome"])),
                place_display_name("region", &italian(&item["regione"]["nome"])),
            );
            for cap in item["cap"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|cap| cap.as_str())
            {
                table.entry(cap.to_owned()).or_default().push(place.clone());
            }
        }
        Ok(Self(table))
    }

    /// Place from the CAP when the source's own municipality is missing or
    /// unknown (a hamlet, a district) and the address names none either. A
    /// CAP served by one municipality gives the whole place; a shared CAP
    /// gives province and region only, when all its municipalities agree.
    pub(crate) fn locate(&self, rows: &mut [SourceRow], geography: &Geography) {
        for row in rows {
            let address_town = row.address.as_deref().and_then(address_town);
            if geography.is_municipality(&row.comune)
                || address_town
                    .as_deref()
                    .is_some_and(|town| geography.is_municipality(town))
            {
                continue;
            }
            let Some(places) = row_cap(row).and_then(|cap| self.0.get(&cap)) else {
                continue;
            };
            let (comune, provincia, regione) = &places[0];
            let same_province = places.iter().all(|place| place.1 == *provincia);
            if places.len() == 1 {
                row.comune = comune.clone();
            } else if same_province {
                row.comune = String::new();
            } else {
                continue;
            }
            row.provincia = provincia.clone();
            row.regione = regione.clone();
        }
    }
}

fn row_cap(row: &SourceRow) -> Option<String> {
    [row.postal_code.as_deref(), row.address.as_deref()]
        .into_iter()
        .flatten()
        .find_map(|text| {
            CAP_CODE
                .captures(text)
                .map(|captures| captures[1].to_owned())
        })
}

/// Resolve places from the CAP table, downloaded only when some row needs it.
/// A failed download leaves the places as the source wrote them.
async fn locate_by_cap(client: &reqwest::Client, rows: &mut [SourceRow], geography: &Geography) {
    let needs_cap = rows
        .iter()
        .any(|row| !geography.is_municipality(&row.comune) && row_cap(row).is_some());
    if !needs_cap {
        return;
    }
    if let Ok(table) = download_text(client, CAP_TABLE_URL)
        .await
        .and_then(|json| CapTable::parse(&json))
    {
        table.locate(rows, geography);
    }
}

/// Resolve the seeds' geography and turn them into crawl seeds.
pub(crate) fn source_seeds(
    seeds: Vec<SourceSeed>,
    geography: &Geography,
) -> Vec<crate::lead_crawl::SeedRow> {
    seeds
        .into_iter()
        .map(|seed| {
            let (comune, provincia, regione) =
                geography.resolve(&seed.comune, &seed.provincia, &seed.regione);
            crate::lead_crawl::SeedRow {
                url: seed.url.as_deref().and_then(normalize_website),
                name: seed.name,
                context: LeadContext {
                    category: seed.category.map(str::to_owned),
                    subcategory: seed.subcategory.map(str::to_owned),
                    comune,
                    provincia,
                    regione,
                    address: seed.address,
                    postal_code: seed.postal_code,
                    official_code: seed.official_code,
                    ..LeadContext::default()
                },
            }
        })
        .collect()
}

async fn download_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let bytes = download_bytes(client, url).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The dataset page lists every school year, newest first.
pub(crate) fn latest_miur_file(landing_html: &str, prefix: &str) -> Option<String> {
    let pattern = regex::Regex::new(&format!(r#"href="({prefix}\d+\.csv)""#)).ok()?;
    pattern
        .captures(landing_html)
        .map(|captures| captures[1].to_owned())
}

/// CKAN `package_show` result -> URL of the `.txt` resource.
pub(crate) fn ckan_txt_resource(package_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(package_json).ok()?;
    value["result"]["resources"]
        .as_array()?
        .iter()
        .filter_map(|resource| resource["url"].as_str())
        .find(|url| url.ends_with(".txt"))
        .map(str::to_owned)
}

/// One dati.gov.it table of a `ckan-*` source: the CKAN dataset name, the
/// region the publisher covers (when the table has no region column) and the
/// taxonomy subcategory of its rows.
struct CkanTable {
    dataset: &'static str,
    regione: &'static str,
    subcategory: &'static str,
}

const fn table(
    dataset: &'static str,
    regione: &'static str,
    subcategory: &'static str,
) -> CkanTable {
    CkanTable {
        dataset,
        regione,
        subcategory,
    }
}

/// Curated tables per `ckan-*` source id: `(category, tables)`. Tables were
/// picked by hand from dati.gov.it for having email columns; the resource URL
/// is read from the catalog at import time, so republished files keep working.
fn ckan_bundle(source_id: &str) -> (&'static str, &'static [CkanTable]) {
    match source_id {
        "ckan-turismo" => {
            const TABLES: &[CkanTable] = &[
                table(
                    "elenco-associazioni-pro-loco1",
                    "Trentino Alto Adige",
                    "Pro Loco",
                ),
                table("elenco-delle-pro-loco-anno-2026", "Piemonte", "Pro Loco"),
                table(
                    "sedi-delle-associazioni-pro-loco-citta-metropolitana-di-firenze1",
                    "Toscana",
                    "Pro Loco",
                ),
                table(
                    "associazioni-e-consorzi-turistici",
                    "Umbria",
                    "Consorzio turistico",
                ),
                table("consorzi-turistici", "Piemonte", "Consorzio turistico"),
                table(
                    "uffici-informazione-e-accoglienza-turistica",
                    "Puglia",
                    "Ufficio turistico",
                ),
            ];
            ("Turismo e promozione territoriale", TABLES)
        }
        "ckan-associazioni" => {
            const TABLES: &[CkanTable] = &[
                table(
                    "associazioni-di-volontariato-e-di-promozione-sociale",
                    "Emilia Romagna",
                    "Volontariato / solidarietà",
                ),
                table(
                    "registro-regionale-delle-organizzazioni-di-volontariato",
                    "Campania",
                    "Volontariato / solidarietà",
                ),
                table(
                    "organizzazioni-di-volontariato2",
                    "Umbria",
                    "Volontariato / solidarietà",
                ),
                table(
                    "elenco-delle-organizzazioni-di-volontariato-ambito-protezione-civile1",
                    "Trentino Alto Adige",
                    "Volontariato / solidarietà",
                ),
                table(
                    "associazioni-di-volontariato-di-protezione-civile",
                    "Friuli Venezia Giulia",
                    "Volontariato / solidarietà",
                ),
                table(
                    "elenco-associazioni-di-promozione-sociale1",
                    "Puglia",
                    "Volontariato / solidarietà",
                ),
                table(
                    "elenco-delle-associazioni-di-promozione-sociale1",
                    "Sicilia",
                    "Volontariato / solidarietà",
                ),
                table(
                    "albo-regionale-delle-cooperative-sociali-al-31-01-2026",
                    "Campania",
                    "Cooperativa / Terzo settore",
                ),
                table(
                    "cooperative-sociali1",
                    "Piemonte",
                    "Cooperativa / Terzo settore",
                ),
                table(
                    "cooperative-sociali",
                    "Basilicata",
                    "Cooperativa / Terzo settore",
                ),
                table(
                    "albo-cooperative-sociali-per-il-comune-di-milano",
                    "Lombardia",
                    "Cooperativa / Terzo settore",
                ),
                table(
                    "albo-cooperative-sociali-provincia-di-lodi",
                    "Lombardia",
                    "Cooperativa / Terzo settore",
                ),
                table(
                    "registro-anagrafico-delle-associazioni",
                    "Lombardia",
                    "Associazione generica",
                ),
                table(
                    "cultura-localizzazione-delle-associazioni-culturali",
                    "Lombardia",
                    "Culturale",
                ),
                table(
                    "albo-delle-associazioni-del-comune-di-lecce",
                    "Puglia",
                    "Associazione generica",
                ),
                table(
                    "comune-di-thiene-albo-delle-associazioni",
                    "Veneto",
                    "Associazione generica",
                ),
                table(
                    "registro-associazioni-provincia-di-padova",
                    "Veneto",
                    "Associazione generica",
                ),
                table(
                    "elenco-delle-associazioni-iscritte-nel-registro-comunale",
                    "Piemonte",
                    "Associazione generica",
                ),
                table(
                    "associazioni-giovanili1",
                    "Campania",
                    "Associazione generica",
                ),
            ];
            ("Associazioni", TABLES)
        }
        "ckan-cultura" => {
            const TABLES: &[CkanTable] = &[
                table(
                    "musei-riconosciuti-da-regione-lombardia",
                    "Lombardia",
                    "Museo",
                ),
                table("provincia-monza-brianza-musei", "Lombardia", "Museo"),
                table("ecomusei-in-umbria", "Umbria", "Museo"),
                table("teatri-storici-in-umbria", "Umbria", "Teatro"),
                table(
                    "provincia-monza-brianza-cinema-e-teatri",
                    "Lombardia",
                    "Teatro",
                ),
                table("elenco-biblioteche-umbre-sbn", "Umbria", "Biblioteca"),
                table(
                    "provincia-monza-brianza-biblioteche",
                    "Lombardia",
                    "Biblioteca",
                ),
            ];
            ("Cultura", TABLES)
        }
        "ckan-sanita" => {
            const TABLES: &[CkanTable] = &[
                table(
                    "strutture-di-ricovero-e-cura-2017",
                    "Trentino Alto Adige",
                    "Ospedale / Azienda ospedaliera",
                ),
                table(
                    "strutture-sanitarie-residenziali-per-anziani-non-autosufficienti",
                    "Umbria",
                    "Altro ente sanitario",
                ),
                table(
                    "strutture-di-offerta-sociale-per-anziani",
                    "Lombardia",
                    "Altro ente sanitario",
                ),
                table(
                    "strutture-di-offerta-sociale-residenziale-minori",
                    "Lombardia",
                    "Altro ente sanitario",
                ),
                table(
                    "strutture-di-riposo-per-anziani1",
                    "Emilia Romagna",
                    "Altro ente sanitario",
                ),
            ];
            ("Sanità", TABLES)
        }
        "ckan-formazione" => {
            const TABLES: &[CkanTable] = &[
                table(
                    "sistema-accreditamento-enti-formazione-e-orientamento-regione-lazio",
                    "Lazio",
                    "Formazione professionale",
                ),
                table(
                    "elenco-degli-operatori-accreditati-ai-servizi-da-formazione-nel-comune-di-milano",
                    "Lombardia",
                    "Formazione professionale",
                ),
                table(
                    "percorsi-di-istruzione-e-formazione-tecnica-superiore-gli-its",
                    "Piemonte",
                    "Formazione professionale",
                ),
                table(
                    "percorsi-di-istruzione-e-formazione-tecnica-superiore-gli-ifts",
                    "Piemonte",
                    "Formazione professionale",
                ),
                table(
                    "elenco-istituti-tecnici-superiori-its-di-nuova-costituzione",
                    "Campania",
                    "Formazione professionale",
                ),
                table(
                    "istruzione-caratteristiche-delle-scuole-dinfanzia",
                    "Lombardia",
                    "Infanzia",
                ),
                table("asili-nido-comunali", "Piemonte", "Infanzia"),
                table(
                    "informazioni-anagrafiche-scuole-paritarie-delle-province-autonome-di-aosta-trento-e-bolzano",
                    "",
                    "Altro ente formativo",
                ),
            ];
            ("Istruzione", TABLES)
        }
        "ckan-pa" => {
            const TABLES: &[CkanTable] = &[
                table(
                    "indirizzi-e-telefoni-di-polizia-locale-comunale",
                    "Lombardia",
                    "Forze dell'ordine",
                ),
                table(
                    "indirizzi-e-telefoni-di-polizia-locale-delle-unioni-di-comuni",
                    "Lombardia",
                    "Forze dell'ordine",
                ),
                table(
                    "indirizzi-e-telefoni-di-polizia-locale-provinciale",
                    "Lombardia",
                    "Forze dell'ordine",
                ),
                table(
                    "comunita-montane-lombarde-1",
                    "Lombardia",
                    "Comunità montana",
                ),
                table("vigili-del-fuoco", "Piemonte", "Forze dell'ordine"),
            ];
            ("Pubblica amministrazione", TABLES)
        }
        _ => ("", &[]),
    }
}

/// Give every row and seed of one table the bundle taxonomy, and the
/// publisher's region when the table does not carry one.
fn tag_ckan_table(data: &mut SourceData, category: &'static str, table: &CkanTable) {
    for row in &mut data.rows {
        row.category = Some(category);
        row.subcategory = Some(table.subcategory);
        row.source_url = Some(data.url.clone());
        if row.regione.is_empty() {
            row.regione = table.regione.to_owned();
        }
    }
    for seed in &mut data.seeds {
        seed.category = Some(category);
        seed.subcategory = Some(table.subcategory);
        if seed.regione.is_empty() {
            seed.regione = table.regione.to_owned();
        }
    }
}

/// The first CSV (else the first table) of a dati.gov.it dataset, parsed.
async fn fetch_ckan_table(
    client: &reqwest::Client,
    dataset: &OpenDataset,
) -> Result<SourceData, String> {
    let resource = dataset
        .resources
        .iter()
        .find(|resource| resource.format == "CSV")
        .unwrap_or(&dataset.resources[0]);
    let bytes = download_bytes(client, &resource.url).await?;
    let (headers, rows) = read_table(&bytes, &resource.url)?;
    let mut data = parse_open_table(&headers, &rows)?;
    data.url = resource.url.clone();
    Ok(data)
}

/// The catalog entries of the given dataset names, in one request.
/// dati.gov.it answers 502 under a burst, so this is the only call to it per
/// source, and it waits and asks again a few times.
async fn ckan_datasets(
    client: &reqwest::Client,
    names: &[&str],
) -> Result<Vec<OpenDataset>, String> {
    let filter = names
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut attempt = 1;
    loop {
        let response = client
            .get(CKAN_SEARCH)
            .query(&[
                ("fq", format!("name:({filter})")),
                ("rows", names.len().to_string()),
            ])
            .send()
            .await
            .map_err(|error| format!("dati.gov.it: {error}"))?;
        let status = response.status();
        if status.is_server_error() && attempt < 4 {
            attempt += 1;
            tokio::time::sleep(Duration::from_secs(3 * attempt)).await;
            continue;
        }
        if !status.is_success() {
            return Err(format!("dati.gov.it: HTTP {}", status.as_u16()));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|error| format!("dati.gov.it: {error}"))?;
        return Ok(parse_ckan_search(&body));
    }
}

/// Every table of one `ckan-*` source. A table that is gone or unreadable is
/// skipped and reported in `errors`; the import fails only when no table gave anything.
async fn fetch_ckan_bundle(
    client: &reqwest::Client,
    source_id: &str,
) -> Result<SourceData, String> {
    let (category, tables) = ckan_bundle(source_id);
    let names: Vec<&str> = tables.iter().map(|table| table.dataset).collect();
    let datasets = ckan_datasets(client, &names).await?;
    let mut all = SourceData {
        url: "https://www.dati.gov.it/".to_owned(),
        ..SourceData::default()
    };
    for table in tables {
        let result = match datasets.iter().find(|dataset| dataset.id == table.dataset) {
            Some(dataset) => fetch_ckan_table(client, dataset).await,
            None => Err("non più nel catalogo".to_owned()),
        };
        match result {
            Ok(mut data) => {
                tag_ckan_table(&mut data, category, table);
                all.rows.append(&mut data.rows);
                all.seeds.append(&mut data.seeds);
            }
            Err(error) => all.errors.push(format!("{}: {error}", table.dataset)),
        }
    }
    if all.rows.is_empty() && all.seeds.is_empty() {
        return Err(all
            .errors
            .into_iter()
            .next()
            .unwrap_or_else(|| "nessuna tabella da importare".to_owned()));
    }
    Ok(all)
}

async fn fetch_source_data(
    client: &reqwest::Client,
    source: &LeadSource,
    geography: &Geography,
) -> Result<SourceData, String> {
    let rows = |rows: Vec<SourceRow>, url: String| SourceData {
        rows,
        seeds: Vec::new(),
        url,
        errors: Vec::new(),
    };
    match source.id {
        "miur-statali" | "miur-paritarie" => {
            let prefix = if source.id == "miur-statali" {
                "SCUANAGRAFESTAT"
            } else {
                "SCUANAGRAFEPAR"
            };
            let landing = download_text(client, source.landing_url).await?;
            let file = latest_miur_file(&landing, prefix)
                .ok_or_else(|| format!("{}: nessun file CSV trovato", source.landing_url))?;
            let url = format!("{MIUR_BASE}{file}");
            Ok(rows(parse_miur(&download_text(client, &url).await?)?, url))
        }
        "indicepa" => {
            let mut texts = Vec::new();
            for package in ["amministrazioni", "ou"] {
                let api = format!("{IPA_PACKAGE_API}{package}");
                let url = ckan_txt_resource(&download_text(client, &api).await?)
                    .ok_or_else(|| format!("{api}: risorsa .txt non trovata"))?;
                texts.push(download_text(client, &url).await?);
            }
            Ok(rows(
                parse_indicepa(&texts[0], &texts[1])?,
                source.landing_url.to_owned(),
            ))
        }
        "ana-sezioni" => Ok(rows(
            parse_ana(&fetch_ana(client).await?, geography),
            source.landing_url.to_owned(),
        )),
        "iccu-biblioteche" => {
            let contatti =
                unzip_first(&download_bytes(client, &format!("{ICCU_BASE}contatti.zip")).await?)?;
            let territorio =
                unzip_first(&download_bytes(client, &format!("{ICCU_BASE}territorio.zip")).await?)?;
            let mut data = parse_iccu(&contatti, &territorio)?;
            data.url = source.landing_url.to_owned();
            Ok(data)
        }
        id if id.starts_with("osm-") => {
            let body = fetch_overpass(client, &overpass_query(osm_selectors(id))).await?;
            let mut data = parse_osm(&body);
            data.url = source.landing_url.to_owned();
            Ok(data)
        }
        id if id.starts_with("ckan-") => fetch_ckan_bundle(client, id).await,
        id if id.starts_with("wikidata-") => {
            let classes = wikidata_classes(id);
            let body = fetch_wikidata(client, &wikidata_query(classes)).await?;
            let mut data = parse_wikidata(&body, classes);
            data.url = source.landing_url.to_owned();
            Ok(data)
        }
        _ => Err(format!("La fonte {} non è ancora importabile", source.name)),
    }
}

#[tauri::command]
pub(crate) fn list_lead_sources() -> Vec<LeadSource> {
    CATALOG.to_vec()
}

/// Store a finished source import: leads as a `source` run, seeds in the list
/// named after the source. Returns the run summary.
fn store_source_import(
    db: &Connection,
    geography: &Geography,
    data: SourceData,
    label: &str,
    source_id: Option<&str>,
    now: &str,
) -> Result<SearchRunSummary, String> {
    let leads = source_leads(data.rows, geography, &data.url, now);
    let seeds = source_seeds(data.seeds, geography);
    let errors = data.errors;
    let transaction = db
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let summary = insert_run(
        &transaction,
        &RunRecord {
            kind: "source",
            query_text: label,
            source_id,
            seed_count: seeds.len() as i64,
            page_count: 0,
            pdf_found: 0,
            errors: &errors,
            now,
        },
        leads,
    )?;
    crate::lead_crawl::insert_seeds(&transaction, label, &seeds, now)?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(summary)
}

#[tauri::command]
pub(crate) async fn import_lead_source(
    app: tauri::AppHandle,
    source_id: String,
) -> Result<SearchRunSummary, String> {
    let source = CATALOG
        .iter()
        .find(|source| source.id == source_id)
        .ok_or_else(|| format!("Fonte sconosciuta: {source_id}"))?;
    if !source.importable || source.kind != "dataset" {
        return Err(format!(
            "La fonte {} non è un dataset importabile",
            source.name
        ));
    }
    let client = polite_client(DOWNLOAD_TIMEOUT)
        .map_err(|error| format!("client HTTP non disponibile: {error}"))?;
    let setup_app = app.clone();
    let geography =
        tauri::async_runtime::spawn_blocking(move || Geography::load(&*open_db(&setup_app)?))
            .await
            .map_err(|error| format!("Importazione fonte interrotta: {error}"))??;
    let mut data = fetch_source_data(&client, source, &geography).await?;
    locate_by_cap(&client, &mut data.rows, &geography).await;
    let now = chrono::Utc::now().to_rfc3339();
    tauri::async_runtime::spawn_blocking(move || {
        let db = open_db(&app)?;
        store_source_import(
            &db,
            &geography,
            data,
            &format!("Fonte: {}", source.name),
            Some(source.id),
            &now,
        )
    })
    .await
    .map_err(|error| format!("Importazione fonte interrotta: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::setup_schema;

    const MIUR_FIXTURE: &str = "\
ANNOSCOLASTICO,AREAGEOGRAFICA,REGIONE,PROVINCIA,CODICEISTITUTORIFERIMENTO,DENOMINAZIONEISTITUTORIFERIMENTO,CODICESCUOLA,DENOMINAZIONESCUOLA,INDIRIZZOSCUOLA,CAPSCUOLA,CODICECOMUNESCUOLA,DESCRIZIONECOMUNE,DESCRIZIONECARATTERISTICASCUOLA,DESCRIZIONETIPOLOGIAGRADOISTRUZIONESCUOLA,INDICAZIONESEDEDIRETTIVO,INDICAZIONESEDEOMNICOMPRENSIVO,INDIRIZZOEMAILSCUOLA,INDIRIZZOPECSCUOLA,SITOWEBSCUOLA,SEDESCOLASTICA
202627,NORD OVEST,LOMBARDIA,BERGAMO,BGIC000001,\"I.C. \"\"ESEMPIO\"\"\",BGAA000002,PLESSO INFANZIA,VIA ROMA 1,24100,A794,BERGAMO,NORMALE,SCUOLA INFANZIA,NO,Non Disponibile,BGIC000001@example.test,Non Disponibile,https//www.icesempio.example.test,SI
202627,NORD OVEST,LOMBARDIA,BERGAMO,BGIC000001,\"I.C. \"\"ESEMPIO\"\"\",BGIC000001,I.C. ESEMPIO,VIA ROMA 1,24100,A794,BERGAMO,NORMALE,ISTITUTO COMPRENSIVO,SI,Non Disponibile,BGIC000001@example.test,bgic000001@pec.example.test,www.icesempio.example.test,SI
202627,NORD EST,FRIULI-VENEZIA G.,UDINE,UDIS000001,LICEO ESEMPIO,UDPS000001,LICEO SCIENTIFICO ESEMPIO,VIA UDINE 2,33100,L483,UDINE,NORMALE,LICEO SCIENTIFICO,NO,Non Disponibile,non valida,Non Disponibile,Non Disponibile,SI
";

    const IPA_ENTI_FIXTURE: &str = "\u{feff}cod_amm\tdes_amm\tComune\tnome_resp\tcogn_resp\tCap\tProvincia\tRegione\tsito_istituzionale\tIndirizzo\ttitolo_resp\ttipologia_istat\ttipologia_amm\tacronimo\tcf_validato\tcf\tmail1\ttipo_mail1\tmail2\ttipo_mail2\tmail3\ttipo_mail3\tmail4\ttipo_mail4\tmail5\ttipo_mail5
c_x001\tComune di Esempio\tBergamo\tMARIO\tROSSI\t24100\tBG\tLombardia\twww.comune.example.test\tPiazza 1\tSindaco\tComuni e loro Consorzi e Associazioni\tPubbliche Amministrazioni\t\tS\t000\tprotocollo@pec.comune.example.test\tPec\tinfo@comune.example.test\tAltro\tnull\tnull\tnull\tnull\tnull\tnull
s_x002\tServizi Esempio S.p.A.\tMilano\t\t\t20100\tMI\tLombardia\t\t\t\tGestori di Pubblici Servizi\tGestori di Pubblici Servizi\t\tS\t001\tgestore@example.test\tAltro\tnull\tnull\tnull\tnull\tnull\tnull\tnull\tnull
";

    const IPA_UFFICI_FIXTURE: &str = "\u{feff}cod_ou\tcod_aoo\tdes_ou\tcomune\tCap\tprovincia\tRegione\tIndirizzo\tTel\tnome_resp\tcogn_resp\tmail_resp\ttel_resp\tcod_amm\tcod_ou_padre\tFax\tcod_uni_ou\tmail1\ttipo_mail1\tmail2\ttipo_mail2\tmail3\ttipo_mail3
Uff_1\tAOO1\tServizi Sociali\tBergamo\t24100\tBG\tLombardia\t\t\tAnna\tBianchi\tanna.bianchi@comune.example.test\t\tc_x001\t\t\tABC123\tsociali@comune.example.test\tAltro\tINFO@comune.example.test\tAltro\tnull\tnull
Uff_2\tAOO1\tTributi\tBergamo\t24100\tBG\tLombardia\t\t\tLuca\tVerdi\ttributi.responsabile@comune.example.test\t\tc_x001\t\t\tDEF456\tnull\tnull\tnull\tnull\tnull\tnull
";

    #[test]
    fn lead_sources_miur_rows_map_grade_head_school_and_website() {
        let rows = parse_miur(MIUR_FIXTURE).unwrap();
        let emails: Vec<_> = rows
            .iter()
            .map(|row| (row.email.as_str(), row.is_pec, row.primary))
            .collect();
        assert_eq!(
            emails,
            vec![
                ("BGIC000001@example.test", false, false),
                ("BGIC000001@example.test", false, true),
                ("bgic000001@pec.example.test", true, true),
            ]
        );
        assert_eq!(rows[0].subcategory, Some("Infanzia"));
        assert_eq!(rows[1].subcategory, Some("Istituto comprensivo"));
        assert_eq!(rows[0].organization.as_deref(), Some("I.C. \"ESEMPIO\""));
        assert_eq!(
            rows[0].website.as_deref(),
            Some("https://www.icesempio.example.test")
        );
        assert_eq!(
            rows[1].website.as_deref(),
            Some("https://www.icesempio.example.test")
        );
        assert_eq!(rows[1].official_code.as_deref(), Some("BGIC000001"));
    }

    #[test]
    fn lead_sources_indicepa_maps_istat_type_offices_and_office_heads() {
        let rows = parse_indicepa(IPA_ENTI_FIXTURE, IPA_UFFICI_FIXTURE).unwrap();
        let summary: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.email.as_str(),
                    row.is_pec,
                    row.category,
                    row.role.as_deref(),
                    row.person_name.as_deref(),
                    row.department.as_deref(),
                    row.official_code.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "protocollo@pec.comune.example.test",
                    true,
                    Some(PA),
                    Some("Sindaco"),
                    Some("Mario Rossi"),
                    None,
                    Some("c_x001")
                ),
                (
                    "info@comune.example.test",
                    false,
                    Some(PA),
                    Some("Sindaco"),
                    Some("Mario Rossi"),
                    None,
                    Some("c_x001")
                ),
                (
                    "gestore@example.test",
                    false,
                    None,
                    None,
                    None,
                    None,
                    Some("s_x002")
                ),
                (
                    "sociali@comune.example.test",
                    false,
                    Some(PA),
                    None,
                    None,
                    Some("Servizi Sociali"),
                    Some("ABC123")
                ),
                (
                    "INFO@comune.example.test",
                    false,
                    Some(PA),
                    None,
                    None,
                    Some("Servizi Sociali"),
                    Some("ABC123")
                ),
                (
                    "anna.bianchi@comune.example.test",
                    false,
                    Some(PA),
                    Some("Responsabile"),
                    Some("Anna Bianchi"),
                    Some("Servizi Sociali"),
                    Some("ABC123")
                ),
                (
                    "tributi.responsabile@comune.example.test",
                    false,
                    Some(PA),
                    Some("Responsabile"),
                    None,
                    Some("Tributi"),
                    Some("DEF456")
                ),
            ]
        );
        assert!(
            rows.iter()
                .filter(|row| row.category == Some(PA))
                .all(|row| row.subcategory == Some("Comune"))
        );
        assert_eq!(rows[3].organization.as_deref(), Some("Comune di Esempio"));
        assert_eq!(
            rows[0].website.as_deref(),
            Some("https://www.comune.example.test")
        );
    }

    #[test]
    fn lead_sources_reject_a_changed_source_format() {
        assert!(
            parse_miur("A,B\n1,2\n")
                .unwrap_err()
                .contains("CODICESCUOLA")
        );
        assert!(
            parse_indicepa("x\ty\n", IPA_UFFICI_FIXTURE)
                .unwrap_err()
                .contains("cod_amm")
        );
    }

    #[test]
    fn lead_sources_resolve_geography_to_contacts_vocabulary() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        // MIUR uppercase full names and IndicePA two-letter codes converge.
        let expected = (
            Some("Bergamo".to_owned()),
            Some("Bergamo".to_owned()),
            Some("Lombardia".to_owned()),
        );
        assert_eq!(
            geography.resolve("BERGAMO", "BERGAMO", "LOMBARDIA"),
            expected
        );
        assert_eq!(geography.resolve("Bergamo", "BG", "Lombardia"), expected);
        assert_eq!(
            geography
                .resolve("UDINE", "UDINE", "FRIULI-VENEZIA G.")
                .2
                .as_deref(),
            Some("Friuli Venezia Giulia")
        );
        // A duplicated municipality name is settled by the source region.
        assert!(geography.resolve("Castro", "", "").0.is_some());
        let (_, province, region) = geography.resolve("Castro", "", "Puglia");
        assert_eq!(
            (province.as_deref(), region.as_deref()),
            (Some("Lecce"), Some("Puglia"))
        );
        // Unknown places keep a readable name without inventing parents.
        assert_eq!(
            geography.resolve("FRAZIONE INESISTENTE", "", ""),
            (Some("Frazione Inesistente".to_owned()), None, None)
        );
    }

    #[test]
    fn lead_sources_import_round_trips_through_the_sandbox_run() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let now = "2026-09-23T10:00:00Z";
        let leads = source_leads(
            parse_miur(MIUR_FIXTURE).unwrap(),
            &geography,
            "https://example.test/miur.csv",
            now,
        );
        assert_eq!(leads.len(), 2, "shared institute mailbox is deduplicated");
        let summary = insert_run(
            &db,
            &RunRecord {
                kind: "source",
                query_text: "Fonte: test",
                source_id: Some("miur-statali"),
                seed_count: 0,
                page_count: 0,
                pdf_found: 0,
                errors: &[],
                now,
            },
            leads,
        )
        .unwrap();
        let list = crate::discovery::list_search_leads_page(&db, &summary.run_id, 100, 0).unwrap();
        let institute = list.leads.iter().find(|lead| !lead.is_pec).unwrap();
        assert_eq!(
            institute.subcategory.as_deref(),
            Some("Istituto comprensivo")
        );
        assert_eq!(institute.category.as_deref(), Some("Istruzione"));
        assert_eq!(institute.comune.as_deref(), Some("Bergamo"));
        assert_eq!(institute.regione.as_deref(), Some("Lombardia"));
        assert_eq!(institute.macroarea.as_deref(), Some("Nord-Ovest"));
        assert_eq!(institute.official_code.as_deref(), Some("BGIC000001"));
        assert_eq!(
            (
                institute.email_domain.as_str(),
                institute.email_tld.as_str()
            ),
            ("example", ".test")
        );
        let source_id: String = db
            .query_row(
                "SELECT source_id FROM search_runs WHERE id=?1",
                [&summary.run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_id, "miur-statali");
        let contacts: i64 = db
            .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(contacts, 0);
    }

    #[test]
    fn lead_sources_locate_current_downloads() {
        let landing = r#"<a href="SCUANAGRAFESTAT20262720260901.csv">x</a><a href="SCUANAGRAFESTAT20252620250901.csv">y</a>"#;
        assert_eq!(
            latest_miur_file(landing, "SCUANAGRAFESTAT").as_deref(),
            Some("SCUANAGRAFESTAT20262720260901.csv")
        );
        assert_eq!(latest_miur_file(landing, "SCUANAGRAFEPAR"), None);
        let package = r#"{"result":{"resources":[{"url":"https://example.test/metadata.json"},{"url":"https://example.test/ou.txt"}]}}"#;
        assert_eq!(
            ckan_txt_resource(package).as_deref(),
            Some("https://example.test/ou.txt")
        );
    }

    #[test]
    fn lead_sources_catalog_ids_are_unique_and_importers_exist() {
        let mut ids: Vec<_> = CATALOG.iter().map(|source| source.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        let importable: Vec<_> = CATALOG
            .iter()
            .filter(|source| source.importable)
            .map(|source| source.id)
            .collect();
        assert!(importable.contains(&"iccu-biblioteche") && importable.contains(&"dir-cai"));
        assert!(
            CATALOG.iter().all(|source| source.importable),
            "ogni voce si importa o si esplora"
        );
        for source in CATALOG {
            match source.kind {
                "dataset" => assert!(
                    matches!(
                        source.id,
                        "miur-statali"
                            | "miur-paritarie"
                            | "indicepa"
                            | "ana-sezioni"
                            | "iccu-biblioteche"
                    ) || !ckan_bundle(source.id).1.is_empty()
                        || !osm_selectors(source.id).is_empty()
                        || !wikidata_classes(source.id).is_empty(),
                    "{} senza importatore",
                    source.id
                ),
                "directory" => {
                    assert!(source.category.is_some() && source.landing_url.starts_with("https://"))
                }
                kind => panic!("{}: tipo {kind} non esplorabile né importabile", source.id),
            }
        }
    }

    #[test]
    fn lead_sources_iccu_joins_contacts_and_addresses_by_isil() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<biblioteche>
  <biblioteca codice-isil="IT-BG0001" denominazione="Biblioteca civica &quot;Esempio&quot;">
    <contatto tipo="e-mail">biblioteca@comune.example.test</contatto>
    <contatto tipo="pec">biblioteca@pec.example.test</contatto>
    <contatto tipo="url">www.biblioteca.example.test</contatto>
    <contatto tipo="telefono">+39 035000000</contatto>
  </biblioteca>
  <biblioteca codice-isil="IT-BG0002" denominazione="Biblioteca del Seminario">
    <contatto tipo="url">https://seminario.example.test/biblioteca</contatto>
  </biblioteca>
  <biblioteca codice-isil="IT-BG0003" denominazione="Solo telefono">
    <contatto tipo="telefono">+39 035000001</contatto>
  </biblioteca>
</biblioteche>"#;
        let csv = "\u{feff}\"codice-isil\";\"codice-sbn\";\"denominazione\";\"indirizzo\";\"frazione\";\"cap\";\"comune\";\"codice istat comune\";\"provincia\";\"regione\";\"codice istat provincia\";\"latitudine\";\"longitudine\";\"telefono\";\"fax\";\"email\";\"url\"\n\"IT-BG0001\";\"\";\"Biblioteca civica\";\"Via Roma 1\";\"\";\"24100\";\"Bergamo\";\"016024\";\"Bergamo\";\"LOMBARDIA\";\"016\";\"\";\"\";\"\";\"\";\"\";\"\"\n";
        let data = parse_iccu(xml, csv).unwrap();
        assert_eq!(data.rows.len(), 2);
        let peo = data.rows.iter().find(|row| !row.is_pec).unwrap();
        assert_eq!(
            peo.organization.as_deref(),
            Some("Biblioteca civica \"Esempio\"")
        );
        assert_eq!(
            (peo.category, peo.subcategory),
            (Some("Cultura"), Some("Biblioteca"))
        );
        assert_eq!(peo.address.as_deref(), Some("Via Roma 1"));
        assert_eq!(peo.postal_code.as_deref(), Some("24100"));
        assert_eq!(peo.comune, "Bergamo");
        assert_eq!(peo.official_code.as_deref(), Some("IT-BG0001"));
        assert!(data.rows.iter().any(|row| row.is_pec));
        assert_eq!(data.seeds.len(), 1, "site without email becomes a seed");
        assert_eq!(
            data.seeds[0].url.as_deref(),
            Some("https://seminario.example.test/biblioteca")
        );
    }

    #[test]
    fn lead_sources_osm_maps_tags_to_taxonomy_leads_and_seeds() {
        let body = serde_json::json!({"elements": [
            {"type": "node", "id": 1, "tags": {"amenity": "place_of_worship", "religion": "christian",
                "name": "Parrocchia San Marco", "email": "info@parrocchia.example.test; don@parrocchia.example.test",
                "addr:street": "Via Roma", "addr:housenumber": "2", "addr:postcode": "24100", "addr:city": "Bergamo"}},
            {"type": "way", "id": 2, "tags": {"amenity": "place_of_worship", "religion": "christian",
                "name": "Santuario della Madonna", "website": "https://santuario.example.test/"}},
            {"type": "node", "id": 3, "tags": {"amenity": "library", "name": "Biblioteca", "contact:email": "bib@example.test"}},
            {"type": "node", "id": 4, "tags": {"shop": "bakery", "email": "pane@example.test"}},
            {"type": "node", "id": 5, "tags": {"club": "sport", "email": "calcio@example.test"}}
        ]});
        let data = parse_osm(&body);
        assert_eq!(
            data.rows.len(),
            4,
            "bakery skipped, two addresses in one tag"
        );
        let parish = data
            .rows
            .iter()
            .find(|row| row.email == "info@parrocchia.example.test")
            .unwrap();
        assert_eq!(parish.subcategory, Some("Parrocchia"));
        assert_eq!(parish.address.as_deref(), Some("Via Roma 2"));
        assert_eq!(parish.official_code.as_deref(), Some("osm:node/1"));
        assert_eq!(data.seeds.len(), 1);
        assert_eq!(data.seeds[0].subcategory, Some("Santuario / basilica"));
        assert!(
            data.rows
                .iter()
                .any(|row| row.subcategory == Some("Sportiva"))
        );
        assert!(
            overpass_query(osm_selectors("osm-religiosi"))
                .contains(r#"["amenity"="place_of_worship"]"#)
        );
    }

    #[test]
    fn lead_sources_wikidata_bindings_become_leads_or_seeds() {
        let classes = wikidata_classes("wikidata-cultura");
        let binding = |item: &str, class: &str, email: Option<&str>, site: Option<&str>| {
            let mut value = serde_json::json!({
                "item": {"value": format!("http://www.wikidata.org/entity/{item}")},
                "itemLabel": {"value": "Museo Esempio"},
                "type": {"value": format!("http://www.wikidata.org/entity/{class}")},
                "placeLabel": {"value": "Bergamo"}
            });
            if let Some(email) = email {
                value["email"] = serde_json::json!({"value": email});
            }
            if let Some(site) = site {
                value["site"] = serde_json::json!({"value": site});
            }
            value
        };
        let body = serde_json::json!({"results": {"bindings": [
            binding("Q1", "Q33506", Some("mailto:museo@example.test"), Some("https://museo.example.test/")),
            binding("Q1", "Q33506", Some("mailto:museo@example.test"), None),
            binding("Q2", "Q24354", None, Some("https://teatro.example.test/")),
            binding("Q3", "Q999", Some("mailto:altro@example.test"), None),
        ]}});
        let data = parse_wikidata(&body, classes);
        assert_eq!(data.rows.len(), 1);
        assert_eq!(data.rows[0].email, "museo@example.test");
        assert_eq!(data.rows[0].subcategory, Some("Museo"));
        assert_eq!(data.rows[0].official_code.as_deref(), Some("wikidata:Q1"));
        assert_eq!(data.seeds.len(), 1);
        assert_eq!(data.seeds[0].subcategory, Some("Teatro"));
        assert!(wikidata_query(classes).contains("wd:Q33506"));
    }

    #[test]
    fn lead_sources_cap_table_places_rows_the_source_leaves_unresolved() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let table = CapTable::parse(
            r#"[
                {"nome": "Firenze", "provincia": {"nome": "Firenze"}, "regione": {"nome": "Toscana"}, "cap": ["50122"]},
                {"nome": "Clusone", "provincia": {"nome": "Bergamo"}, "regione": {"nome": "Lombardia"}, "cap": ["24023", "24020"]},
                {"nome": "Gorno", "provincia": {"nome": "Bergamo"}, "regione": {"nome": "Lombardia"}, "cap": ["24020"]},
                {"nome": "Bolzano", "provincia": {"nome": "Bolzano/Bozen"}, "regione": {"nome": "Trentino-Alto Adige/Südtirol"}, "cap": ["39100"]}
            ]"#,
        )
        .unwrap();
        let row = |comune: &str, address: Option<&str>, cap: Option<&str>| SourceRow {
            email: "utente@example.test".to_owned(),
            comune: comune.to_owned(),
            address: address.map(str::to_owned),
            postal_code: cap.map(str::to_owned),
            ..SourceRow::default()
        };
        let mut rows = vec![
            // A district name: the CAP serves one municipality.
            row("Quartiere 1", Some("Via Roma 1"), Some("50122")),
            // A CAP shared by two municipalities of one province.
            row("", None, Some("24020")),
            // The CAP inside the address, Italian half of bilingual names.
            row("", Some("Piazza Walther 1, 39100"), None),
            // A known municipality is never replaced.
            row("Bergamo", None, Some("50122")),
            // Unknown CAP: unchanged.
            row("Frazione", None, Some("99999")),
        ];
        table.locate(&mut rows, &geography);
        let places: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.comune.as_str(),
                    row.provincia.as_str(),
                    row.regione.as_str(),
                )
            })
            .collect();
        assert_eq!(
            places,
            vec![
                ("Firenze", "Firenze", "Toscana"),
                ("", "Bergamo", "Lombardia"),
                ("Bolzano", "Bolzano", "Trentino Alto Adige"),
                ("Bergamo", "", ""),
                ("Frazione", "", ""),
            ]
        );
    }

    #[test]
    fn lead_sources_address_town_resolves_a_missing_or_unknown_municipality() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let classes = wikidata_classes("wikidata-cultura");
        let body = serde_json::json!({"results": {"bindings": [{
            "item": {"value": "http://www.wikidata.org/entity/Q1"},
            "itemLabel": {"value": "Museo Esempio"},
            "type": {"value": "http://www.wikidata.org/entity/Q33506"},
            "placeLabel": {"value": "Città Alta"},
            "email": {"value": "mailto:museo@example.test"},
            "address": {"value": "Piazza Vecchia 1, 24129 Bergamo"},
            "cap": {"value": "24129"}
        }]}});
        let data = parse_wikidata(&body, classes);
        assert_eq!(data.rows[0].postal_code.as_deref(), Some("24129"));
        let leads = source_leads(
            data.rows,
            &geography,
            "https://www.wikidata.org/",
            "2026-09-30T10:00:00Z",
        );
        let context = &leads[0].context;
        assert_eq!(
            (
                context.comune.as_deref(),
                context.provincia.as_deref(),
                context.regione.as_deref()
            ),
            (Some("Bergamo"), Some("Bergamo"), Some("Lombardia"))
        );
    }

    #[test]
    fn lead_sources_ckan_search_keeps_datasets_with_tables() {
        let body = serde_json::json!({"result": {"results": [
            {"name": "pro-loco", "title": "Albo pro loco", "organization": {"title": "Regione Esempio"},
             "resources": [{"url": "https://dati.example.test/proloco.csv", "format": "CSV", "name": "Elenco"},
                           {"url": "https://dati.example.test/proloco.pdf", "format": "PDF"}]},
            {"name": "solo-pdf", "title": "Solo PDF", "resources": [{"url": "https://dati.example.test/x.pdf", "format": "PDF"}]}
        ]}});
        let datasets = parse_ckan_search(&body);
        assert_eq!(datasets.len(), 1);
        assert_eq!(datasets[0].resources.len(), 1);
        assert_eq!(datasets[0].organization, "Regione Esempio");
    }

    #[test]
    fn lead_sources_generic_tables_detect_columns_and_split_leads_from_seeds() {
        let csv = "comune,denominazione,indirizzo sede operativa,Tel_1,e-mail_1,e-mail_2,PEC,Sito web ,Facebook\n\
BAGNO A RIPOLI,PRO LOCO ESEMPIO,Via Pertini 5,055 000,proloco@example.test,visit@example.test ,proloco@pec.example.test ,www.proloco.example.test ,https://www.facebook.com/x\n\
FIESOLE,PRO LOCO SENZA MAIL,Via Roma 1,,,,,www.fiesole.example.test,\n\
IMPRUNETA,PRO LOCO SOLO NOME,,,,,,,\n";
        let (headers, rows) =
            read_table(csv.as_bytes(), "https://dati.example.test/a.csv").unwrap();
        let columns = detect_dataset_columns(&headers, &rows);
        assert_eq!(columns.name, Some(1));
        assert_eq!(columns.comune, Some(0));
        assert_eq!(columns.website, Some(7));
        assert_eq!(columns.emails.len(), 3);
        let data = parse_open_table(&headers, &rows).unwrap();
        assert_eq!(data.rows.len(), 3);
        assert!(data.rows.iter().any(|row| row.is_pec));
        assert_eq!(
            data.rows[0].organization.as_deref(),
            Some("PRO LOCO ESEMPIO")
        );
        assert_eq!(data.rows[0].address.as_deref(), Some("Via Pertini 5"));
        assert_eq!(
            data.seeds.len(),
            2,
            "site-only and name-only rows become seeds"
        );
        assert_eq!(data.seeds[1].url, None);

        let semicolon = "\"DENOMINAZIONE\";\"PEC\";\"MAIL\";\"CAP\";\"COMUNE\"\n\"MORANO PO\";\"\";\"utente@example.test\";\"15025\";\"MORANO PO\"\n";
        let (headers, rows) =
            read_table(semicolon.as_bytes(), "https://x.example.test/all").unwrap();
        let data = parse_open_table(&headers, &rows).unwrap();
        assert_eq!(data.rows.len(), 1);
        assert_eq!(data.rows[0].postal_code.as_deref(), Some("15025"));
        let latin1 = b"Denominazione;Email\nAssociazione Citt\xe0;utente@example.test\n";
        let (_, rows) = read_table(latin1, "https://x.example.test/b.csv").unwrap();
        assert_eq!(rows[0][0], "Associazione Città");
    }

    #[test]
    fn lead_sources_ckan_tables_get_the_bundle_taxonomy_and_publisher_region() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let csv = "denominazione;email;sito\nPro Loco Esempio;utente@example.test;\nPro Loco Due;;www.due.example.test\n";
        let (headers, rows) = read_table(csv.as_bytes(), "https://x.example.test/a.csv").unwrap();
        let mut data = parse_open_table(&headers, &rows).unwrap();
        data.url = "https://x.example.test/a.csv".to_owned();
        data.errors = vec!["altra-tabella: non più nel catalogo".to_owned()];
        let (category, tables) = ckan_bundle("ckan-turismo");
        tag_ckan_table(&mut data, category, &tables[1]);
        let summary = store_source_import(
            &db,
            &geography,
            data,
            "Fonte: Pro Loco",
            Some("ckan-turismo"),
            "2026-09-29T10:00:00Z",
        )
        .unwrap();
        assert_eq!(summary.lead_count, 1);
        assert_eq!(summary.errors, ["altra-tabella: non più nel catalogo"]);
        let list = crate::discovery::list_search_leads_page(&db, &summary.run_id, 100, 0).unwrap();
        let lead = &list.leads[0];
        assert_eq!(
            (
                lead.category.as_deref(),
                lead.subcategory.as_deref(),
                lead.regione.as_deref()
            ),
            (
                Some("Turismo e promozione territoriale"),
                Some("Pro Loco"),
                Some("Piemonte")
            )
        );
        let (subcategory, regione): (String, String) = db
            .query_row(
                "SELECT subcategory, regione FROM search_seeds WHERE list_name='Fonte: Pro Loco'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (subcategory.as_str(), regione.as_str()),
            ("Pro Loco", "Piemonte")
        );
    }

    #[test]
    fn lead_sources_every_ckan_source_has_tables_in_the_taxonomy() {
        let sources: Vec<_> = CATALOG
            .iter()
            .filter(|s| s.id.starts_with("ckan-"))
            .collect();
        assert_eq!(sources.len(), 6);
        let mut names = std::collections::HashSet::new();
        for source in sources {
            let (category, tables) = ckan_bundle(source.id);
            let (_, subcategories) = crate::schema::TAXONOMY
                .iter()
                .find(|(name, _)| *name == category)
                .unwrap_or_else(|| panic!("{}: categoria {category}", source.id));
            assert!(!tables.is_empty());
            for table in tables {
                assert!(
                    subcategories.contains(&table.subcategory),
                    "{}: sottocategoria {}",
                    table.dataset,
                    table.subcategory
                );
                assert!(
                    table.regione.is_empty() || REGIONS.contains(&table.regione),
                    "{}: regione {}",
                    table.dataset,
                    table.regione
                );
                assert!(names.insert(table.dataset), "{} due volte", table.dataset);
            }
        }
    }

    fn ana_fixture() -> Vec<serde_json::Value> {
        serde_json::json!([
            {
                "title": {"rendered": "Bergamo"},
                "link": "https://www.ana.example.test/sezioni/bergamo/",
                "content": {"rendered": "<table><tbody>\
<tr><td><b>Indirizzo:</b></td><td>Via Roma, 1 &#8211; 24100 Bergamo</td></tr>\
<tr><td><b>Presidente:</b></td><td>Mario Rossi</td></tr>\
<tr><td><b>Web:</b></td><td><a href=\"https://www.anabergamo.example.test/\">www.anabergamo.example.test</a></td></tr>\
<tr><td><b>Email:</b></td><td><a href=\"mailto:bergamo@example.test\">bergamo@example.test</a></td></tr>\
</tbody></table><p>Elenco gruppi Sezione</p><table><tbody>\
<tr><td><strong>Gruppi</strong></td><td><strong>Informazioni</strong></td></tr>\
<tr><td>ALBINO</td><td>albino.bergamo@example.test</td></tr>\
<tr><td>CALDIERO</td><td><a href=\"mailto:cadidavid.bergamo@example.test\">caldiero.bergamo@example.test</a></td></tr>\
<tr><td>MAGLIANO</td><td><a href=\"mailto:ianoalfieri.bergamo@example.test\">maglianoalfieri.bergamo@example.test</a> <a href=\"mailto:x@hexample.test\">x@example.test</a></td></tr>\
<tr><td>BORGO D&#8217;INESISTENTE</td><td>borgo.bergamo@example.test</td></tr>\
<tr><td>CENTRO</td><td><table><tbody><tr><td><a href=\"mailto:centro@example.test\">centro@example.test</a></td></tr>\
<tr><td>Via Monte 2 &#8211; 24023 Clusone Tel. 0346000000</td></tr></tbody></table></td></tr>\
</tbody></table>"}
            },
            {
                "title": {"rendered": "Toronto"},
                "link": "https://www.ana.example.test/sezioni/toronto/",
                "content": {"rendered": "<table><tr><td><b>Indirizzo:</b></td><td>1 Main Street, Toronto</td></tr>\
<tr><td><b>Email:</b></td><td>toronto@example.test</td></tr></table>"}
            }
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    #[test]
    fn lead_sources_ana_maps_sections_groups_and_group_towns() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let rows = parse_ana(&ana_fixture(), &geography);
        let summary: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.email.as_str(),
                    row.department.as_deref(),
                    row.comune.as_str(),
                    row.regione.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("bergamo@example.test", None, "Bergamo", "Lombardia"),
                (
                    "albino.bergamo@example.test",
                    Some("Gruppo Albino"),
                    "Albino",
                    "Lombardia"
                ),
                (
                    "caldiero.bergamo@example.test",
                    Some("Gruppo Caldiero"),
                    "",
                    "Lombardia"
                ),
                (
                    "cadidavid.bergamo@example.test",
                    Some("Gruppo Caldiero"),
                    "",
                    "Lombardia"
                ),
                (
                    "maglianoalfieri.bergamo@example.test",
                    Some("Gruppo Magliano"),
                    "",
                    "Lombardia"
                ),
                ("x@example.test", Some("Gruppo Magliano"), "", "Lombardia"),
                (
                    "borgo.bergamo@example.test",
                    Some("Gruppo Borgo D’inesistente"),
                    "",
                    "Lombardia"
                ),
                (
                    "centro@example.test",
                    Some("Gruppo Centro"),
                    "Clusone",
                    "Lombardia"
                ),
                ("toronto@example.test", None, "", ""),
            ]
        );
        assert!(
            rows.iter().all(
                |row| row.category == Some("Associazioni") && row.subcategory == Some("Alpini")
            )
        );
        assert_eq!(rows[1].organization.as_deref(), Some("Sezione ANA Bergamo"));
        assert_eq!(
            rows[1].website.as_deref(),
            Some("https://www.anabergamo.example.test")
        );
        assert_eq!(
            rows[1].source_url.as_deref(),
            Some("https://www.ana.example.test/sezioni/bergamo/")
        );
        assert!(rows[0].primary && !rows[1].primary);
        // The section president is the referente of the section mailbox only.
        assert_eq!(
            (rows[0].person_name.as_deref(), rows[0].role.as_deref()),
            (Some("Mario Rossi"), Some("Presidente"))
        );
        assert_eq!(
            (rows[1].person_name.as_deref(), rows[1].role.as_deref()),
            (None, None)
        );
    }

    /// Fetches ana.it. Set `ANA_DUMP=/tmp/x.txt` to write the lead emails for
    /// an offline overlap check.
    #[tokio::test]
    #[ignore = "live ana.it fetch; run: cargo test --manifest-path src-tauri/Cargo.toml lead_sources_live_ana -- --ignored --nocapture"]
    async fn lead_sources_live_ana() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let client = polite_client(DOWNLOAD_TIMEOUT).unwrap();
        let items = fetch_ana(&client).await.unwrap();
        let rows = parse_ana(&items, &geography);
        let with_person = rows.iter().filter(|row| row.person_name.is_some()).count();
        let leads = source_leads(
            rows,
            &geography,
            "https://www.ana.it/",
            "2026-09-23T10:00:00Z",
        );
        let with_comune = leads
            .iter()
            .filter(|lead| lead.context.comune.is_some())
            .count();
        println!(
            "LIVE ana: sections={} leads={} with_comune={with_comune} with_person={with_person}",
            items.len(),
            leads.len()
        );
        if let Ok(path) = std::env::var("ANA_DUMP") {
            let keys: Vec<_> = leads
                .iter()
                .map(|lead| lead.comparison_key.as_str())
                .collect();
            std::fs::write(path, keys.join("\n")).unwrap();
        }
        assert!(items.len() > 100 && leads.len() > 3000);
    }

    /// Every pinned dati.gov.it table must still download and give leads.
    #[tokio::test]
    #[ignore = "live dati.gov.it download; run: cargo test --manifest-path src-tauri/Cargo.toml lead_sources_live_ckan_tables -- --ignored --nocapture"]
    async fn lead_sources_live_ckan_tables() {
        let client = polite_client(DOWNLOAD_TIMEOUT).unwrap();
        let mut failed = Vec::new();
        for source in CATALOG.iter().filter(|s| s.id.starts_with("ckan-")) {
            let (_, tables) = ckan_bundle(source.id);
            let names: Vec<&str> = tables.iter().map(|table| table.dataset).collect();
            let datasets = ckan_datasets(&client, &names).await.unwrap();
            for table in tables {
                let Some(dataset) = datasets.iter().find(|d| d.id == table.dataset) else {
                    failed.push(format!("{}: non più nel catalogo", table.dataset));
                    continue;
                };
                match fetch_ckan_table(&client, dataset).await {
                    Ok(data) => {
                        println!(
                            "LIVE {} {}: leads={} seeds={}",
                            source.id,
                            table.dataset,
                            data.rows.len(),
                            data.seeds.len()
                        );
                        if data.rows.is_empty() {
                            failed.push(format!("{}: nessuna email", table.dataset));
                        }
                    }
                    Err(error) => failed.push(format!("{}: {error}", table.dataset)),
                }
            }
        }
        assert!(failed.is_empty(), "tabelle da sistemare: {failed:#?}");
    }

    /// Downloads the real registries and prints counts only.
    #[tokio::test]
    #[ignore = "live open-data download; run: cargo test --manifest-path src-tauri/Cargo.toml lead_sources_live_import -- --ignored --nocapture"]
    async fn lead_sources_live_import() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        let client = polite_client(DOWNLOAD_TIMEOUT).unwrap();
        let only = std::env::var("LEAD_SOURCE").ok();
        for source in CATALOG
            .iter()
            .filter(|source| source.kind == "dataset")
            .filter(|source| only.as_deref().is_none_or(|id| id == source.id))
        {
            let started = std::time::Instant::now();
            let mut data = fetch_source_data(&client, source, &geography)
                .await
                .expect("source downloads");
            locate_by_cap(&client, &mut data.rows, &geography).await;
            let seeds = source_seeds(data.seeds, &geography).len();
            let (rows, url) = (data.rows, data.url);
            let raw = rows.len();
            let leads = source_leads(rows, &geography, &url, "2026-09-23T10:00:00Z");
            let resolved = leads
                .iter()
                .filter(|lead| lead.context.regione.is_some())
                .count();
            let municipalities = leads
                .iter()
                .filter(|lead| {
                    lead.context
                        .comune
                        .as_deref()
                        .is_some_and(|name| geography.is_municipality(name))
                })
                .count();
            println!(
                "LIVE {}: rows={raw} leads={} with_regione={resolved} ({:.0}%) with_comune={municipalities} elapsed={:.1}s",
                source.id,
                leads.len(),
                100.0 * resolved as f64 / leads.len().max(1) as f64,
                started.elapsed().as_secs_f64()
            );
            println!("LIVE {}: seeds={seeds}", source.id);
            assert!(leads.len() + seeds > 300);
            let unknown: std::collections::BTreeSet<_> = leads
                .iter()
                .filter_map(|lead| lead.context.regione.as_deref())
                .filter(|region| !REGIONS.contains(region))
                .collect();
            assert!(unknown.is_empty(), "regioni fuori vocabolario: {unknown:?}");
        }
    }
}
