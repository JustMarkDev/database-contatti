//! Phase 0 spike: the plan's measurements on the real database with a Free
//! context.dev key, under a hard cap of 500 credits (the client refuses every
//! call past it). The database is opened read-only; nothing is written.
//!
//! Run: `CONTEXT_DEV_API_KEY=… CONTEXT_DEV_SPIKE_DB=/path/contatti.sqlite3
//! cargo test --manifest-path src-tauri/Cargo.toml contextdev_live_spike -- --ignored --nocapture`
//! Prints counts and domains only, never an address.

use super::*;
use crate::contextdev::ContextDev;
use crate::lead_candidates::{KnownSites, SiteMatch, choose_maps_site, is_unwanted};
use std::time::Instant;

const SPIKE_CREDITS: u64 = 500;
const SITES_PER_CATEGORY: usize = 10;
const POOR_CATEGORIES: [&str; 4] = [
    "Enti religiosi",
    "Associazioni",
    "Turismo e promozione territoriale",
    "Cultura",
];
/// Map `search` phrases tried against the local score, per category.
const MAP_SEARCH: [(&str, &str); 4] = [
    ("Enti religiosi", "contatti parroco segreteria parrocchiale"),
    ("Associazioni", "consiglio direttivo contatti"),
    (
        "Turismo e promozione territoriale",
        "ufficio informazioni contatti",
    ),
    ("Cultura", "contatti direzione uffici"),
];

fn rows(db: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Vec<Vec<String>> {
    let mut statement = db.prepare(sql).expect("spike query");
    let columns = statement.column_count();
    statement
        .query_map(args, |row| {
            (0..columns)
                .map(|index| {
                    row.get::<_, Option<String>>(index)
                        .map(Option::unwrap_or_default)
                })
                .collect()
        })
        .expect("spike rows")
        .collect::<Result<_, _>>()
        .expect("spike values")
}

fn known_emails(db: &Connection, host: &str) -> HashSet<String> {
    rows(
        db,
        "SELECT lower(email) FROM contacts WHERE lower(email_domain||email_tld)=?1",
        &[&host.trim_start_matches("www.")],
    )
    .into_iter()
    .map(|row| row[0].clone())
    .collect()
}

#[tokio::test]
#[ignore = "live context.dev spike, spends up to 500 credits; see the module docs"]
async fn contextdev_live_spike() {
    let key = std::env::var("CONTEXT_DEV_API_KEY").expect("CONTEXT_DEV_API_KEY");
    let path = std::env::var("CONTEXT_DEV_SPIKE_DB").expect("CONTEXT_DEV_SPIKE_DB");
    let db = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("database opens read-only");
    let context = ContextDev::new(key, 2, Some(SPIKE_CREDITS)).expect("client");
    let usage = context.usage().await.expect("GET /org/usage");
    println!(
        "SPIKE usage: remaining={} auto_topup={} budget={:?}",
        usage.credits_remaining, usage.auto_topup.enabled, usage.auto_topup.monthly_budget_credits
    );
    usage
        .spend_guard()
        .expect("Auto-Topup must be off or capped before spending");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let settings = CrawlSettings {
        context: &context,
        max_pages: 3,
        max_pdfs: 0,
        directory: false,
        ai: None,
        cancel: &cancel,
    };

    // Reading: ~10 pending seeds with a site per poor category.
    let mut read = Vec::new();
    for category in POOR_CATEGORIES {
        let seeds = rows(
            &db,
            "SELECT url, host FROM search_seeds WHERE status='pending' AND url IS NOT NULL
               AND category=?1 GROUP BY host ORDER BY random() LIMIT ?2",
            &[&category, &(SITES_PER_CATEGORY as i64)],
        );
        for seed in seeds {
            let before = context.credits_used();
            let started = Instant::now();
            let crawl = crawl_host(&settings, &[seed[0].clone()], Some(category)).await;
            let credits = context.credits_used() - before;
            let host = crawl.site_host.clone().unwrap_or_else(|| seed[1].clone());
            let found: HashSet<String> = crawl
                .pages
                .iter()
                .flat_map(|page| parse_page(&page.url, &page.html).emails)
                .map(|hit| hit.email.to_lowercase())
                .collect();
            let known = known_emails(&db, &host);
            let recovered = found.intersection(&known).count();
            println!(
                "SPIKE site {category} {host}: credits={credits} pages={} known={} recovered={recovered} new={} blocked={} outbound={} {:.1}s",
                crawl.pages.len(),
                known.len(),
                found.len() - recovered,
                crawl
                    .errors
                    .iter()
                    .filter(|error| error.contains("bloccata"))
                    .count(),
                crawl.outbound.len(),
                started.elapsed().as_secs_f64(),
            );
            read.push((category, host, crawl, credits, found.len() - recovered));
            if context.stop_reason().is_some() {
                break;
            }
        }
    }
    let sites = read.len().max(1) as f64;
    let total: u64 = read.iter().map(|(_, _, _, credits, _)| credits).sum();
    let new_emails: usize = read.iter().map(|(_, _, _, _, new)| new).sum();
    println!(
        "SPIKE reading: sites={} credits/site={:.2} new emails/credit={:.3}",
        read.len(),
        total as f64 / sites,
        new_emails as f64 / total.max(1) as f64
    );

    // Map with and without `search` on the first 20 sites, against the local score.
    for (category, host, crawl, _, _) in read.iter().take(20) {
        if context.stop_reason().is_some() {
            break;
        }
        let phrase = MAP_SEARCH
            .iter()
            .find(|(name, _)| name == category)
            .map(|(_, phrase)| *phrase);
        let plain = context.map(host, None, None).await.unwrap_or_default();
        let searched = context.map(host, None, phrase).await.unwrap_or_default();
        let home = crawl.pages.first();
        let anchors = home.map_or_else(Vec::new, |page| page_anchors(&page.url, &page.html));
        let ranked = rank_candidates(host, &HashSet::new(), &anchors, &plain, Some(category));
        let picked: HashSet<String> = pick_pages(&ranked, &[], 2)
            .iter()
            .map(|url| page_key(url))
            .collect();
        let deep = plain
            .iter()
            .filter(|page| picked.contains(&page_key(&page.url)))
            .filter(|page| {
                !anchors
                    .iter()
                    .any(|(url, _)| page_key(url) == page_key(&page.url))
            })
            .count();
        let search_hits = searched
            .iter()
            .take(5)
            .filter(|page| picked.contains(&page_key(&page.url)))
            .count();
        println!(
            "SPIKE map {host}: urls={} with_search={} picked_from_map_only={deep} search_top5_in_picked={search_hits}",
            plain.len(),
            searched.len()
        );
    }

    // Price of html vs markdown and of a cached repeat on one page.
    if let Some((_, _, crawl, _, _)) = read
        .iter()
        .find(|(_, _, crawl, _, _)| !crawl.pages.is_empty())
    {
        let url = crawl.pages[0].url.clone();
        for (label, markdown) in [("html", false), ("markdown", true), ("html again", false)] {
            let before = context.credits_used();
            let result = if markdown {
                context.scrape_markdown(&url).await
            } else {
                context.scrape_html(&url).await
            };
            println!(
                "SPIKE price {label}: credits={} ok={}",
                context.credits_used() - before,
                result.is_ok()
            );
        }
    }

    // Discovery, channel 4: ten searches, filtered against the known sites.
    let known = KnownSites::load(&db).expect("known sites");
    let provinces = ["Bergamo", "Brescia", "Como", "Varese", "Verona"];
    for (index, query) in provinces
        .iter()
        .flat_map(|provincia| {
            [
                format!("parrocchia {provincia}"),
                format!("pro loco {provincia}"),
            ]
        })
        .take(10)
        .enumerate()
    {
        let before = context.credits_used();
        let hits = context.search(&query).await.unwrap_or_default();
        let fresh = hits
            .iter()
            .filter_map(|hit| url_host(&hit.url))
            .filter(|host| !known.contains(host) && !is_unwanted(host))
            .count();
        println!(
            "SPIKE search {index}: results={} new_sites={fresh} credits={}",
            hits.len(),
            context.credits_used() - before
        );
    }

    // Discovery, channel 3: five Maps searches, and five organizations whose site is known.
    for provincia in provinces {
        let before = context.credits_used();
        let places = context
            .scrape_maps(&format!("parrocchia {provincia}"))
            .await;
        println!(
            "SPIKE maps discovery {provincia}: results={:?} with_site={:?} credits={}",
            places.as_ref().map(Vec::len),
            places.as_ref().map(|places| places
                .iter()
                .filter(|place| place.website.is_some())
                .count()),
            context.credits_used() - before
        );
    }
    let entities = rows(
        &db,
        "SELECT max(organization_name), comune, max(website) FROM contacts
          WHERE subcategory='Parrocchia' AND coalesce(website,'')<>'' AND coalesce(comune,'')<>''
            AND coalesce(organization_name,'')<>''
          GROUP BY comune ORDER BY random() LIMIT 5",
        &[],
    );
    let mut exact = 0;
    for entity in &entities {
        let before = context.credits_used();
        let places = context
            .scrape_maps(&format!("{} {}", entity[0], entity[1]))
            .await
            .unwrap_or_default();
        let expected =
            crate::lead_crawl::normalize_seed_url(&entity[2]).and_then(|url| url_host(&url));
        let decision = choose_maps_site(&entity[0], &entity[1], &places);
        let right = matches!(&decision, SiteMatch::Found(url)
            if url_host(url).map(|host| host.trim_start_matches("www.").to_owned())
                == expected.as_deref().map(|host| host.trim_start_matches("www.").to_owned()));
        exact += usize::from(right);
        println!(
            "SPIKE maps check: decision={} right={right} credits={}",
            match decision {
                SiteMatch::Found(_) => "found",
                SiteMatch::ToVerify(_) => "verify",
                SiteMatch::NotFound => "none",
            },
            context.credits_used() - before
        );
    }
    println!("SPIKE maps precision: {exact}/{}", entities.len());

    // Channel 1: hub pages (comuni and diocesi) and their links to other sites.
    let hubs = rows(
        &db,
        "SELECT url FROM search_seeds WHERE url IS NOT NULL AND (host LIKE 'comune.%' OR host LIKE '%diocesi%')
          GROUP BY host ORDER BY random() LIMIT 5",
        &[],
    );
    for hub in hubs {
        let crawl = crawl_host(&settings, &[hub[0].clone()], None).await;
        let fresh = crawl
            .outbound
            .iter()
            .filter_map(|(url, _)| url_host(url))
            .filter(|host| !known.contains(host) && !is_unwanted(host))
            .count();
        println!(
            "SPIKE hub {}: outbound={} new_sites={fresh}",
            url_host(&hub[0]).unwrap_or_default(),
            crawl.outbound.len()
        );
    }
    println!(
        "SPIKE total credits={} stop={:?}",
        context.credits_used(),
        context.stop_reason()
    );
    assert!(context.credits_used() <= SPIKE_CREDITS + 2, "cap respected");
}
