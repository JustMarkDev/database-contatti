use super::*;

pub(super) static CELL_BREAK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<\s*/?\s*(?:td|th|tr|p|br|div|li|table|tbody|h[1-6])\b[^>]*>").unwrap()
});
pub(super) static NUMERIC_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&#(\d{2,6});").unwrap());
pub(super) static MAILTO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)href\s*=\s*["']mailto:[:\s]*([^"'?\s<]+)"#).unwrap());
pub(super) static CAP_TOWN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{5}\s+([^\d(),;]+)").unwrap());

#[derive(Default)]
pub(super) struct AnaUnit {
    name: Option<String>,
    president: Option<String>,
    emails: Vec<String>,
    website: Option<String>,
    town: Option<String>,
}

/// `html_to_text` keeps entities other than the email ones (`&#8217;`).
pub(super) fn ana_text(html: &str) -> String {
    let text = html_to_text(html)
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    NUMERIC_ENTITY
        .replace_all(&text, |captures: &regex::Captures| {
            captures[1]
                .parse()
                .ok()
                .and_then(char::from_u32)
                .map_or_else(String::new, String::from)
        })
        .trim()
        .to_owned()
}

pub(super) fn address_town(line: &str) -> Option<String> {
    let town = CAP_TOWN.captures(line)?.get(1)?.as_str();
    let town = town.split(" Tel").next().unwrap_or(town);
    present(town).map(str::to_owned)
}

/// Group rows start with the group name in capitals (`ALICE CASTELLO`).
pub(super) fn is_group_name(line: &str) -> bool {
    let lower = line.to_lowercase();
    line.chars().any(char::is_alphabetic)
        && line
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(char::is_uppercase)
        && !line.chars().any(|c| c.is_ascii_digit() || c == '@')
        && !lower.contains("www")
        && !lower.contains("http")
        && !matches!(lower.as_str(), "gruppi" | "gruppo" | "informazioni")
}

/// WP Store Locator records of ana.it: a header table (Indirizzo, Presidente,
/// Web, Email) and then "Elenco gruppi Sezione" with one row per group.
pub(crate) fn parse_ana(items: &[serde_json::Value], geography: &Geography) -> Vec<SourceRow> {
    let mut rows = Vec::new();
    for item in items {
        let section = ana_text(item["title"]["rendered"].as_str().unwrap_or(""));
        let link = item["link"].as_str().map(str::to_owned);
        let mut head = AnaUnit::default();
        let mut groups: Vec<AnaUnit> = Vec::new();
        let mut label = String::new();
        let mut in_groups = false;
        let content = item["content"]["rendered"].as_str().unwrap_or("");
        for raw in CELL_BREAK.split(content) {
            let line = ana_text(raw);
            let lower = line.to_lowercase();
            if line.is_empty() {
                continue;
            }
            if lower.starts_with("elenco gruppi") {
                in_groups = true;
                continue;
            }
            // Most sections write "Presidente:", Cuneo and Saluzzo "Presidente".
            if !in_groups && (line.ends_with(':') || lower == "presidente") {
                label = lower.trim_end_matches(':').trim().to_owned();
                continue;
            }
            if in_groups && is_group_name(&line) {
                groups.push(AnaUnit {
                    name: Some(line),
                    ..AnaUnit::default()
                });
                continue;
            }
            if !in_groups && label == "presidente" && !line.contains('@') {
                head.president = head.president.take().or_else(|| Some(tidy_case(&line)));
                label.clear();
                continue;
            }
            let unit = match groups.last_mut() {
                Some(group) if in_groups => group,
                _ => &mut head,
            };
            let mut emails: Vec<String> = crate::EMAIL_RE
                .find_iter(&line)
                .filter_map(|found| source_email(found.as_str()))
                .collect();
            // Some ana.it links point to another address than the text shows.
            // Keep a link only when it is a distinct address on a shown domain,
            // not a truncated copy ("ianoalfieri.cuneo@" for "maglianoalfieri.cuneo@").
            let domain = |email: &str| email.rsplit('@').next().unwrap_or("").to_owned();
            let shown = emails.clone();
            for link in MAILTO
                .captures_iter(raw)
                .filter_map(|captures| source_email(&ana_text(&captures[1])))
            {
                let known_domain =
                    shown.is_empty() || shown.iter().any(|email| domain(email) == domain(&link));
                if known_domain && !emails.iter().any(|email| email.ends_with(&link)) {
                    emails.push(link);
                }
            }
            if !emails.is_empty() {
                unit.emails.extend(emails);
            } else if label == "web" || lower.contains("www.") || lower.starts_with("http") {
                unit.website = unit.website.take().or_else(|| normalize_website(&line));
            } else if unit.town.is_none() {
                unit.town = address_town(&line);
            }
            label.clear();
        }

        let (comune, provincia, regione) = match head.town.as_deref() {
            Some(town) => geography.resolve(town, &section, ""),
            None => (None, None, None),
        };
        // Foreign sections (Canada, Australia, ...) have no Italian geography.
        let place = if regione.is_some() {
            (comune, provincia, regione)
        } else {
            (None, None, None)
        };
        let organization = format!("Sezione ANA {section}");
        let president = head.president.clone();
        let row =
            |email: String,
             department: Option<String>,
             website: Option<String>,
             (comune, provincia, regione): (Option<String>, Option<String>, Option<String>),
             primary: bool| SourceRow {
                is_pec: is_pec_email(&email),
                email,
                // The section president is the referente of the section mailboxes.
                role: (primary && president.is_some()).then(|| "Presidente".to_owned()),
                person_name: president.clone().filter(|_| primary),
                category: Some("Associazioni"),
                subcategory: Some("Alpini"),
                organization: Some(organization.clone()),
                comune: comune.unwrap_or_default(),
                provincia: provincia.unwrap_or_default(),
                regione: regione.unwrap_or_default(),
                website,
                official_code: None,
                department,
                source_url: link.clone(),
                primary,
                address: None,
                postal_code: None,
            };
        for email in head.emails {
            rows.push(row(email, None, head.website.clone(), place.clone(), true));
        }
        for group in groups {
            let group_place = place.2.as_deref().and_then(|region| {
                [group.town.as_deref(), group.name.as_deref()]
                    .into_iter()
                    .flatten()
                    .find_map(|town| geography.municipality_in(town, region))
            });
            let group_place = match group_place {
                Some((comune, provincia, regione)) => {
                    (Some(comune), Some(provincia), Some(regione))
                }
                // Sections can span provinces: keep only the region.
                None => (None, None, place.2.clone()),
            };
            let department = group
                .name
                .as_deref()
                .map(|name| format!("Gruppo {}", title_case(name)));
            let website = group.website.or_else(|| head.website.clone());
            for email in group.emails {
                rows.push(row(
                    email,
                    department.clone(),
                    website.clone(),
                    group_place.clone(),
                    false,
                ));
            }
        }
    }
    rows
}

pub(super) async fn fetch_ana(client: &reqwest::Client) -> Result<Vec<serde_json::Value>, String> {
    let mut items = Vec::new();
    for page in 1..=ANA_MAX_PAGES {
        let url = format!("{ANA_STORES_API}{page}");
        let batch: Vec<serde_json::Value> =
            serde_json::from_str(&download_text(client, &url).await?)
                .map_err(|error| format!("{url}: {error}"))?;
        let last = batch.len() < ANA_PAGE_SIZE;
        items.extend(batch);
        if last {
            break;
        }
    }
    Ok(items)
}
