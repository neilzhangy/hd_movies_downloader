use std::collections::{BTreeMap, HashSet};

use anyhow::{bail, Context, Result};
use chrono::Datelike;
use reqwest::blocking::Client;
use reqwest::Url;
use serde_json::Value;

use crate::feed::normalise_name;
use crate::models::{Release, ReleaseCandidate};

const IMDB_SUGGESTION_BASE: &str = "https://v3.sg.media-imdb.com/suggestion/x";
const CINEMETA_MOVIE_BASE: &str = "https://v3-cinemeta.strem.io/meta/movie";
const MEBIBYTE: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FilterConfig {
    years: HashSet<i32>,
    minimum_torrent_size_bytes: u64,
    minimum_imdb_score: f64,
}

#[derive(Debug, Default)]
pub struct FilterOutcome {
    pub releases: Vec<Release>,
    pub basic_rejections: usize,
    pub rating_rejections: usize,
    pub lookup_failures: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MovieIdentity {
    title: String,
    year: i32,
}

#[derive(Debug, Clone)]
enum RatingLookup {
    Resolved(Option<f64>),
    Failed(String),
}

pub struct MovieFilter {
    config: FilterConfig,
    client: Client,
}

impl FilterConfig {
    pub fn new(
        years: &[i32],
        minimum_torrent_size_mib: u64,
        minimum_imdb_score: f64,
    ) -> Result<Self> {
        if !minimum_imdb_score.is_finite() || !(0.0..10.0).contains(&minimum_imdb_score) {
            bail!("--minimum-imdb-score must be at least 0 and below 10");
        }
        Ok(Self {
            years: effective_years(years),
            minimum_torrent_size_bytes: minimum_torrent_size_mib.saturating_mul(MEBIBYTE),
            minimum_imdb_score,
        })
    }
}

pub fn effective_years(years: &[i32]) -> HashSet<i32> {
    if !years.is_empty() {
        return years.iter().copied().collect();
    }

    let current_year = chrono::Local::now().year();
    [current_year, current_year - 1].into_iter().collect()
}

impl MovieFilter {
    pub fn new(config: FilterConfig, client: Client) -> Self {
        Self { config, client }
    }

    pub fn filter(&self, candidates: &[ReleaseCandidate], verbose: bool) -> FilterOutcome {
        let mut outcome = FilterOutcome::default();
        let mut accepted = BTreeMap::new();
        let mut rating_cache = BTreeMap::new();

        for candidate in candidates {
            let Some(identity) = movie_identity(candidate, &self.config) else {
                outcome.basic_rejections += 1;
                if verbose {
                    eprintln!(
                        "filtered {}: size, year, or 4K/2160p requirement not met",
                        candidate.name
                    );
                }
                continue;
            };

            let rating = rating_cache.entry(identity.clone()).or_insert_with(|| {
                match self.imdb_score(&identity) {
                    Ok(score) => RatingLookup::Resolved(score),
                    Err(error) => RatingLookup::Failed(format!("{error:#}")),
                }
            });
            match rating {
                RatingLookup::Resolved(Some(score)) if *score > self.config.minimum_imdb_score => {
                    accepted
                        .entry(candidate.name.clone())
                        .or_insert_with(|| Release {
                            name: candidate.name.clone(),
                            url: candidate.url.clone(),
                        });
                    if verbose {
                        eprintln!(
                            "accepted {}: IMDb {:.1} is above {:.1}",
                            candidate.name, score, self.config.minimum_imdb_score
                        );
                    }
                }
                RatingLookup::Resolved(Some(score)) => {
                    outcome.rating_rejections += 1;
                    if verbose {
                        eprintln!(
                            "filtered {}: IMDb {:.1} is not above {:.1}",
                            candidate.name, score, self.config.minimum_imdb_score
                        );
                    }
                }
                RatingLookup::Resolved(None) => {
                    outcome.rating_rejections += 1;
                    if verbose {
                        eprintln!(
                            "filtered {}: no exact IMDb movie/rating match for {} ({})",
                            candidate.name, identity.title, identity.year
                        );
                    }
                }
                RatingLookup::Failed(error) => {
                    outcome.lookup_failures += 1;
                    if verbose {
                        eprintln!("filtered {}: IMDb lookup failed: {error}", candidate.name);
                    }
                }
            }
        }

        outcome.releases = accepted.into_values().collect();
        outcome
    }

    fn imdb_score(&self, identity: &MovieIdentity) -> Result<Option<f64>> {
        let Some(imdb_id) = self.find_imdb_id(identity)? else {
            return Ok(None);
        };

        let mut url = Url::parse(CINEMETA_MOVIE_BASE).expect("valid Cinemeta base URL");
        url.path_segments_mut()
            .expect("Cinemeta base URL accepts path segments")
            .push(&format!("{imdb_id}.json"));
        let payload: Value = self
            .client
            .get(url)
            .send()
            .context("fetch IMDb rating metadata")?
            .error_for_status()
            .context("IMDb rating metadata returned an error status")?
            .json()
            .context("decode IMDb rating metadata")?;
        Ok(parse_imdb_rating(&payload))
    }

    fn find_imdb_id(&self, identity: &MovieIdentity) -> Result<Option<String>> {
        let mut url = Url::parse(IMDB_SUGGESTION_BASE).expect("valid IMDb suggestion base URL");
        url.path_segments_mut()
            .expect("IMDb suggestion base URL accepts path segments")
            .push(&format!("{} {}.json", identity.title, identity.year));
        let payload: Value = self
            .client
            .get(url)
            .send()
            .context("search IMDb title suggestions")?
            .error_for_status()
            .context("IMDb title suggestion returned an error status")?
            .json()
            .context("decode IMDb title suggestions")?;
        Ok(find_exact_imdb_movie(&payload, identity))
    }
}

fn movie_identity(candidate: &ReleaseCandidate, config: &FilterConfig) -> Option<MovieIdentity> {
    let size_bytes = candidate.size_bytes?;
    if size_bytes <= config.minimum_torrent_size_bytes || !has_4k_resolution(&candidate.name) {
        return None;
    }

    let words: Vec<_> = candidate.name.split_whitespace().collect();
    let (year_index, year) = words.iter().enumerate().find_map(|(index, word)| {
        word.parse::<i32>()
            .ok()
            .filter(|year| config.years.contains(year))
            .map(|year| (index, year))
    })?;
    let title = normalise_name(&words[..year_index].join(" "));
    if title.is_empty() {
        return None;
    }
    Some(MovieIdentity { title, year })
}

fn has_4k_resolution(name: &str) -> bool {
    name.split_whitespace()
        .any(|word| word.eq_ignore_ascii_case("4k") || word.eq_ignore_ascii_case("2160p"))
}

fn find_exact_imdb_movie(payload: &Value, identity: &MovieIdentity) -> Option<String> {
    payload["d"].as_array()?.iter().find_map(|result| {
        let id = result["id"].as_str()?;
        let title = normalise_name(result["l"].as_str()?);
        let year = result["y"].as_i64()?;
        let kind = result["qid"]
            .as_str()
            .or_else(|| result["q"].as_str())
            .unwrap_or_default();
        let is_movie = matches!(kind, "movie" | "feature");
        (id.starts_with("tt")
            && year == i64::from(identity.year)
            && is_movie
            && title.eq_ignore_ascii_case(&identity.title))
        .then(|| id.to_owned())
    })
}

fn parse_imdb_rating(payload: &Value) -> Option<f64> {
    let score = payload["meta"]["imdbRating"]
        .as_str()?
        .parse::<f64>()
        .ok()?;
    (score.is_finite() && (0.0..=10.0).contains(&score)).then_some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FilterConfig {
        FilterConfig {
            years: [2025, 2026].into_iter().collect(),
            minimum_torrent_size_bytes: 500 * MEBIBYTE,
            minimum_imdb_score: 6.0,
        }
    }

    fn candidate(name: &str, size_mib: u64) -> ReleaseCandidate {
        ReleaseCandidate {
            name: name.to_owned(),
            url: "magnet:?example".to_owned(),
            size_bytes: Some(size_mib * MEBIBYTE),
        }
    }

    #[test]
    fn requires_size_year_and_4k_or_2160p() {
        let filter_config = config();
        assert_eq!(
            movie_identity(
                &candidate("Dune Part Two 2025 2160p WEB", 501),
                &filter_config
            ),
            Some(MovieIdentity {
                title: "Dune Part Two".to_owned(),
                year: 2025,
            })
        );
        assert!(
            movie_identity(&candidate("Dune Part Two 2025 4K WEB", 501), &filter_config).is_some()
        );
        assert!(movie_identity(
            &candidate("Dune Part Two 2025 1080p WEB", 501),
            &filter_config
        )
        .is_none());
        assert!(movie_identity(
            &candidate("Dune Part Two 2024 2160p WEB", 501),
            &filter_config
        )
        .is_none());
        assert!(movie_identity(
            &candidate("Dune Part Two 2025 2160p WEB", 500),
            &filter_config
        )
        .is_none());
    }

    #[test]
    fn matches_only_an_exact_imdb_movie_and_year() {
        let payload: Value = serde_json::json!({
            "d": [
                {"id": "tt0000001", "l": "Dune: Part Two", "y": 2024, "qid": "movie"},
                {"id": "tt15239678", "l": "Dune: Part Two", "y": 2024, "q": "feature"},
                {"id": "tt0000002", "l": "Dune: Part Two", "y": 2023, "qid": "movie"}
            ]
        });
        let identity = MovieIdentity {
            title: "Dune Part Two".to_owned(),
            year: 2024,
        };
        assert_eq!(
            find_exact_imdb_movie(&payload, &identity),
            Some("tt0000001".to_owned())
        );
    }

    #[test]
    fn accepts_valid_imdb_rating_values() {
        assert_eq!(
            parse_imdb_rating(&serde_json::json!({"meta": {"imdbRating": "6.1"}})),
            Some(6.1)
        );
        assert_eq!(
            parse_imdb_rating(&serde_json::json!({"meta": {"imdbRating": "N/A"}})),
            None
        );
    }

    #[test]
    #[ignore = "live integration test for IMDb title resolution and rating metadata"]
    fn resolves_a_live_imdb_rating() {
        let client = crate::http::build_http_client(false).unwrap();
        let filter = MovieFilter::new(config(), client);
        let identity = MovieIdentity {
            title: "Dune Part Two".to_owned(),
            year: 2024,
        };
        assert!(filter.imdb_score(&identity).unwrap().unwrap() > 6.0);
    }
}
