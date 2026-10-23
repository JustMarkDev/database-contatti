use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;

pub(crate) const ACTIVE_VERSION: &str = "geonames-2026-07-25";
const CATALOG: &str = include_str!("../resources/gazetteer/geonames-2026-07-25.tsv");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub id: i64,
    pub country: String,
    pub level: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub parents: Vec<(String, String)>,
    pub parent_ids: Vec<i64>,
}

pub(crate) fn norm(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            '’' | '‘' | '´' | '`' | '\'' | '-' | '/' | '.' | ',' | '_' => ' ',
            _ => c,
        })
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

use std::sync::LazyLock;

fn clean_canton_name(name: &str) -> &str {
    let mut s = name.trim();
    for prefix in [
        "Cantone di ",
        "Cantone ",
        "Canton de ",
        "Canton du ",
        "Canton d'",
        "Canton of ",
        "Canton ",
        "Kanton ",
        "Chantun ",
        "Repubblica e Cantone ",
        "République et Canton de ",
        "Republique et Canton de ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    s
}

fn clean_province_name(name: &str) -> &str {
    let mut s = name;
    for prefix in [
        "Province of ",
        "Province de ",
        "Province d'",
        "Province o ",
        "Provincia di ",
        "Provincia d'",
        "Provincia del ",
        "Provincia de ",
        "Provincia dell'",
        "Città metropolitana di ",
        "Citta' metropolitana di ",
        "Città Metropolitana di ",
        "Citta Metropolitana di ",
        "Libero consorzio comunale di ",
        "Libero Consorzio Comunale di ",
        "Autonomous Province of ",
        "Autonome Provinz ",
        "Provinz ",
        "Provincie ",
        "Provincia autonoma di ",
        "Provincia ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
            break;
        }
    }
    for (prov, _) in crate::parsing::PROVINCE_REGIONS {
        if norm(s) == norm(prov) || norm(name) == norm(prov) {
            return prov;
        }
    }
    // GeoNames names a few provinces in English or by their old form.
    match norm(s).as_str() {
        "florence" => "Firenze",
        "roma capitale" => "Roma",
        "aquila" => "L'Aquila",
        "monza e della brianza" => "Monza e Brianza",
        "valle d aosta" => "Aosta",
        _ => s,
    }
}

fn clean_region_name(name: &str) -> &str {
    let s = name
        .strip_prefix("Regione Autonoma ")
        .or_else(|| name.strip_prefix("Regione "))
        .unwrap_or(name);
    for (_, reg) in crate::parsing::PROVINCE_REGIONS {
        if norm(s) == norm(reg) {
            return reg;
        }
    }
    s
}

fn display_name(name: &str) -> String {
    match norm(name).as_str() {
        "reggio nell emilia" => "Reggio Emilia".to_owned(),
        _ => name.to_owned(),
    }
}

pub(crate) fn place_display_name(level: &str, name: &str) -> String {
    let cleaned = match level {
        "province" => clean_province_name(name),
        "region" => clean_region_name(name),
        _ => name,
    };
    display_name(cleaned)
}

pub(crate) const EXTRA_PROVINCES: &[(&str, &str)] = &[
    ("roma capitale", "Roma"),
    ("og", "Sud Sardegna"),
    ("ogliastra", "Sud Sardegna"),
    ("ot", "Sassari"),
    ("olbia tempio", "Sassari"),
    ("olbia-tempio", "Sassari"),
    ("olbia", "Sassari"),
    ("tempio", "Sassari"),
    ("vs", "Sud Sardegna"),
    ("medio campidano", "Sud Sardegna"),
    ("ci", "Sud Sardegna"),
    ("carbonia", "Sud Sardegna"),
    ("iglesias", "Sud Sardegna"),
    ("carbonia iglesias", "Sud Sardegna"),
    ("carbonia-iglesias", "Sud Sardegna"),
    ("su", "Sud Sardegna"),
    ("sud sardegna", "Sud Sardegna"),
    ("fc", "Forlì-Cesena"),
    ("fo", "Forlì-Cesena"),
    ("forli", "Forlì-Cesena"),
    ("forlì", "Forlì-Cesena"),
    ("forli'", "Forlì-Cesena"),
    ("forli cesena", "Forlì-Cesena"),
    ("forlì cesena", "Forlì-Cesena"),
    ("forli' cesena", "Forlì-Cesena"),
    ("forli-cesena", "Forlì-Cesena"),
    ("forlì-cesena", "Forlì-Cesena"),
    ("cesena", "Forlì-Cesena"),
    ("ms", "Massa-Carrara"),
    ("massa", "Massa-Carrara"),
    ("carrara", "Massa-Carrara"),
    ("massa carrara", "Massa-Carrara"),
    ("massa-carrara", "Massa-Carrara"),
    ("massa e carrara", "Massa-Carrara"),
    ("pu", "Pesaro e Urbino"),
    ("ps", "Pesaro e Urbino"),
    ("pesaro", "Pesaro e Urbino"),
    ("urbino", "Pesaro e Urbino"),
    ("pesaro urbino", "Pesaro e Urbino"),
    ("pesaro-urbino", "Pesaro e Urbino"),
    ("pesaro e urbino", "Pesaro e Urbino"),
    ("bt", "Barletta-Andria-Trani"),
    ("barletta", "Barletta-Andria-Trani"),
    ("andria", "Barletta-Andria-Trani"),
    ("trani", "Barletta-Andria-Trani"),
    ("barletta andria trani", "Barletta-Andria-Trani"),
    ("barletta-andria-trani", "Barletta-Andria-Trani"),
    ("barletta andr5", "Barletta-Andria-Trani"),
    ("mb", "Monza e Brianza"),
    ("monza", "Monza e Brianza"),
    ("brianza", "Monza e Brianza"),
    ("monza brianza", "Monza e Brianza"),
    ("monza-brianza", "Monza e Brianza"),
    ("monza e della brianza", "Monza e Brianza"),
    ("monza e brianza", "Monza e Brianza"),
    ("fm", "Fermo"),
    ("fermo", "Fermo"),
    ("rm", "Roma"),
    ("roma", "Roma"),
    ("vb", "Verbano-Cusio-Ossola"),
    ("verbania", "Verbano-Cusio-Ossola"),
    ("verbano", "Verbano-Cusio-Ossola"),
    ("cusio", "Verbano-Cusio-Ossola"),
    ("ossola", "Verbano-Cusio-Ossola"),
    ("verbano cusio ossola", "Verbano-Cusio-Ossola"),
    ("verbano-cusio-ossola", "Verbano-Cusio-Ossola"),
    ("re", "Reggio Emilia"),
    ("reggio emilia", "Reggio Emilia"),
    ("reggio nell emilia", "Reggio Emilia"),
    ("reggio nell'emilia", "Reggio Emilia"),
    ("reggio nell’emilia", "Reggio Emilia"),
    ("rc", "Reggio Calabria"),
    ("reggio calabria", "Reggio Calabria"),
    ("reggio di calabria", "Reggio Calabria"),
    ("aq", "L'Aquila"),
    ("aquila", "L'Aquila"),
    ("l'aquila", "L'Aquila"),
    ("l’aquila", "L'Aquila"),
    ("bz", "Bolzano"),
    ("bozen", "Bolzano"),
    ("bolzano", "Bolzano"),
    ("alto adige", "Bolzano"),
    ("ao", "Aosta"),
    ("aosta", "Aosta"),
    ("sp", "La Spezia"),
    ("spezia", "La Spezia"),
    ("la spezia", "La Spezia"),
    ("ap", "Ascoli Piceno"),
    ("ascoli", "Ascoli Piceno"),
    ("ascoli piceno", "Ascoli Piceno"),
    ("vv", "Vibo Valentia"),
    ("vibo", "Vibo Valentia"),
    ("vibo valentia", "Vibo Valentia"),
];

pub(crate) static GAZETTEER_INDEX: LazyLock<GazetteerIndex> = LazyLock::new(|| {
    let mut municipalities: HashMap<String, &'static str> = HashMap::with_capacity(16000);
    let mut municipality_names: HashMap<String, &'static str> = HashMap::with_capacity(8000);
    let mut municipality_locations: HashMap<String, Vec<(&'static str, &'static str)>> =
        HashMap::with_capacity(16000);
    let mut records: HashMap<
        &'static str,
        (&'static str, &'static str, &'static str, &'static str),
    > = HashMap::with_capacity(20000);
    let mut provinces: HashMap<String, &'static str> = HashMap::with_capacity(500);
    let mut regions: HashMap<String, &'static str> = HashMap::with_capacity(100);
    let mut cantons: HashMap<String, &'static str> = HashMap::with_capacity(64);

    for line in CATALOG.lines().skip(1) {
        let f: Vec<&'static str> = line.split('\t').collect();
        if f.len() < 4 {
            continue;
        }
        let level = f[2];
        let raw_canonical_name = f[3];
        let aliases_str = f.get(5).copied().unwrap_or_default();
        records.insert(
            f[0],
            (
                level,
                raw_canonical_name,
                f.get(4).copied().unwrap_or_default(),
                aliases_str,
            ),
        );

        match level {
            "municipality" => {
                let canonical_name = raw_canonical_name
                    .split('/')
                    .next()
                    .unwrap_or(raw_canonical_name);
                municipalities.insert(norm(raw_canonical_name), canonical_name);
                municipalities.insert(norm(canonical_name), canonical_name);
                municipality_names.insert(norm(raw_canonical_name), canonical_name);
                municipality_names.insert(norm(canonical_name), canonical_name);
                for part in raw_canonical_name.split('/') {
                    municipalities.insert(norm(part), canonical_name);
                    municipality_names.insert(norm(part), canonical_name);
                }
                for alias in aliases_str.split('|').filter(|a| !a.is_empty()) {
                    let n = norm(alias);
                    if n.len() >= 2 && !n.chars().all(|c| c.is_ascii_digit()) {
                        municipalities.insert(n, canonical_name);
                    }
                }
            }
            "province" => {
                let canonical_name = clean_province_name(raw_canonical_name);
                provinces.insert(norm(raw_canonical_name), canonical_name);
                provinces.insert(norm(canonical_name), canonical_name);
                for alias in aliases_str.split('|').filter(|a| !a.is_empty()) {
                    let n = norm(alias);
                    if n.len() >= 2 && !n.chars().all(|c| c.is_ascii_digit()) {
                        provinces.insert(n, canonical_name);
                    }
                }
            }
            "region" => {
                let canonical_name = clean_region_name(raw_canonical_name);
                regions.insert(norm(raw_canonical_name), canonical_name);
                regions.insert(norm(canonical_name), canonical_name);
                for alias in aliases_str.split('|').filter(|a| !a.is_empty()) {
                    let n = norm(alias);
                    if n.len() >= 2 && !n.chars().all(|c| c.is_ascii_digit()) {
                        regions.insert(n, canonical_name);
                    }
                }
            }
            "canton" => {
                let canonical_name = clean_canton_name(raw_canonical_name);
                cantons.insert(norm(raw_canonical_name), canonical_name);
                cantons.insert(norm(canonical_name), canonical_name);
                for alias in aliases_str.split('|').filter(|a| !a.is_empty()) {
                    let n = norm(alias);
                    if n.len() >= 2 && !n.chars().all(|c| c.is_ascii_digit()) {
                        cantons.insert(n, canonical_name);
                    }
                }
            }
            _ => {}
        }
    }

    let extra_regions = [
        ("valle daosta", "Valle d'Aosta"),
        ("valle d aosta", "Valle d'Aosta"),
        ("valle d'aosta", "Valle d'Aosta"),
        ("valle d’aosta", "Valle d'Aosta"),
        ("vda", "Valle d'Aosta"),
        ("val d'aoste", "Valle d'Aosta"),
        ("val d aoste", "Valle d'Aosta"),
        ("vallee d aoste", "Valle d'Aosta"),
        ("vallée d'aoste", "Valle d'Aosta"),
        ("aosta", "Valle d'Aosta"),
        ("abruzzi", "Abruzzo"),
        ("abruzzo", "Abruzzo"),
        ("friuli venezia giulia", "Friuli Venezia Giulia"),
        ("friuli venezia g", "Friuli Venezia Giulia"),
        ("friuli venezia g.", "Friuli Venezia Giulia"),
        ("friuli vg", "Friuli Venezia Giulia"),
        ("friuli v.g.", "Friuli Venezia Giulia"),
        ("friuli v. g.", "Friuli Venezia Giulia"),
        ("fvg", "Friuli Venezia Giulia"),
        ("friuli", "Friuli Venezia Giulia"),
        ("trentino alto adige", "Trentino Alto Adige"),
        ("trentino-alto adige", "Trentino Alto Adige"),
        ("trentino alto adige sudtirol", "Trentino Alto Adige"),
        ("trentino alto adige südtirol", "Trentino Alto Adige"),
        ("trentino", "Trentino Alto Adige"),
        ("alto adige", "Trentino Alto Adige"),
        ("sudtirol", "Trentino Alto Adige"),
        ("südtirol", "Trentino Alto Adige"),
        ("emilia romagna", "Emilia Romagna"),
        ("emilia-romagna", "Emilia Romagna"),
        ("puglie", "Puglia"),
        ("lucania", "Basilicata"),
    ];
    for (k, v) in extra_regions {
        regions.insert(norm(k), v);
    }

    for &(k, v) in EXTRA_PROVINCES {
        provinces.insert(norm(k), v);
    }

    let extra_municipalities = [
        ("alluvioni cambio", "Alluvioni Piovera"),
        ("alluvioni cambio'", "Alluvioni Piovera"),
        ("alluvioni", "Alluvioni Piovera"),
        ("piovera", "Alluvioni Piovera"),
        ("abetone", "Abetone Cutigliano"),
        ("cutigliano", "Abetone Cutigliano"),
        ("acquarica del capo", "Presicce-Acquarica"),
        ("presicce", "Presicce-Acquarica"),
        ("alano di piave", "Setteville"),
        ("quero vas", "Setteville"),
        ("quero", "Setteville"),
        ("vas", "Setteville"),
        ("albaredo arnaboldi", "Campospinoso Albaredo"),
        ("campospinoso", "Campospinoso Albaredo"),
        ("albisola marina", "Albissola Marina"),
        ("agliano", "Agliano Terme"),
        ("alo dei sardi", "Alà dei Sardi"),
        ("ala dei sardi", "Alà dei Sardi"),
        ("arzene", "Valvasone Arzene"),
        ("valvasone", "Valvasone Arzene"),
        ("auditore", "Sassocorvaro Auditore"),
        ("sassocorvaro", "Sassocorvaro Auditore"),
        ("barbarano vicentino", "Barbarano Mossano"),
        ("mossano", "Barbarano Mossano"),
        ("barberino val d elsa", "Barberino Tavarnelle"),
        ("barberino val d'elsa", "Barberino Tavarnelle"),
        ("barberino di val d elsa", "Barberino Tavarnelle"),
        ("tavarnelle val di pesa", "Barberino Tavarnelle"),
        ("barchi", "Terre Roveresche"),
        ("orciano di pesaro", "Terre Roveresche"),
        ("piagge", "Terre Roveresche"),
        ("san giorgio di pesaro", "Terre Roveresche"),
        ("bardello", "Bardello con Malgesso e Bregano"),
        ("bregano", "Bardello con Malgesso e Bregano"),
        ("malgesso", "Bardello con Malgesso e Bregano"),
        ("bazzano", "Valsamoggia"),
        ("castello di serravalle", "Valsamoggia"),
        ("crespellano", "Valsamoggia"),
        ("monteveglio", "Valsamoggia"),
        ("savigno", "Valsamoggia"),
        ("bigarello", "San Giorgio Bigarello"),
        ("bolbeno", "Sella Giudicarie"),
        ("bondo", "Sella Giudicarie"),
        ("breguzzo", "Sella Giudicarie"),
        ("lardaro", "Sella Giudicarie"),
        ("roncone", "Sella Giudicarie"),
        ("borgoforte", "Borgo Virgilio"),
        ("virgilio", "Borgo Virgilio"),
        ("brembilla", "Val Brembilla"),
        ("gerosa", "Val Brembilla"),
        ("brez", "Novella"),
        ("cagno", "Novella"),
        ("cloz", "Novella"),
        ("revo", "Novella"),
        ("romallo", "Novella"),
        ("buia", "Buja"),
        ("busana", "Ventasso"),
        ("collagna", "Ventasso"),
        ("ligonchio", "Ventasso"),
        ("ramiseto", "Ventasso"),
        ("cadrezzate", "Cadrezzate con Osmate"),
        ("osmate", "Cadrezzate con Osmate"),
        ("solbiate", "Solbiate con Cagno"),
        ("calatafimi", "Calatafimi-Segesta"),
        ("calavino", "Madruzzo"),
        ("lasino", "Madruzzo"),
        ("padergnone", "Madruzzo"),
        ("campolongo sul brenta", "Valbrenta"),
        ("cismon del grappa", "Valbrenta"),
        ("san nazario", "Valbrenta"),
        ("valstagna", "Valbrenta"),
        ("carano", "Ville di Fiemme"),
        ("daiano", "Ville di Fiemme"),
        ("varena", "Ville di Fiemme"),
        ("carbonara di po", "Borgocarbonara"),
        ("borgofranco sul po", "Borgocarbonara"),
        ("casciana terme", "Casciana Terme Lari"),
        ("lari", "Casciana Terme Lari"),
        ("crespina", "Crespina Lorenzana"),
        ("lorenzana", "Crespina Lorenzana"),
        ("casole bruzio", "Casali del Manco"),
        ("pedace", "Casali del Manco"),
        ("serra pedace", "Casali del Manco"),
        ("spezzano piccolo", "Casali del Manco"),
        ("trenta", "Casali del Manco"),
        ("castel colonna", "Trecastelli"),
        ("monterado", "Trecastelli"),
        ("ripe", "Trecastelli"),
        ("castelfondo", "Borgo d'Anaunia"),
        ("fondo", "Borgo d'Anaunia"),
        ("malosco", "Borgo d'Anaunia"),
        ("castelfranco di sopra", "Castelfranco Piandiscò"),
        ("pian di sco", "Castelfranco Piandiscò"),
        ("pian di sco'", "Castelfranco Piandiscò"),
        ("castellavazzo", "Longarone"),
        ("castelnuovo val di cecina", "Castelnuovo di Val di Cecina"),
        ("cavacurta", "Castelgerundo"),
        ("camairago", "Castelgerundo"),
        ("casasco d intelvi", "Centro Valle Intelvi"),
        ("castiglione d intelvi", "Centro Valle Intelvi"),
        ("san fedele intelvi", "Centro Valle Intelvi"),
        ("montemaggiore al metauro", "Colli al Metauro"),
        ("saltara", "Colli al Metauro"),
        ("serrungarina", "Colli al Metauro"),
        ("corigliano calabro", "Corigliano-Rossano"),
        ("rossano", "Corigliano-Rossano"),
        ("corteolona", "Corteolona e Genzone"),
        ("genzone", "Corteolona e Genzone"),
        ("duino aurisina", "Duino Aurisina"),
        ("fabbriche di vallico", "Fabbriche di Vergemoli"),
        ("vergemoli", "Fabbriche di Vergemoli"),
        ("figline valdarno", "Figline e Incisa Valdarno"),
        ("incisa in val d arno", "Figline e Incisa Valdarno"),
        ("incisa in val d'arno", "Figline e Incisa Valdarno"),
        ("massa fiscaglia", "Fiscaglia"),
        ("migliaro", "Fiscaglia"),
        ("migliarino", "Fiscaglia"),
        ("gravedona", "Gravedona ed Uniti"),
        ("consiglio di rumo", "Gravedona ed Uniti"),
        ("germasino", "Gravedona ed Uniti"),
        ("laterina", "Laterina Pergine Valdarno"),
        ("pergine valdarno", "Laterina Pergine Valdarno"),
        ("lusiana", "Lusiana Conco"),
        ("conco", "Lusiana Conco"),
        ("maccagno", "Maccagno con Pino e Veddasca"),
        (
            "pino sulla sponda del lago maggiore",
            "Maccagno con Pino e Veddasca",
        ),
        ("veddasca", "Maccagno con Pino e Veddasca"),
        ("montescudo", "Montescudo-Monte Colombo"),
        ("monte colombo", "Montescudo-Monte Colombo"),
        ("pieve di bono", "Pieve di Bono-Prezzo"),
        ("prezzo", "Pieve di Bono-Prezzo"),
        ("pieve di coriano", "Borgo Mantovano"),
        ("revere", "Borgo Mantovano"),
        ("villa poma", "Borgo Mantovano"),
        ("poggio berni", "Poggio Torriana"),
        ("torriana", "Poggio Torriana"),
        ("polesine parmense", "Polesine Zibello"),
        ("zibello", "Polesine Zibello"),
        ("berra", "Riva del Po"),
        ("ro", "Riva del Po"),
        ("san marcello pistoiese", "San Marcello Piteglio"),
        ("piteglio", "San Marcello Piteglio"),
        ("scarperia", "Scarperia e San Piero"),
        ("san piero a sieve", "Scarperia e San Piero"),
        ("sillano", "Sillano Giuncugnano"),
        ("giuncugnano", "Sillano Giuncugnano"),
        ("nave san rocco", "Terre d'Adige"),
        ("zambana", "Terre d'Adige"),
        ("mirabello", "Terre del Reno"),
        ("sant agostino", "Terre del Reno"),
        ("sant'agostino", "Terre del Reno"),
        ("lenno", "Tremezzina"),
        ("mezzegra", "Tremezzina"),
        ("ossuccio", "Tremezzina"),
        ("tremezzo", "Tremezzina"),
        ("forno di zoldo", "Val di Zoldo"),
        ("zoldo alto", "Val di Zoldo"),
        ("colbordolo", "Vallefoglia"),
        ("sant angelo in lizzola", "Vallefoglia"),
        ("sant'angelo in lizzola", "Vallefoglia"),
        ("budduso", "Buddusò"),
        ("muggia", "Muggia"),
        ("san dorligo della valle dolina", "San Dorligo della Valle"),
        ("san dorligo della valle", "San Dorligo della Valle"),
        ("nibbiano", "Alta Val Tidone"),
        ("pecorara", "Alta Val Tidone"),
        ("caminata", "Alta Val Tidone"),
        ("granaglione", "Alto Reno Terme"),
        ("porretta terme", "Alto Reno Terme"),
        ("vigolo vattaro", "Altopiano della Vigolana"),
        ("bosentino", "Altopiano della Vigolana"),
        ("centa san nicolo", "Altopiano della Vigolana"),
        ("centa san nicolo'", "Altopiano della Vigolana"),
        ("vattaro", "Altopiano della Vigolana"),
        ("santo stino di livenza", "San Stino di Livenza"),
        ("seppiana", "Borgomezzavalle"),
        ("viganella", "Borgomezzavalle"),
        ("fiumicello", "Fiumicello Villa Vicentina"),
        ("villa vicentina", "Fiumicello Villa Vicentina"),
        ("nughedu di san nicolo", "Nughedu San Nicolò"),
        ("nughedu di san nicolo'", "Nughedu San Nicolò"),
        ("san giovanni d asso", "Montalcino"),
        ("san giovanni d'asso", "Montalcino"),
        ("civenna", "Bellagio"),
        ("montoro inferiore", "Montoro"),
        ("montoro superiore", "Montoro"),
        ("santa margherita d adige", "Borgo Veneto"),
        ("megliadino san fidenzio", "Borgo Veneto"),
        ("saletto", "Borgo Veneto"),
        ("sant omobono imagna", "Sant'Omobono Terme"),
        ("sant'omobono imagna", "Sant'Omobono Terme"),
        ("mosso", "Valdilana"),
        ("soprana", "Valdilana"),
        ("trivero", "Valdilana"),
        ("valle mosso", "Valdilana"),
        ("ronago", "Uggiate con Ronago"),
        ("uggiate trevano", "Uggiate con Ronago"),
        ("daone", "Valdaone"),
        ("bersone", "Valdaone"),
        ("praso", "Valdaone"),
        ("zuclo", "Tre Ville"),
        ("montagne", "Tre Ville"),
        ("preore", "Tre Ville"),
        ("crosa", "Lessona"),
        ("verres", "Verrès"),
        ("dimaro", "Dimaro Folgarida"),
        ("monclassico", "Dimaro Folgarida"),
        ("piadena", "Piadena Drizzona"),
        ("drizzona", "Piadena Drizzona"),
        ("pozza di fassa", "San Giovanni di Fassa"),
        ("vigo di fassa", "San Giovanni di Fassa"),
        ("flavon", "Contà"),
        ("cunevo", "Contà"),
        ("terres", "Contà"),
        ("cavaglio spoccia", "Valle Cannobina"),
        ("cursolo orasso", "Valle Cannobina"),
        ("falmenta", "Valle Cannobina"),
        ("ruino", "Colli Verdi"),
        ("canevino", "Colli Verdi"),
        ("valverde", "Colli Verdi"),
        ("felonica", "Sermide e Felonica"),
        ("sermide", "Sermide e Felonica"),
        ("carceri", "Santa Caterina d'Este"),
        ("vighizzolo d este", "Santa Caterina d'Este"),
        ("reana del roiale", "Reana del Rojale"),
        ("strigno", "Castel Ivano"),
        ("spera", "Castel Ivano"),
        ("villa agnedo", "Castel Ivano"),
        ("pratovecchio", "Pratovecchio Stia"),
        ("stia", "Pratovecchio Stia"),
        ("perego", "La Valletta Brianza"),
        ("rovagnate", "La Valletta Brianza"),
        ("lanzo d intelvi", "Alta Valle Intelvi"),
        ("lanzo d'intelvi", "Alta Valle Intelvi"),
        ("pellio intelvi", "Alta Valle Intelvi"),
        ("ramponio verna", "Alta Valle Intelvi"),
        ("cavallasca", "San Fermo della Battaglia"),
        ("crespano del grappa", "Pieve del Grappa"),
        ("paderno del grappa", "Pieve del Grappa"),
        ("cellio", "Cellio con Breia"),
        ("breia", "Cellio con Breia"),
        ("adorno micca", "Andorno Micca"),
        ("formignana", "Tresignana"),
        ("tresigallo", "Tresignana"),
        ("sorbolo", "Sorbolo Mezzani"),
        ("mezzani", "Sorbolo Mezzani"),
        ("pievebovigliana", "Valfornace"),
        ("fiordimonte", "Valfornace"),
        ("gattico", "Gattico-Veruno"),
        ("veruno", "Gattico-Veruno"),
        ("lu", "Lu e Cuccaro Monferrato"),
        ("cuccaro monferrato", "Lu e Cuccaro Monferrato"),
    ];
    for (k, v) in extra_municipalities {
        municipalities.insert(norm(k), v);
        // Curated pre-merger names are official history, unlike GeoNames alternates.
        municipality_names.insert(norm(k), v);
    }
    for (external_id, (level, raw_name, parent_id, _)) in &records {
        if *level != "municipality" {
            continue;
        }
        let mut cursor = *parent_id;
        let mut province = None;
        while !cursor.is_empty() {
            let Some((parent_level, parent_name, next_parent, _)) = records.get(cursor) else {
                break;
            };
            if *parent_level == "province" {
                province = Some(clean_province_name(parent_name));
                break;
            }
            cursor = *next_parent;
        }
        let Some(province) = province else {
            continue;
        };
        let canonical = raw_name.split('/').next().unwrap_or(raw_name);
        let aliases = records
            .get(external_id)
            .map(|(_, _, _, aliases)| *aliases)
            .unwrap_or_default();
        for name in std::iter::once(*raw_name)
            .chain(std::iter::once(canonical))
            .chain(raw_name.split('/'))
            .chain(aliases.split('|'))
        {
            let key = norm(name);
            if key.is_empty() {
                continue;
            }
            let locations = municipality_locations.entry(key).or_default();
            if !locations.iter().any(|(existing, existing_province)| {
                *existing == canonical && *existing_province == province
            }) {
                locations.push((canonical, province));
            }
        }
    }

    let aliases_to_link = municipalities
        .iter()
        .filter_map(|(alias, canonical)| {
            municipality_locations
                .get(&norm(canonical))
                .cloned()
                .map(|locations| (alias.clone(), locations))
        })
        .collect::<Vec<_>>();
    for (alias, locations) in aliases_to_link {
        municipality_locations.entry(alias).or_insert(locations);
    }

    for (k, v) in [
        ("ticino", "Ticino"),
        ("cantone ticino", "Ticino"),
        ("tessin", "Ticino"),
        ("grigioni", "Graubünden"),
        ("grisons", "Graubünden"),
        ("graubunden", "Graubünden"),
        ("graubuenden", "Graubünden"),
    ] {
        cantons.insert(norm(k), v);
    }

    GazetteerIndex {
        municipalities,
        municipality_names,
        municipality_locations,
        provinces,
        regions,
        cantons,
    }
});

pub(crate) struct GazetteerIndex {
    /// Every known spelling, including GeoNames alternates (`Com` → Como) and
    /// pre-merger names (`Virgilio` → Borgo Virgilio). Safe for an explicit Comune field.
    pub municipalities: HashMap<String, &'static str>,
    /// Official names only. Free text (email, denomination) must use this map.
    pub municipality_names: HashMap<String, &'static str>,
    pub municipality_locations: HashMap<String, Vec<(&'static str, &'static str)>>,
    pub provinces: HashMap<String, &'static str>,
    pub regions: HashMap<String, &'static str>,
    pub cantons: HashMap<String, &'static str>,
}

pub(crate) fn seed(db: &Connection) -> Result<(), String> {
    db.execute(
        "UPDATE gazetteer_versions SET status='retired' WHERE semantic_version<>?1",
        [ACTIVE_VERSION],
    )
    .map_err(|e| e.to_string())?;
    db.execute(
        "INSERT OR IGNORE INTO gazetteer_versions(semantic_version,status) VALUES(?1,'active')",
        [ACTIVE_VERSION],
    )
    .map_err(|e| e.to_string())?;
    db.execute(
        "UPDATE gazetteer_versions SET status='active' WHERE semantic_version=?1",
        [ACTIVE_VERSION],
    )
    .map_err(|e| e.to_string())?;
    let version: i64 = db
        .query_row(
            "SELECT id FROM gazetteer_versions WHERE semantic_version=?1",
            [ACTIVE_VERSION],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut ids = HashMap::new();
    for line in CATALOG.lines().skip(1) {
        let f: Vec<_> = line.split('\t').collect();
        if f.len() < 4 {
            return Err("invalid bundled gazetteer row".into());
        }
        db.execute("INSERT OR IGNORE INTO gazetteer_places(gazetteer_version_id,external_id,country_code,level,canonical_name,normalized_name) VALUES(?1,?2,?3,?4,?5,?6)",params![version,f[0],f[1],f[2],f[3],norm(f[3])]).map_err(|e|e.to_string())?;
        let id: i64 = db
            .query_row(
                "SELECT id FROM gazetteer_places WHERE gazetteer_version_id=?1 AND external_id=?2",
                params![version, f[0]],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        ids.insert(f[0], id);
        for alias in f
            .get(5)
            .copied()
            .unwrap_or_default()
            .split('|')
            .filter(|a| !a.is_empty())
        {
            db.execute("INSERT OR IGNORE INTO gazetteer_aliases(place_id,alias,normalized_alias) VALUES(?1,?2,?3)",params![id,alias,norm(alias)]).map_err(|e|e.to_string())?;
        }
    }
    for line in CATALOG.lines().skip(1) {
        let f: Vec<_> = line.split('\t').collect();
        if let Some(parent_external_id) = f.get(4).filter(|value| !value.is_empty()) {
            let parent = ids
                .get(parent_external_id)
                .ok_or_else(|| format!("missing gazetteer parent {parent_external_id}"))?;
            db.execute("UPDATE gazetteer_places SET parent_id=?1 WHERE gazetteer_version_id=?2 AND external_id=?3",params![parent,version,f[0]]).map_err(|e|e.to_string())?;
        }
    }
    Ok(())
}

fn country_code(country: &str) -> Result<&'static str, String> {
    match country.trim().to_uppercase().as_str() {
        "ITALIA" | "IT" => Ok("IT"),
        "SVIZZERA" | "SWITZERLAND" | "CH" => Ok("CH"),
        _ => Err("unsupported country".into()),
    }
}

pub fn catalog(db: &Connection, country: &str) -> Result<Vec<Place>, String> {
    let code = country_code(country)?;
    let mut statement = db
        .prepare(
            "SELECT p.id,p.level,p.canonical_name,p.parent_id
             FROM gazetteer_places p
             JOIN gazetteer_versions v ON v.id=p.gazetteer_version_id
             WHERE v.status='active' AND p.country_code=?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([code], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let records: HashMap<i64, (String, String, Option<i64>)> = rows
        .iter()
        .map(|(id, level, name, parent_id)| (*id, (level.clone(), name.clone(), *parent_id)))
        .collect();
    let mut aliases: HashMap<i64, Vec<String>> = HashMap::new();
    let mut alias_statement = db
        .prepare(
            "SELECT a.place_id,a.alias
             FROM gazetteer_aliases a
             JOIN gazetteer_places p ON p.id=a.place_id
             JOIN gazetteer_versions v ON v.id=p.gazetteer_version_id
             WHERE v.status='active' AND p.country_code=?1
             ORDER BY a.place_id,a.alias",
        )
        .map_err(|e| e.to_string())?;
    let alias_rows = alias_statement
        .query_map([code], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in alias_rows {
        let (place_id, alias) = row.map_err(|e| e.to_string())?;
        aliases.entry(place_id).or_default().push(alias);
    }
    rows.into_iter()
        .map(|(id, level, name, parent_id)| {
            let mut parents = Vec::new();
            let mut parent_ids = Vec::new();
            let mut next = parent_id;
            while let Some(parent_id) = next {
                let (parent_level, parent_name, parent_parent_id) = records
                    .get(&parent_id)
                    .ok_or_else(|| format!("missing gazetteer parent {parent_id}"))?;
                parents.push((
                    parent_level.clone(),
                    place_display_name(parent_level, parent_name),
                ));
                parent_ids.push(parent_id);
                next = *parent_parent_id;
            }
            let mut place_aliases = aliases.remove(&id).unwrap_or_default();
            if level == "province" {
                for &(alias_key, target_prov) in EXTRA_PROVINCES {
                    if clean_province_name(target_prov) == clean_province_name(&name) {
                        let a = alias_key.to_string();
                        if !place_aliases.contains(&a) {
                            place_aliases.push(a);
                        }
                    }
                }
            }
            let name = place_display_name(&level, &name);
            Ok(Place {
                id,
                country: code.to_owned(),
                level,
                name,
                aliases: place_aliases,
                parents,
                parent_ids,
            })
        })
        .collect()
}

fn place_by_id(db: &Connection, country: &str, id: i64) -> Result<Place, String> {
    let (level, name): (String, String) = db
        .query_row(
            "SELECT level,canonical_name FROM gazetteer_places WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let mut alias_statement = db
        .prepare("SELECT alias FROM gazetteer_aliases WHERE place_id=?1 ORDER BY alias")
        .map_err(|e| e.to_string())?;
    let aliases = alias_statement
        .query_map([id], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut parents = Vec::new();
    let mut parent_ids = Vec::new();
    let mut pid = Some(id);
    while let Some(p) = pid {
        let x: Option<(String, String, Option<i64>)> = db
            .query_row(
                "SELECT level,canonical_name,parent_id FROM gazetteer_places WHERE id=?1",
                [p],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((l, n, next)) = x {
            if p != id {
                let parent_name = place_display_name(&l, &n);
                parents.push((l, parent_name));
                parent_ids.push(p)
            }
            pid = next
        } else {
            break;
        }
    }
    let name = place_display_name(&level, &name);
    Ok(Place {
        id,
        country: country.into(),
        level,
        name,
        aliases,
        parents,
        parent_ids,
    })
}

pub(crate) fn resolve(db: &Connection, country: &str, claim: &str) -> Result<Place, String> {
    let (requested_level, claim) = claim.split_once(':').map_or((None, claim), |(l, n)| {
        (Some(l.trim().to_lowercase()), n.trim())
    });
    let country = country_code(country)?;
    let version: i64 = db
        .query_row(
            "SELECT id FROM gazetteer_versions WHERE status='active'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut st=db.prepare("SELECT DISTINCT p.id,p.level,p.canonical_name FROM gazetteer_places p LEFT JOIN gazetteer_aliases a ON a.place_id=p.id WHERE p.gazetteer_version_id=?1 AND p.country_code=?2 AND (p.normalized_name=?3 OR a.normalized_alias=?3) AND (?4 IS NULL OR p.level=?4)").map_err(|e|e.to_string())?;
    let rows = st
        .query_map(
            params![version, country, norm(claim), requested_level],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if rows.len() != 1 {
        return Err(if rows.is_empty() {
            "località non trovata"
        } else {
            "località ambigua"
        }
        .into());
    }
    place_by_id(db, country, rows[0].0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::setup_schema;
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        db
    }
    #[test]
    fn canonicalizes_alias_and_derives_complete_italian_chain() {
        let p = resolve(&db(), "IT", "municipality:Bozen").unwrap();
        assert_eq!(p.name, "Bolzano/Bozen");
        assert_eq!(
            p.parents.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["province", "region"]
        );
    }
    #[test]
    fn every_italian_province_displays_its_italian_name() {
        let database = db();
        let mut statement = database
            .prepare("SELECT canonical_name FROM gazetteer_places WHERE country_code='IT' AND level='province'")
            .unwrap();
        let names: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(names.len() > 100);
        let unknown: Vec<(String, String)> = names
            .into_iter()
            .map(|name| (place_display_name("province", &name), name))
            .filter(|(shown, _)| {
                !crate::parsing::PROVINCE_REGIONS
                    .iter()
                    .any(|(province, _)| province == shown)
            })
            .collect();
        assert!(unknown.is_empty(), "{unknown:?}");
    }
    #[test]
    fn uses_product_display_name_for_reggio_emilia() {
        let database = db();
        let place = resolve(&database, "IT", "municipality:Reggio nell'Emilia").unwrap();
        assert_eq!(place.name, "Reggio Emilia");
        assert!(
            place
                .parents
                .iter()
                .any(|(level, name)| level == "province" && name == "Reggio Emilia")
        );
    }
    #[test]
    fn autonomous_region_uses_product_region_name() {
        let place = resolve(&db(), "IT", "municipality:Aosta").unwrap();
        assert!(
            place
                .parents
                .iter()
                .any(|(level, name)| level == "region" && name == "Valle d'Aosta")
        );
    }
    #[test]
    fn derives_complete_swiss_chain() {
        let p = resolve(&db(), "CH", "municipality:Lugano").unwrap();
        assert_eq!(p.parents, vec![("canton".into(), "Cantone Ticino".into())]);
    }
    #[test]
    fn known_duplicate_name_is_ambiguous_without_parent_claim() {
        assert_eq!(
            resolve(&db(), "IT", "municipality:Castro").unwrap_err(),
            "località ambigua"
        );
    }
    #[test]
    fn catalog_is_complete_and_country_is_a_constraint() {
        let db = db();
        assert!(catalog(&db, "IT").unwrap().len() > 7900);
        assert!(catalog(&db, "CH").unwrap().len() > 2100);
        let missing_parents: i64 = db
            .query_row(
                "SELECT count(*) FROM gazetteer_places
                 WHERE (country_code='IT' AND level IN ('municipality','province')
                        OR country_code='CH' AND level='municipality')
                   AND parent_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(missing_parents, 0);
        assert!(resolve(&db, "CH", "Milano").is_err());
    }
}
