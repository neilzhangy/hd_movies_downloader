use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;

pub fn build_http_client(insecure_tls: bool) -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(45))
        .user_agent("hd-movies/3.0")
        .danger_accept_invalid_certs(insecure_tls)
        .build()
        .context("build HTTP client")
}
