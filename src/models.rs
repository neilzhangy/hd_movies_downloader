#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub name: String,
    pub url: String,
    /// Normalized title and year. It remains stable when TPB release tags differ.
    pub movie_key: String,
    /// Exact IMDb movie identifier used as the canonical identity during a scan.
    pub imdb_id: String,
    pub imdb_rating: f64,
}

/// A torrent row as advertised by a remote index before it has passed the movie filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCandidate {
    pub name: String,
    pub url: String,
    pub size_bytes: Option<u64>,
    pub seeders: Option<u64>,
    pub leechers: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct StoredRelease {
    pub name: String,
    pub url: String,
    pub movie_key: Option<String>,
    pub imdb_id: Option<String>,
    pub imdb_rating: Option<f64>,
    pub first_seen_at: String,
    pub status: String,
    pub queued_at: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
}
