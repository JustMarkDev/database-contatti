use super::*;

/// The robots.txt group that applies to us (RFC 9309): our own token if any
/// group names it, otherwise `*`. Sitemap lines are global.
#[derive(Debug, Default, Clone)]
pub(crate) struct Robots {
    /// `(allow, pattern)` pairs; patterns keep `*` and a trailing `$`.
    rules: Vec<(bool, String)>,
    pub(super) crawl_delay: Option<Duration>,
    pub(super) sitemaps: Vec<String>,
}

impl Robots {
    pub(super) fn delay(&self) -> Duration {
        self.crawl_delay
            .unwrap_or_default()
            .max(Duration::from_millis(FETCH_PAUSE_MILLIS))
    }

    /// Longest matching pattern wins; on a tie `Allow` wins (RFC 9309 §2.2.2).
    pub(super) fn allows(&self, url: &str) -> bool {
        let path = url_path(url);
        let path = if path.is_empty() { "/" } else { path.as_str() };
        let mut best: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            if !robots_pattern_matches(pattern, path) {
                continue;
            }
            let better = best.is_none_or(|(length, best_allow)| {
                pattern.len() > length || (pattern.len() == length && *allow && !best_allow)
            });
            if better {
                best = Some((pattern.len(), *allow));
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

pub(super) fn robots_pattern_matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(stripped) => (stripped, true),
        None => (pattern, false),
    };
    let mut parts = pattern.split('*');
    let Some(mut rest) = path.strip_prefix(parts.next().unwrap_or("")) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    for (index, part) in parts.iter().enumerate() {
        if anchored && index + 1 == parts.len() {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    !anchored || rest.is_empty()
}

pub(crate) fn parse_robots(text: &str) -> Robots {
    let mut ours = Robots::default();
    let mut star = Robots::default();
    let mut ours_named = false;
    let mut group_agents: Vec<String> = Vec::new();
    let mut reading_agents = false;
    let mut sitemaps = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "user-agent" => {
                if !reading_agents {
                    group_agents.clear();
                    reading_agents = true;
                }
                let agent = value.to_ascii_lowercase();
                ours_named |= agent == ROBOTS_TOKEN;
                group_agents.push(agent);
            }
            "sitemap" => sitemaps.push(value.to_owned()),
            key @ ("allow" | "disallow" | "crawl-delay") => {
                reading_agents = false;
                for (target, token) in [(&mut ours, ROBOTS_TOKEN), (&mut star, "*")] {
                    if !group_agents.iter().any(|agent| agent == token) {
                        continue;
                    }
                    if key == "crawl-delay" {
                        if let Ok(seconds) = value.parse::<f64>()
                            && seconds.is_finite()
                            && seconds >= 0.0
                        {
                            target.crawl_delay = Some(Duration::from_secs_f64(seconds));
                        }
                    } else if !value.is_empty() {
                        target.rules.push((key == "allow", value.to_owned()));
                    }
                }
            }
            _ => {}
        }
    }
    let mut chosen = if ours_named { ours } else { star };
    chosen.sitemaps = sitemaps;
    chosen
}

/// 4xx means "no robots.txt, crawl freely"; 5xx, 429 and unreachable hosts
/// mean "assume everything is disallowed" (RFC 9309 §2.3.1), so the caller
/// skips the host on `Err`.
pub(super) async fn load_robots(client: &reqwest::Client, seed: &str) -> Result<Robots, String> {
    let Some(url) = resolve_url(seed, "/robots.txt") else {
        return Ok(Robots::default());
    };
    match fetch_html(client, &url).await {
        Ok(text) => Ok(parse_robots(&text)),
        Err(FetchError {
            status: Some(status),
            ..
        }) if (400..500).contains(&status) && status != 429 => Ok(Robots::default()),
        Err(error) => Err(error.message),
    }
}
