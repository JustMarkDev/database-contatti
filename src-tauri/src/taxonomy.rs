use rusqlite::{Connection, Transaction, params};

use crate::TaxonomyGap;

pub(crate) const SUBCATEGORY_ALIASES: &[(&str, &str, &str)] = &[
    ("Istruzione", "Scuola dell'infanzia", "Infanzia"),
    ("Istruzione", "Scuola primaria", "Primaria"),
    (
        "Istruzione",
        "Scuola secondaria di primo grado",
        "Secondaria di primo grado",
    ),
    (
        "Istruzione",
        "Scuola secondaria di secondo grado",
        "Secondaria di secondo grado",
    ),
    (
        "Istruzione",
        "Centro per l'istruzione degli adulti (CPIA)",
        "Centro territoriale / CPIA",
    ),
    (
        "Pubblica amministrazione",
        "Forze dell'ordine / Sicurezza",
        "Forze dell'ordine",
    ),
    (
        "Associazioni",
        "Volontariato / Assistenza",
        "Volontariato / solidarietà",
    ),
];

pub(crate) fn fallback_subcategory(category: &str) -> Option<&'static str> {
    match crate::gazetteer::norm(category).as_str() {
        "istruzione" => Some("Altro ente formativo"),
        "pubblica amministrazione" => Some("Altro ente pubblico"),
        "sanita" | "sanità" => Some("Altro ente sanitario"),
        "associazioni" => Some("Associazione generica"),
        "enti religiosi" => Some("Altro ente religioso"),
        "cultura" => Some("Altro ente culturale"),
        "turismo e promozione territoriale" => Some("Altro ente turistico"),
        "varie" => Some("Ente non determinato"),
        _ => None,
    }
}

pub(crate) fn canonical_subcategory_label(category: &str, value: &str) -> Option<&'static str> {
    let normalized_category = crate::gazetteer::norm(category);
    let normalized_value = crate::gazetteer::norm(value);
    SUBCATEGORY_ALIASES
        .iter()
        .find(|(candidate_category, alias, canonical)| {
            crate::gazetteer::norm(candidate_category) == normalized_category
                && (crate::gazetteer::norm(alias) == normalized_value
                    || crate::gazetteer::norm(canonical) == normalized_value)
        })
        .map(|(_, _, canonical)| *canonical)
}

#[derive(Debug, Clone)]
pub struct TaxonomyVersion {
    pub id: i64,
    pub semantic_version: String,
    pub categories: Vec<(String, Vec<String>)>,
}
pub fn load_active_taxonomy(db: &Connection) -> Result<TaxonomyVersion, String> {
    let mut s = db
        .prepare("SELECT id,semantic_version FROM taxonomy_versions WHERE status='active'")
        .map_err(|e| e.to_string())?;
    let v: Vec<(i64, String)> = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if v.len() != 1 {
        return Err(format!("expected one active taxonomy, found {}", v.len()));
    }
    let (id, semantic_version) = v[0].clone();
    let mut c=db.prepare("SELECT semantic_key FROM taxonomy_entries WHERE taxonomy_version_id=?1 AND level='category' ORDER BY sort_order,semantic_key").map_err(|e|e.to_string())?;
    let mut cats = Vec::new();
    for r in c
        .query_map(params![id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
    {
        let key = r.map_err(|e| e.to_string())?;
        let subs=db.prepare("SELECT semantic_key FROM taxonomy_entries WHERE taxonomy_version_id=?1 AND level='subcategory' AND parent_semantic_key=?2 ORDER BY sort_order,semantic_key").map_err(|e|e.to_string())?.query_map(params![id,key],|r|r.get(0)).map_err(|e|e.to_string())?.collect::<Result<Vec<String>,_>>().map_err(|e|e.to_string())?;
        cats.push((key, subs));
    }
    Ok(TaxonomyVersion {
        id,
        semantic_version,
        categories: cats,
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CandidateApproval {
    Applied {
        version_id: i64,
        semantic_version: String,
        category: String,
        subcategory: Option<String>,
    },
    Stale,
}

fn semantic_key(display_name: &str) -> String {
    crate::gazetteer::norm(display_name).replace(' ', "_")
}

pub(crate) fn taxonomy_gap_group_key(gap: &TaxonomyGap) -> String {
    let category = gap
        .nearest_category_key
        .as_deref()
        .or(gap.parent_category_key.as_deref())
        .map(crate::gazetteer::norm)
        .unwrap_or_default();
    let parent = gap
        .parent_category_key
        .as_deref()
        .map(crate::gazetteer::norm)
        .unwrap_or_default();
    let label = gap.proposed_display_name.trim().to_lowercase();
    let definition = gap.proposed_definition.trim().to_lowercase();
    serde_json::json!({
        "level": gap.level.trim().to_lowercase(),
        "category": category,
        "parent": parent,
        "label": label,
        "definition": definition,
    })
    .to_string()
}

fn subcategory_equivalent_exists(
    db: &Connection,
    taxonomy_version_id: i64,
    parent_key: &str,
    parent_display: &str,
    proposed_display: &str,
) -> Result<bool, String> {
    let proposed_normalized = crate::gazetteer::norm(proposed_display);
    let proposed_canonical =
        canonical_subcategory_label(parent_display, proposed_display).map(crate::gazetteer::norm);
    let mut statement = db
        .prepare(
            "SELECT display_name,synonyms FROM taxonomy_entries
             WHERE taxonomy_version_id=?1 AND level='subcategory' AND parent_semantic_key=?2",
        )
        .map_err(|error| error.to_string())?;
    let entries = statement
        .query_map(params![taxonomy_version_id, parent_key], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    for entry in entries {
        let (display_name, synonyms_json) = entry.map_err(|error| error.to_string())?;
        let display_normalized = crate::gazetteer::norm(&display_name);
        if display_normalized == proposed_normalized
            || proposed_canonical.as_ref().is_some_and(|canonical| {
                canonical_subcategory_label(parent_display, &display_name)
                    .map(crate::gazetteer::norm)
                    .as_ref()
                    == Some(canonical)
            })
        {
            return Ok(true);
        }
        let synonyms: Vec<String> = serde_json::from_str(&synonyms_json).unwrap_or_default();
        if synonyms
            .iter()
            .any(|synonym| crate::gazetteer::norm(synonym) == proposed_normalized)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn next_semantic_version(db: &Connection) -> Result<String, String> {
    let mut statement = db
        .prepare("SELECT semantic_version FROM taxonomy_versions")
        .map_err(|error| error.to_string())?;
    let versions = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut maximum = 0_u64;
    for version in versions {
        let version = version.map_err(|error| error.to_string())?;
        if let Some(number) = version
            .strip_prefix("taxonomy-v")
            .and_then(|value| value.parse::<u64>().ok())
        {
            maximum = maximum.max(number);
        }
    }
    Ok(format!("taxonomy-v{}", maximum + 1))
}

pub(crate) fn approve_change_candidate(
    tx: &Transaction<'_>,
    candidate_id: &str,
) -> Result<CandidateApproval, String> {
    let (based_on_version_id, change_json): (i64, String) = tx
        .query_row(
            "SELECT based_on_version_id,change_json
             FROM taxonomy_change_candidates WHERE id=?1 AND status='pending'",
            params![candidate_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| error.to_string())?;
    let active_version_id: i64 = tx
        .query_row(
            "SELECT id FROM taxonomy_versions WHERE status='active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if based_on_version_id != active_version_id {
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE taxonomy_change_candidates SET status='stale',decided_at=?1 WHERE id=?2",
            params![now, candidate_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "UPDATE classification_reviews SET status='rejected',resolved_at=?1 WHERE id=?2",
            params![now, candidate_id],
        )
        .map_err(|error| error.to_string())?;
        return Ok(CandidateApproval::Stale);
    }

    let gap: TaxonomyGap = serde_json::from_str(&change_json).map_err(|error| error.to_string())?;
    let level = gap.level.trim().to_ascii_lowercase();
    if !matches!(level.as_str(), "category" | "subcategory") {
        return Err("Livello della proposta di tassonomia non valido".to_owned());
    }
    let key = semantic_key(&gap.proposed_display_name);
    if key.is_empty() {
        return Err("Nome della proposta di tassonomia vuoto".to_owned());
    }
    let parent_key = if level == "subcategory" {
        Some(
            gap.parent_category_key
                .as_deref()
                .or(gap.nearest_category_key.as_deref())
                .ok_or_else(|| "La nuova sottocategoria non ha una categoria padre".to_owned())?
                .to_owned(),
        )
    } else {
        None
    };
    if let Some(parent) = parent_key.as_deref() {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM taxonomy_entries
                 WHERE taxonomy_version_id=?1 AND level='category' AND semantic_key=?2)",
                params![active_version_id, parent],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !exists {
            return Err("Categoria padre della proposta non trovata".to_owned());
        }
    }
    let duplicate: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM taxonomy_entries
             WHERE taxonomy_version_id=?1 AND level=?2 AND semantic_key=?3)",
            params![active_version_id, level, key],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if duplicate {
        return Err("La voce proposta esiste già nella tassonomia attiva".to_owned());
    }
    if let Some(parent) = parent_key.as_deref() {
        let parent_display: String = tx
            .query_row(
                "SELECT display_name FROM taxonomy_entries
                 WHERE taxonomy_version_id=?1 AND level='category' AND semantic_key=?2",
                params![active_version_id, parent],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if subcategory_equivalent_exists(
            tx,
            active_version_id,
            parent,
            &parent_display,
            &gap.proposed_display_name,
        )? {
            return Err("La voce proposta equivale a una sottocategoria già attiva".to_owned());
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let semantic_version = next_semantic_version(tx)?;
    tx.execute(
        "UPDATE taxonomy_versions SET status='retired' WHERE id=?1",
        params![active_version_id],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT INTO taxonomy_versions(semantic_version,status,created_at,activated_at)
         VALUES(?1,'active',?2,?2)",
        params![semantic_version, now],
    )
    .map_err(|error| error.to_string())?;
    let version_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO taxonomy_entries(
            taxonomy_version_id,semantic_key,level,parent_semantic_key,display_name,
            definition,synonyms,inclusions,exclusions,precedence_rules,sort_order,reviewer_only
         )
         SELECT ?1,semantic_key,level,parent_semantic_key,display_name,
                definition,synonyms,inclusions,exclusions,precedence_rules,sort_order,reviewer_only
         FROM taxonomy_entries WHERE taxonomy_version_id=?2",
        params![version_id, active_version_id],
    )
    .map_err(|error| error.to_string())?;
    let next_sort: i64 = tx
        .query_row(
            "SELECT coalesce(max(sort_order),-1)+1 FROM taxonomy_entries
             WHERE taxonomy_version_id=?1 AND level=?2 AND parent_semantic_key IS ?3",
            params![version_id, level, parent_key],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let definition = serde_json::json!({ "it": gap.proposed_definition }).to_string();
    tx.execute(
        "INSERT INTO taxonomy_entries(
            taxonomy_version_id,semantic_key,level,parent_semantic_key,display_name,
            definition,sort_order,reviewer_only
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,0)",
        params![
            version_id,
            key,
            level,
            parent_key,
            gap.proposed_display_name,
            definition,
            next_sort
        ],
    )
    .map_err(|error| error.to_string())?;

    let (category, subcategory) = if level == "category" {
        tx.execute(
            "INSERT OR IGNORE INTO categories(name,normalized_name,sort_order,active)
             VALUES(?1,?2,?3,1)",
            params![gap.proposed_display_name, key, next_sort],
        )
        .map_err(|error| error.to_string())?;
        let category_id: i64 = tx
            .query_row(
                "SELECT id FROM categories WHERE name=?1 AND active=1",
                params![gap.proposed_display_name],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT OR IGNORE INTO subcategories(category_id,name,normalized_name,active)
             VALUES(?1,'Altro ente','altro_ente',1)",
            params![category_id],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO taxonomy_entries(
                taxonomy_version_id,semantic_key,level,parent_semantic_key,display_name,
                definition,sort_order,reviewer_only
             ) VALUES(?1,'altro_ente','subcategory',?2,'Altro ente','{}',?3,0)",
            params![version_id, key, next_sort + 1],
        )
        .map_err(|error| error.to_string())?;
        (gap.proposed_display_name, Some("Altro ente".to_owned()))
    } else {
        let parent = parent_key.as_deref().expect("validated parent key");
        let parent_display: String = tx
            .query_row(
                "SELECT display_name FROM taxonomy_entries
                 WHERE taxonomy_version_id=?1 AND level='category' AND semantic_key=?2",
                params![version_id, parent],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let category_id: i64 = tx
            .query_row(
                "SELECT id FROM categories WHERE name=?1",
                params![parent_display],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT OR IGNORE INTO subcategories(category_id,name,normalized_name,active)
             VALUES(?1,?2,?3,1)",
            params![category_id, gap.proposed_display_name, key],
        )
        .map_err(|error| error.to_string())?;
        (parent_display, Some(gap.proposed_display_name))
    };
    tx.execute(
        "UPDATE taxonomy_change_candidates
         SET status='approved',resulting_version_id=?1,decided_at=?2 WHERE id=?3",
        params![version_id, now, candidate_id],
    )
    .map_err(|error| error.to_string())?;
    Ok(CandidateApproval::Applied {
        version_id,
        semantic_version,
        category,
        subcategory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_fallbacks_are_category_specific() {
        assert_eq!(
            fallback_subcategory("Istruzione"),
            Some("Altro ente formativo")
        );
        assert_eq!(
            fallback_subcategory("Pubblica amministrazione"),
            Some("Altro ente pubblico")
        );
        assert_eq!(fallback_subcategory("Sanità"), Some("Altro ente sanitario"));
        assert_eq!(
            fallback_subcategory("Associazioni"),
            Some("Associazione generica")
        );
        assert_eq!(
            fallback_subcategory("Enti religiosi"),
            Some("Altro ente religioso")
        );
        assert_eq!(
            fallback_subcategory("Cultura"),
            Some("Altro ente culturale")
        );
        assert_eq!(
            fallback_subcategory("Turismo e promozione territoriale"),
            Some("Altro ente turistico")
        );
        assert_eq!(fallback_subcategory("Varie"), Some("Ente non determinato"));
    }

    #[test]
    fn taxonomy_gap_grouping_ignores_evidence_and_preserves_scope() {
        let first = TaxonomyGap {
            level: "subcategory".to_owned(),
            nearest_category_key: Some("Istruzione".to_owned()),
            nearest_subcategory_key: None,
            proposed_display_name: "Robotica".to_owned(),
            proposed_definition: "Corsi specialistici".to_owned(),
            parent_category_key: Some("istruzione".to_owned()),
            evidence: vec![],
        };
        let mut second = first.clone();
        second.evidence.push(crate::GroundedSourceReference {
            source_id: "S2".to_owned(),
            field: "Nome".to_owned(),
            quote: "altra prova".to_owned(),
        });
        assert_eq!(
            taxonomy_gap_group_key(&first),
            taxonomy_gap_group_key(&second)
        );
        second.proposed_definition = "Corsi diversi".to_owned();
        assert_ne!(
            taxonomy_gap_group_key(&first),
            taxonomy_gap_group_key(&second)
        );
    }

    #[test]
    fn equivalent_subcategory_detection_covers_aliases_and_declared_synonyms() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE taxonomy_entries(
                taxonomy_version_id INTEGER NOT NULL,
                semantic_key TEXT NOT NULL,
                level TEXT NOT NULL,
                parent_semantic_key TEXT,
                display_name TEXT NOT NULL,
                synonyms TEXT NOT NULL DEFAULT '[]'
             );
             INSERT INTO taxonomy_entries VALUES
                (1,'infanzia','subcategory','istruzione','Infanzia','[\"Materna\"]');",
        )
        .unwrap();

        for proposed in ["Scuola dell'infanzia", "Materna", "Infanzia"] {
            assert!(
                subcategory_equivalent_exists(&db, 1, "istruzione", "Istruzione", proposed)
                    .unwrap()
            );
        }
        assert!(
            !subcategory_equivalent_exists(&db, 1, "istruzione", "Istruzione", "Primaria").unwrap()
        );
    }
}
