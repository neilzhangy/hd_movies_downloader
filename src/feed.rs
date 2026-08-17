use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use scraper::{ElementRef, Html, Selector};

use crate::models::ReleaseCandidate;

pub const DEFAULT_SOURCES: [&str; 2] = [
    "https://tpb.party/top/207",
    "https://tpb.party/browse/207/1/7/0",
];

pub fn effective_sources(sources: &[String]) -> Vec<String> {
    if sources.is_empty() {
        DEFAULT_SOURCES
            .iter()
            .map(|source| (*source).to_owned())
            .collect()
    } else {
        sources.to_vec()
    }
}

pub fn scan_sources(
    client: &Client,
    sources: &[String],
    verbose: bool,
) -> Result<Vec<ReleaseCandidate>> {
    let mut by_name = BTreeMap::new();
    let mut successful_feeds = 0;

    for source in sources {
        match fetch_source(client, source) {
            Ok(page) => {
                successful_feeds += 1;
                let parsed = parse_candidates(&page, source);
                if parsed.is_empty() {
                    eprintln!(
                        "warning: feed {source} returned no recognizable torrent rows; no releases from it were recorded"
                    );
                } else if verbose {
                    eprintln!(
                        "feed {source}: parsed {} candidate release(s)",
                        parsed.len()
                    );
                }
                for candidate in parsed {
                    let key = candidate.name.clone();
                    let should_replace = by_name
                        .get(&key)
                        .map(|existing: &ReleaseCandidate| {
                            existing.size_bytes.is_none() && candidate.size_bytes.is_some()
                        })
                        .unwrap_or(true);
                    if should_replace {
                        by_name.insert(key, candidate);
                    }
                }
            }
            Err(error) => eprintln!("warning: could not scan {source}: {error:#}"),
        }
    }

    if successful_feeds == 0 {
        bail!("every configured feed failed; SQLite and Transmission were left unchanged");
    }
    Ok(by_name.into_values().collect())
}

fn fetch_source(client: &Client, source: &str) -> Result<String> {
    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 1..=3 {
        match client.get(source).send() {
            Ok(response) => match response.error_for_status() {
                Ok(response) => return response.text().context("read feed response"),
                Err(error) => last_error = Some(error.into()),
            },
            Err(error) => last_error = Some(error.into()),
        }
        if attempt < 3 {
            thread::sleep(Duration::from_secs(attempt));
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("unknown HTTP error"))).context("fetch feed")
}

pub fn parse_candidates(page: &str, source: &str) -> Vec<ReleaseCandidate> {
    let document = Html::parse_document(page);
    let detail_selector =
        Selector::parse(".detName a, a.detLink, a[title^='Details for']").expect("valid selector");
    let anchor_selector = Selector::parse("a[href]").expect("valid selector");
    let record_selectors = [
        Selector::parse("tr").expect("valid selector"),
        Selector::parse(".torrent").expect("valid selector"),
        Selector::parse(".item").expect("valid selector"),
    ];
    let mut candidates = Vec::new();

    for record_selector in &record_selectors {
        for record in document.select(record_selector) {
            if let Some(candidate) =
                candidate_from_element(record, &detail_selector, &anchor_selector, source)
            {
                candidates.push(candidate);
            }
        }
    }

    if candidates.is_empty() {
        candidates.extend(parse_legacy_blocks(page, source, &anchor_selector));
    }

    let mut unique = BTreeMap::new();
    for candidate in candidates {
        unique.entry(candidate.name.clone()).or_insert(candidate);
    }
    unique.into_values().collect()
}

fn candidate_from_element(
    element: ElementRef<'_>,
    detail_selector: &Selector,
    anchor_selector: &Selector,
    source: &str,
) -> Option<ReleaseCandidate> {
    let raw_name = element
        .select(detail_selector)
        .next()
        .map(element_text)
        .filter(|name| !name.is_empty())?;
    let name = normalise_name(&raw_name);
    if name.is_empty() {
        return None;
    }

    let url = element
        .select(anchor_selector)
        .filter_map(|anchor| anchor.value().attr("href"))
        .find(|href| is_torrent_link(href))
        .map(|href| resolve_url(source, href))?;
    Some(ReleaseCandidate {
        name,
        url,
        size_bytes: torrent_size_bytes(&element_text(element)),
    })
}

fn parse_legacy_blocks(
    page: &str,
    source: &str,
    anchor_selector: &Selector,
) -> Vec<ReleaseCandidate> {
    let mut releases = Vec::new();
    let mut cursor = 0;

    while let Some(relative_start) = page[cursor..].find("detName") {
        let start = cursor + relative_start;
        let after_start = start + "detName".len();
        let end = page[after_start..]
            .find("detName")
            .map(|relative_end| after_start + relative_end)
            .unwrap_or(page.len());
        let fragment = Html::parse_fragment(&page[start..end]);
        let anchors: Vec<_> = fragment.select(anchor_selector).collect();
        let raw_name = anchors.first().map(|anchor| element_text(*anchor));
        let url = anchors
            .iter()
            .filter_map(|anchor| anchor.value().attr("href"))
            .find(|href| is_torrent_link(href))
            .map(|href| resolve_url(source, href));

        if let (Some(raw_name), Some(url)) = (raw_name, url) {
            let name = normalise_name(&raw_name);
            if !name.is_empty() {
                releases.push(ReleaseCandidate {
                    name,
                    url,
                    size_bytes: torrent_size_bytes(&element_text(fragment.root_element())),
                });
            }
        }
        cursor = end;
    }
    releases
}

fn torrent_size_bytes(text: &str) -> Option<u64> {
    let tokens: Vec<_> = text.split_whitespace().collect();
    for pair in tokens.windows(2) {
        let number = pair[0]
            .trim_matches(|character: char| !character.is_ascii_digit() && character != '.')
            .parse::<f64>();
        let Ok(value) = number else {
            continue;
        };
        let unit = pair[1]
            .trim_matches(|character: char| !character.is_ascii_alphanumeric())
            .to_ascii_lowercase();
        let multiplier = match unit.as_str() {
            "b" => 1,
            "kb" => 1_000,
            "kib" => 1_024,
            "mb" => 1_000_000,
            "mib" => 1_024 * 1_024,
            "gb" => 1_000_000_000,
            "gib" => 1_024 * 1_024 * 1_024,
            "tb" => 1_000_000_000_000,
            "tib" => 1_024_u64.pow(4),
            _ => continue,
        };
        if value.is_finite() && value >= 0.0 {
            return Some((value * multiplier as f64) as u64);
        }
    }
    None
}

fn element_text(element: ElementRef<'_>) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

fn is_torrent_link(href: &str) -> bool {
    let lowered = href.trim().to_ascii_lowercase();
    lowered.starts_with("magnet:")
        || lowered.contains(".torrent")
        || lowered.contains("/torrent/download")
}

fn resolve_url(source: &str, href: &str) -> String {
    let href = href.trim();
    if href.to_ascii_lowercase().starts_with("magnet:") {
        return href.to_owned();
    }
    match reqwest::Url::parse(source).and_then(|base| base.join(href)) {
        Ok(url) => url.into(),
        Err(_) => href.to_owned(),
    }
}

pub fn normalise_name(name: &str) -> String {
    let mut output = String::new();
    let mut previous_was_separator = true;
    for character in name.chars() {
        if character.is_alphanumeric() {
            output.push(character);
            previous_was_separator = false;
        } else if !previous_was_separator {
            output.push(' ');
            previous_was_separator = true;
        }
    }
    output.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::build_http_client;

    #[test]
    fn normalises_legacy_style_names() {
        assert_eq!(normalise_name("A.Movie_2026-1080p!"), "A Movie 2026 1080p");
    }

    #[test]
    #[ignore = "live integration test against tpb.party; run with cargo test -- --ignored"]
    fn parses_real_tpb_party_pages() {
        let client = build_http_client(false).unwrap();
        for source in DEFAULT_SOURCES {
            let page = fetch_source(&client, source).unwrap();
            let candidates = parse_candidates(&page, source);
            assert!(
                !candidates.is_empty(),
                "{source} returned no parsable torrent releases; inspect its current HTML before deploying"
            );
            assert!(candidates.iter().all(|candidate| {
                !candidate.name.is_empty()
                    && is_torrent_link(&candidate.url)
                    && candidate.size_bytes.is_some()
            }));
        }
    }

    #[test]
    fn parses_torrent_size_units() {
        assert_eq!(torrent_size_bytes("Film 1.5 GiB 7 2"), Some(1_610_612_736));
        assert_eq!(torrent_size_bytes("Film 500 MiB 7 2"), Some(524_288_000));
        assert_eq!(torrent_size_bytes("Film without a size"), None);
    }
}
