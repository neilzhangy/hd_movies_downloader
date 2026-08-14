#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub name: String,
    pub url: String,
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
