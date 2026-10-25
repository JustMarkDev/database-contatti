//! Public web lists of organizations (**directories**) for Cerca lead, and
//! the gazetteer lookup that places the addresses a crawl finds.
//!
//! A directory is read like any site (Map + Scrape) following its own pages;
//! its addresses become leads with the directory's taxonomy and its links to
//! other sites become candidate seeds (`lead_candidates::accept_candidates`).

use serde::Serialize;

mod geography;

pub(crate) use geography::*;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LeadDirectory {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) publisher: &'static str,
    pub(crate) landing_url: &'static str,
    /// Taxonomy the directory's leads and seeds inherit.
    pub(crate) category: &'static str,
    pub(crate) subcategory: &'static str,
    pub(crate) note: &'static str,
}

const fn directory(
    id: &'static str,
    name: &'static str,
    publisher: &'static str,
    landing_url: &'static str,
    category: &'static str,
    subcategory: &'static str,
    note: &'static str,
) -> LeadDirectory {
    LeadDirectory {
        id,
        name,
        publisher,
        landing_url,
        category,
        subcategory,
        note,
    }
}

const RELIGIOSI: &str = "Enti religiosi";

pub(crate) const DIRECTORIES: &[LeadDirectory] = &[
    directory(
        "dir-cai",
        "CAI: sezioni e sottosezioni",
        "Club Alpino Italiano",
        "https://www.cai.it/sezioni-territoriali/sezioni-e-sottosezioni/",
        "Associazioni",
        "CAI",
        "Elenco per regione: i siti delle sezioni diventano siti da esplorare.",
    ),
    directory(
        "dir-caritas",
        "Caritas diocesane",
        "Caritas Italiana",
        "https://www.caritas.it/caritas-diocesane/",
        RELIGIOSI,
        "Caritas / ente caritativo religioso",
        "Email e siti delle Caritas di ogni diocesi.",
    ),
    directory(
        "dir-rotary-2080",
        "Rotary: club del Distretto 2080",
        "Rotary International Distretto 2080",
        "https://www.rotary2080.org/club/",
        "Associazioni",
        "Rotary",
        "Email dei club di Lazio e Sardegna. Altri distretti: aggiungi il loro elenco club.",
    ),
    directory(
        "dir-rotary-2071",
        "Rotary: club del Distretto 2071",
        "Rotary International Distretto 2071",
        "https://www.rotary2071.org/club/",
        "Associazioni",
        "Rotary",
        "Club della Toscana: ogni pagina club ha il sito e l'email del club.",
    ),
    directory(
        "dir-rotary-2042",
        "Rotary: club del Distretto 2042",
        "Rotary International Distretto 2042",
        "https://rotary2042.it/club/",
        "Associazioni",
        "Rotary",
        "Club della Lombardia (Bergamo, Como, Lecco, Milano, Monza, Sondrio, Varese).",
    ),
    directory(
        "dir-lions-108ib1",
        "Lions: club del Distretto 108 Ib1",
        "Lions Clubs International Distretto 108 Ib1",
        "https://www.lions108ib1.it/circoscrizioni",
        "Associazioni",
        "Lions",
        "Club di Como, Lecco, Monza Brianza, Sondrio e Varese, con i referenti di zona.",
    ),
    directory(
        "dir-fom-oratori",
        "Oratori ambrosiani (FOM)",
        "Fondazione Oratori Milanesi",
        "https://www.chiesadimilano.it/pgfom/oratori/",
        RELIGIOSI,
        "Oratorio / centro giovanile",
        "Pagine e siti degli oratori della diocesi di Milano.",
    ),
    directory(
        "dir-anpas",
        "ANPAS: pubbliche assistenze",
        "ANPAS",
        "https://www.anpas.org/associazioni/",
        "Associazioni",
        "Volontariato / solidarietà",
        "Comitati regionali e associazioni di pubblica assistenza.",
    ),
    directory(
        "dir-azione-cattolica",
        "Azione Cattolica diocesana",
        "Azione Cattolica Italiana",
        "https://azionecattolica.it/diocesi/",
        RELIGIOSI,
        "Altro ente religioso",
        "Siti delle associazioni diocesane di Azione Cattolica.",
    ),
];

#[tauri::command]
pub(crate) fn list_lead_directories() -> Vec<LeadDirectory> {
    DIRECTORIES.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::setup_schema;
    use rusqlite::Connection;

    #[test]
    fn lead_directories_ids_are_unique_and_point_at_public_pages() {
        let mut ids: Vec<_> = DIRECTORIES.iter().map(|entry| entry.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), DIRECTORIES.len());
        for entry in DIRECTORIES {
            assert!(entry.landing_url.starts_with("https://"), "{}", entry.id);
            assert!(!entry.category.is_empty() && !entry.subcategory.is_empty());
            assert!(crate::lead_crawl::normalize_seed_url(entry.landing_url).is_some());
        }
    }

    #[test]
    fn lead_directories_geography_resolves_to_contacts_vocabulary() {
        let db = Connection::open_in_memory().unwrap();
        setup_schema(&db).unwrap();
        let geography = Geography::load(&db).unwrap();
        // Uppercase full names and two-letter province codes converge.
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
        // A duplicated municipality name is settled by the region.
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
}
