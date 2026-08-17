#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub name: String,
    pub url: String,
}

/// A torrent row as advertised by a remote index before it has passed the movie filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCandidate {
    pub name: String,
    pub url: String,
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct StoredRelease {
    pub name: String,
    pub url: String,
    pub first_seen_at: String,
    pub status: String,
    pub queued_at: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
}
