use std::cmp::Reverse;
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
    /// Exactly one selected release per resolved IMDb movie.
    pub releases: Vec<Release>,
    pub basic_rejections: usize,
    pub rating_rejections: usize,
    pub lookup_failures: usize,
    pub duplicate_rejections: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MovieIdentity {
    title: String,
    year: i32,
}

impl MovieIdentity {
    fn movie_key(&self) -> String {
        format!("{} {}", self.title.to_ascii_lowercase(), self.year)
    }
}

#[derive(Debug, Clone)]
struct ImdbMovie {
    id: String,
    rating: f64,
}

#[derive(Debug, Clone)]
enum MovieLookup {
    Resolved(Option<ImdbMovie>),
    Failed(String),
}

#[derive(Debug)]
struct RatedCandidate<'candidate> {
    candidate: &'candidate ReleaseCandidate,
    identity: MovieIdentity,
    imdb: ImdbMovie,
}

/// Higher fields always win. A TPB row with a known zero-seeder swarm loses to
/// one with seeders; otherwise Dolby Vision is deliberately first, before source
/// quality or swarm popularity is considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateRank {
    has_seeders: u8,
    dolby_vision: u8,
    source_tier: u8,
    hdr: u8,
    codec_tier: u8,
    seeders: u64,
    leechers: u64,
    size_bytes: u64,
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

    /// Filters candidates and selects one best torrent for each exact IMDb movie.
    /// IMDb lookups are cached by normalized title/year within this scan; the
    /// resolved IMDb ID is then used to collapse alternate TPB release names.
    pub fn filter(&self, candidates: &[ReleaseCandidate], verbose: bool) -> FilterOutcome {
        let mut outcome = FilterOutcome::default();
        let mut groups: BTreeMap<String, Vec<RatedCandidate<'_>>> = BTreeMap::new();
        let mut lookup_cache = BTreeMap::new();

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

            let lookup = lookup_cache.entry(identity.clone()).or_insert_with(|| {
                match self.resolve_imdb_movie(&identity) {
                    Ok(movie) => MovieLookup::Resolved(movie),
                    Err(error) => MovieLookup::Failed(format!("{error:#}")),
                }
            });
            match lookup {
                MovieLookup::Resolved(Some(movie))
                    if movie.rating > self.config.minimum_imdb_score =>
                {
                    groups
                        .entry(movie.id.clone())
                        .or_default()
                        .push(RatedCandidate {
                            candidate,
                            identity,
                            imdb: movie.clone(),
                        });
                }
                MovieLookup::Resolved(Some(movie)) => {
                    outcome.rating_rejections += 1;
                    if verbose {
                        eprintln!(
                            "filtered {}: IMDb {:.1} is not above {:.1}",
                            candidate.name, movie.rating, self.config.minimum_imdb_score
                        );
                    }
                }
                MovieLookup::Resolved(None) => {
                    outcome.rating_rejections += 1;
                    if verbose {
                        eprintln!(
                            "filtered {}: no exact IMDb movie/rating match for {} ({})",
                            candidate.name, identity.title, identity.year
                        );
                    }
                }
                MovieLookup::Failed(error) => {
                    outcome.lookup_failures += 1;
                    if verbose {
                        eprintln!("filtered {}: IMDb lookup failed: {error}", candidate.name);
                    }
                }
            }
        }

        for (imdb_id, group) in groups {
            let selected = select_best_candidate(&group);
            let rank = candidate_rank(selected.candidate);
            outcome.duplicate_rejections += group.len().saturating_sub(1);
            if verbose {
                eprintln!(
                    "selected {} for IMDb {} from {} TPB variant(s): {}",
                    selected.candidate.name,
                    imdb_id,
                    group.len(),
                    rank_description(rank, selected.candidate),
                );
            }
            outcome.releases.push(Release {
                name: selected.candidate.name.clone(),
                url: selected.candidate.url.clone(),
                movie_key: selected.identity.movie_key(),
                imdb_id,
                imdb_rating: selected.imdb.rating,
            });
        }

        outcome
    }

    fn resolve_imdb_movie(&self, identity: &MovieIdentity) -> Result<Option<ImdbMovie>> {
        let Some(imdb_id) = self.find_imdb_id(identity)? else {
            return Ok(None);
        };
        let Some(rating) = self.imdb_score(&imdb_id)? else {
            return Ok(None);
        };
        Ok(Some(ImdbMovie {
            id: imdb_id,
            rating,
        }))
    }

    fn imdb_score(&self, imdb_id: &str) -> Result<Option<f64>> {
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

fn select_best_candidate<'group, 'candidate>(
    group: &'group [RatedCandidate<'candidate>],
) -> &'group RatedCandidate<'candidate> {
    group
        .iter()
        .max_by_key(|rated| {
            (
                candidate_rank(rated.candidate),
                Reverse(rated.candidate.name.as_str()),
                Reverse(rated.candidate.url.as_str()),
            )
        })
        .expect("selection group is never empty")
}

fn candidate_rank(candidate: &ReleaseCandidate) -> CandidateRank {
    let lowered = candidate.name.to_ascii_lowercase();
    let words: Vec<_> = lowered.split_whitespace().collect();
    CandidateRank {
        has_seeders: u8::from(candidate.seeders.unwrap_or(1) > 0),
        dolby_vision: u8::from(has_dolby_vision(&words)),
        source_tier: source_tier(&words),
        hdr: u8::from(has_any_token(&words, &["hdr", "hdr10", "hlg"])),
        codec_tier: codec_tier(&words),
        seeders: candidate.seeders.unwrap_or_default(),
        leechers: candidate.leechers.unwrap_or_default(),
        size_bytes: candidate.size_bytes.unwrap_or_default(),
    }
}

fn has_dolby_vision(words: &[&str]) -> bool {
    has_any_token(words, &["dv", "dovi"]) || has_phrase(words, &["dolby", "vision"])
}

fn source_tier(words: &[&str]) -> u8 {
    if has_any_token(words, &["remux", "bdremux"]) {
        4
    } else if has_any_token(words, &["bluray", "bdrip", "brrip"])
        || has_phrase(words, &["blu", "ray"])
    {
        3
    } else if has_any_token(words, &["webdl"]) || has_phrase(words, &["web", "dl"]) {
        2
    } else if has_any_token(words, &["webrip"]) || has_phrase(words, &["web", "rip"]) {
        1
    } else {
        0
    }
}

fn codec_tier(words: &[&str]) -> u8 {
    if has_any_token(words, &["av1"]) {
        3
    } else if has_any_token(words, &["hevc", "x265", "h265"]) || has_phrase(words, &["h", "265"]) {
        2
    } else if has_any_token(words, &["x264", "h264"]) || has_phrase(words, &["h", "264"]) {
        1
    } else {
        0
    }
}

fn has_any_token(words: &[&str], expected: &[&str]) -> bool {
    words
        .iter()
        .any(|word| expected.iter().any(|token| word == token))
}

fn has_phrase(words: &[&str], phrase: &[&str]) -> bool {
    words.windows(phrase.len()).any(|window| window == phrase)
}

fn rank_description(rank: CandidateRank, candidate: &ReleaseCandidate) -> String {
    let mut labels = Vec::new();
    if rank.dolby_vision > 0 {
        labels.push("Dolby Vision".to_owned());
    }
    labels.push(match rank.source_tier {
        4 => "REMUX".to_owned(),
        3 => "BluRay".to_owned(),
        2 => "WEB-DL".to_owned(),
        1 => "WEBRip".to_owned(),
        _ => "unknown source".to_owned(),
    });
    if rank.hdr > 0 {
        labels.push("HDR".to_owned());
    }
    if let Some(seeders) = candidate.seeders {
        labels.push(format!("{seeders} seeders"));
    }
    labels.join(", ")
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

    fn candidate(name: &str, size_mib: u64, seeders: u64) -> ReleaseCandidate {
        ReleaseCandidate {
            name: name.to_owned(),
            url: format!("magnet:?{name}"),
            size_bytes: Some(size_mib * MEBIBYTE),
            seeders: Some(seeders),
            leechers: Some(1),
        }
    }

    fn rated_candidate<'candidate>(
        candidate: &'candidate ReleaseCandidate,
    ) -> RatedCandidate<'candidate> {
        RatedCandidate {
            candidate,
            identity: MovieIdentity {
                title: "Dune Part Two".to_owned(),
                year: 2025,
            },
            imdb: ImdbMovie {
                id: "tt15239678".to_owned(),
                rating: 8.4,
            },
        }
    }

    fn live_remote_client() -> reqwest::blocking::Client {
        let proxy = std::env::var("HD_MOVIES_PROXY").ok();
        crate::http::build_remote_http_client(false, proxy.as_deref()).unwrap()
    }

    #[test]
    fn requires_size_year_and_4k_or_2160p() {
        let filter_config = config();
        assert_eq!(
            movie_identity(
                &candidate("Dune Part Two 2025 2160p WEB", 501, 1),
                &filter_config
            ),
            Some(MovieIdentity {
                title: "Dune Part Two".to_owned(),
                year: 2025,
            })
        );
        assert!(movie_identity(
            &candidate("Dune Part Two 2025 4K WEB", 501, 1),
            &filter_config
        )
        .is_some());
        assert!(movie_identity(
            &candidate("Dune Part Two 2025 1080p WEB", 501, 1),
            &filter_config
        )
        .is_none());
        assert!(movie_identity(
            &candidate("Dune Part Two 2024 2160p WEB", 501, 1),
            &filter_config
        )
        .is_none());
        assert!(movie_identity(
            &candidate("Dune Part Two 2025 2160p WEB", 500, 1),
            &filter_config
        )
        .is_none());
    }

    #[test]
    fn prioritises_dolby_vision_before_source_quality_and_swarm_size() {
        let dolby_vision = candidate("Dune Part Two 2025 2160p DV WEB DL HEVC", 12_000, 4);
        let non_dolby_remux = candidate(
            "Dune Part Two 2025 2160p BluRay REMUX HDR HEVC",
            60_000,
            40_000,
        );
        let group = vec![
            rated_candidate(&dolby_vision),
            rated_candidate(&non_dolby_remux),
        ];

        assert_eq!(
            select_best_candidate(&group).candidate.name,
            dolby_vision.name,
            "Dolby Vision must win even if a non-Dolby variant has a better source tag and more seeders"
        );
    }

    #[test]
    fn does_not_prefer_a_known_dead_dolby_vision_swarm() {
        let dead_dolby_vision = candidate("Dune Part Two 2025 2160p DV REMUX HEVC", 60_000, 0);
        let live_non_dolby = candidate("Dune Part Two 2025 2160p WEB DL HEVC", 12_000, 10);
        let group = vec![
            rated_candidate(&dead_dolby_vision),
            rated_candidate(&live_non_dolby),
        ];

        assert_eq!(
            select_best_candidate(&group).candidate.name,
            live_non_dolby.name
        );
    }

    #[test]
    fn uses_seeders_to_choose_equivalent_dolby_vision_variants() {
        let fewer_seeders = candidate("Dune Part Two 2025 2160p DoVi WEB DL HEVC", 12_000, 10);
        let more_seeders = candidate(
            "Dune Part Two 2025 2160p Dolby Vision WEB DL HEVC",
            12_000,
            20,
        );
        let group = vec![
            rated_candidate(&fewer_seeders),
            rated_candidate(&more_seeders),
        ];

        assert_eq!(
            select_best_candidate(&group).candidate.name,
            more_seeders.name
        );
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
        let client = live_remote_client();
        let filter = MovieFilter::new(config(), client);
        let identity = MovieIdentity {
            title: "Dune Part Two".to_owned(),
            year: 2024,
        };
        assert!(
            filter
                .resolve_imdb_movie(&identity)
                .unwrap()
                .unwrap()
                .rating
                > 6.0
        );
    }

    #[test]
    #[ignore = "live TPB/IMDb/database integration test; run with cargo test -- --ignored"]
    fn selects_one_live_tpb_variant_per_imdb_movie() {
        let client = live_remote_client();
        let sources =
            vec!["https://tpb.party/search/dune%20part%20two%202024%202160p/1/99/0".to_owned()];
        let candidates = crate::feed::scan_sources(&client, &sources, false).unwrap();
        let filter = MovieFilter::new(
            FilterConfig {
                years: [2024].into_iter().collect(),
                minimum_torrent_size_bytes: 500 * MEBIBYTE,
                minimum_imdb_score: 6.0,
            },
            client,
        );
        let outcome = filter.filter(&candidates, false);
        let dune_releases: Vec<_> = outcome
            .releases
            .iter()
            .filter(|release| release.imdb_id == "tt15239678")
            .collect();
        assert_eq!(
            dune_releases.len(),
            1,
            "every matching TPB Dune: Part Two variant must collapse to one selection"
        );
        let selected_name = dune_releases[0].name.to_ascii_lowercase();
        let selected_words: Vec<_> = selected_name.split_whitespace().collect();
        assert!(
            has_dolby_vision(&selected_words),
            "the live search contains Dolby Vision variants, so the selected release must be Dolby Vision"
        );
        assert!(
            outcome.duplicate_rejections > 0,
            "the live query should contain multiple qualifying Dune: Part Two variants"
        );

        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("movies.db");
        let mut database = crate::db::open_database(&database_path).unwrap();
        crate::db::record_releases(&mut database, &outcome.releases, "baseline").unwrap();
        let stored_dune_count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM movies WHERE imdb_id = 'tt15239678'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_dune_count, 1);
    }
}
