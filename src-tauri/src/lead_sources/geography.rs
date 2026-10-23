use super::*;

/// Canonical `(comune, provincia, regione)` for the source's raw geography.
/// Municipality names resolve through the gazetteer; a name shared by several
/// municipalities is disambiguated by the source's province or region.
pub(crate) struct Geography {
    municipalities: HashMap<String, Vec<(String, String, String)>>,
}

impl Geography {
    pub(crate) fn load(db: &Connection) -> Result<Self, String> {
        let mut statement = db
            .prepare(
                "SELECT m.canonical_name, p.canonical_name, r.canonical_name, m.normalized_name,
                        COALESCE(group_concat(a.normalized_alias, char(31)), '')
                 FROM gazetteer_places m
                 JOIN gazetteer_places p ON p.id = m.parent_id AND p.level = 'province'
                 JOIN gazetteer_places r ON r.id = p.parent_id AND r.level = 'region'
                 LEFT JOIN gazetteer_aliases a ON a.place_id = m.id
                 WHERE m.country_code = 'IT' AND m.level = 'municipality'
                   AND m.gazetteer_version_id = (SELECT id FROM gazetteer_versions WHERE status = 'active')
                 GROUP BY m.id",
            )
            .map_err(|error| error.to_string())?;
        let mut municipalities: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (comune, provincia, regione, normalized, aliases) =
                row.map_err(|error| error.to_string())?;
            let place = (
                place_display_name("municipality", &comune),
                place_display_name("province", &provincia),
                place_display_name("region", &regione),
            );
            let mut keys: Vec<String> = aliases
                .split('\u{1f}')
                .filter(|alias| !alias.is_empty())
                .map(str::to_owned)
                .collect();
            keys.push(normalized);
            keys.sort();
            keys.dedup();
            for key in keys {
                let entry = municipalities.entry(key).or_default();
                if !entry.contains(&place) {
                    entry.push(place.clone());
                }
            }
        }
        Ok(Self { municipalities })
    }

    pub(crate) fn is_municipality(&self, name: &str) -> bool {
        self.municipalities.contains_key(&norm(name))
    }

    /// Exact municipality inside `regione`, without the readable fallback of
    /// `resolve`: ANA group names are often hamlets or city districts.
    pub(crate) fn municipality_in(
        &self,
        comune: &str,
        regione: &str,
    ) -> Option<(String, String, String)> {
        let found: Vec<_> = self
            .municipalities
            .get(&norm(comune))?
            .iter()
            .filter(|(_, _, region)| norm(region) == norm(regione))
            .collect();
        match found.as_slice() {
            [only] => Some((*only).clone()),
            _ => None,
        }
    }

    pub(crate) fn resolve(
        &self,
        comune: &str,
        provincia: &str,
        regione: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let province_name = canonical_province(provincia);
        let region_name = canonical_region(regione).or_else(|| {
            province_name.and_then(|name| {
                PROVINCE_REGIONS
                    .iter()
                    .find(|(province, _)| *province == name)
                    .map(|(_, region)| *region)
            })
        });
        let candidates = self
            .municipalities
            .get(&norm(comune))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let matching: Vec<_> = candidates
            .iter()
            .filter(|(_, province, region)| {
                province_name.is_none_or(|name| norm(name) == norm(province))
                    && region_name.is_none_or(|name| norm(name) == norm(region))
            })
            .collect();
        let chosen = match (candidates, matching.as_slice()) {
            ([only], _) => Some(only),
            (_, [only]) => Some(*only),
            _ => None,
        };
        match chosen {
            Some((comune, provincia, regione)) => (
                Some(comune.clone()),
                Some(provincia.clone()),
                Some(regione.clone()),
            ),
            None => (
                present(comune).map(title_case),
                province_name.map(str::to_owned),
                region_name.map(str::to_owned),
            ),
        }
    }
}

pub(super) fn canonical_region(value: &str) -> Option<&'static str> {
    let wanted = norm(value);
    if wanted.is_empty() {
        return None;
    }
    // MIUR abbreviates "FRIULI-VENEZIA G.".
    REGIONS.iter().copied().find(|region| {
        norm(region) == wanted || (wanted.len() > 12 && norm(region).starts_with(&wanted))
    })
}

/// Full province names only (MIUR). IndicePA's two-letter codes stay
/// unresolved and the municipality lookup supplies the province instead.
pub(super) fn canonical_province(value: &str) -> Option<&'static str> {
    let wanted = norm(value);
    (wanted.len() > 2)
        .then(|| {
            PROVINCE_REGIONS
                .iter()
                .map(|(province, _)| *province)
                .find(|province| norm(province) == wanted)
        })
        .flatten()
}

/// Title case for values written all in capitals (`DI PIETRO`); mixed-case
/// values are kept as written (`McDonald`).
pub(super) fn tidy_case(value: &str) -> String {
    if value.chars().any(char::is_lowercase) {
        value.trim().to_owned()
    } else {
        title_case(value)
    }
}

pub(crate) fn title_case(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}
