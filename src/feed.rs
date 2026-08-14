use std::collections::{BTreeMap, HashSet};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Datelike;
use reqwest::blocking::Client;
use scraper::{ElementRef, Html, Selector};

use crate::models::Release;

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

pub fn effective_years(years: &[i32]) -> HashSet<i32> {
    if !years.is_empty() {
        return years.iter().copied().collect();
    }

    let current_year = chrono::Local::now().year();
    [current_year, current_year - 1].into_iter().collect()
}

pub fn scan_sources(
    client: &Client,
    sources: &[String],
    years: &HashSet<i32>,
    quality: &str,
    verbose: bool,
) -> Result<Vec<Release>> {
    let mut by_name = BTreeMap::new();
    let mut successful_feeds = 0;

    for source in sources {
        match fetch_source(client, source) {
            Ok(page) => {
                successful_feeds += 1;
                let parsed = parse_releases(&page, source);
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
                for release in parsed {
                    if eligible(&release.name, years, quality) {
                        by_name.entry(release.name.clone()).or_insert(release);
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

pub fn parse_releases(page: &str, source: &str) -> Vec<Release> {
    let document = Html::parse_document(page);
    let detail_selector =
        Selector::parse(".detName a, a.detLink, a[title^='Details for']").expect("valid selector");
    let anchor_selector = Selector::parse("a[href]").expect("valid selector");
    let record_selectors = [
        Selector::parse("tr").expect("valid selector"),
        Selector::parse(".torrent").expect("valid selector"),
        Selector::parse(".item").expect("valid selector"),
    ];
    let mut releases = Vec::new();

    for record_selector in &record_selectors {
        for record in document.select(record_selector) {
            if let Some(release) =
                release_from_element(record, &detail_selector, &anchor_selector, source)
            {
                releases.push(release);
            }
        }
    }

    if releases.is_empty() {
        releases.extend(parse_legacy_blocks(page, source, &anchor_selector));
    }

    let mut unique = BTreeMap::new();
    for release in releases {
        unique.entry(release.name.clone()).or_insert(release);
    }
    unique.into_values().collect()
}

fn release_from_element(
    element: ElementRef<'_>,
    detail_selector: &Selector,
    anchor_selector: &Selector,
    source: &str,
) -> Option<Release> {
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
    Some(Release { name, url })
}

fn parse_legacy_blocks(page: &str, source: &str, anchor_selector: &Selector) -> Vec<Release> {
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
                releases.push(Release { name, url });
            }
        }
        cursor = end;
    }
    releases
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

fn eligible(name: &str, years: &HashSet<i32>, quality: &str) -> bool {
    years.iter().any(|year| name.contains(&year.to_string()))
        && name
            .to_ascii_lowercase()
            .contains(&quality.to_ascii_lowercase())
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
            let releases = parse_releases(&page, source);
            assert!(
                !releases.is_empty(),
                "{source} returned no parsable torrent releases; inspect its current HTML before deploying"
            );
            assert!(releases
                .iter()
                .all(|release| { !release.name.is_empty() && is_torrent_link(&release.url) }));
        }
    }

    #[test]
    fn uses_year_and_quality_filters() {
        let years = [2026].into_iter().collect();
        assert!(eligible("Movie 2026 1080p", &years, "1080"));
        assert!(!eligible("Movie 2025 1080p", &years, "1080"));
        assert!(!eligible("Movie 2026 2160p", &years, "1080"));
    }
}
