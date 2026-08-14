use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::{json, Value};

#[derive(Debug)]
pub struct TransmissionClient {
    client: Client,
    endpoint: String,
    session_id: Option<HeaderValue>,
}

#[derive(Debug)]
pub struct CompletedTorrent {
    pub id: i64,
    pub name: String,
    pub download_dir: PathBuf,
}

pub fn transmission_endpoint(ip: &str, port: u16) -> String {
    let host = if ip.contains(':') && !(ip.starts_with('[') && ip.ends_with(']')) {
        format!("[{ip}]")
    } else {
        ip.to_owned()
    };
    format!("http://{host}:{port}/transmission/rpc")
}

impl TransmissionClient {
    pub fn new(client: Client, endpoint: String) -> Self {
        Self {
            client,
            endpoint,
            session_id: None,
        }
    }

    fn request(&self, payload: &Value) -> RequestBuilder {
        let mut request = self
            .client
            .post(&self.endpoint)
            .header(CONTENT_TYPE, "application/json")
            .json(payload);
        if let Some(session_id) = &self.session_id {
            request = request.header("X-Transmission-Session-Id", session_id);
        }
        request
    }

    fn call(&mut self, method: &str, arguments: Value) -> Result<Value> {
        let payload = json!({"method": method, "arguments": arguments});
        for _ in 0..2 {
            let response = self
                .request(&payload)
                .send()
                .with_context(|| format!("call Transmission {method}"))?;
            if response.status() == StatusCode::CONFLICT {
                let session_id = response
                    .headers()
                    .get("X-Transmission-Session-Id")
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("Transmission returned 409 without X-Transmission-Session-Id")
                    })?;
                self.session_id = Some(session_id);
                continue;
            }

            let status = response.status();
            let body = response.text().context("read Transmission response")?;
            if !status.is_success() {
                bail!("Transmission {method} returned HTTP {status}: {body}");
            }
            let value: Value =
                serde_json::from_str(&body).context("decode Transmission JSON response")?;
            if value.get("result").and_then(Value::as_str) != Some("success") {
                bail!("Transmission {method} failed: {body}");
            }
            return Ok(value);
        }
        bail!("Transmission repeatedly requested a new RPC session id")
    }

    pub fn session_download_dir(&mut self) -> Result<PathBuf> {
        let value = self.call("session-get", json!({"fields": ["download-dir"]}))?;
        let path = value["arguments"]["download-dir"]
            .as_str()
            .ok_or_else(|| anyhow!("Transmission session-get did not return download-dir"))?;
        Ok(PathBuf::from(path))
    }

    pub fn add_torrent(&mut self, url: &str, download_dir: &Path) -> Result<()> {
        let value = self.call(
            "torrent-add",
            json!({
                "filename": url,
                "download-dir": download_dir.to_string_lossy(),
            }),
        )?;
        let arguments = &value["arguments"];
        if arguments.get("torrent-added").is_some() || arguments.get("torrent-duplicate").is_some()
        {
            return Ok(());
        }
        bail!(
            "Transmission accepted torrent-add but returned no torrent-added/torrent-duplicate object"
        )
    }

    pub fn completed_torrents(&mut self) -> Result<Vec<CompletedTorrent>> {
        let value = self.call(
            "torrent-get",
            json!({
                "fields": ["id", "name", "status", "percentDone", "downloadDir"],
            }),
        )?;
        let torrents = value["arguments"]["torrents"]
            .as_array()
            .ok_or_else(|| anyhow!("Transmission torrent-get did not return torrents"))?;
        let mut completed = Vec::new();
        for torrent in torrents {
            if !is_completed_torrent(&torrent["status"], &torrent["percentDone"]) {
                continue;
            }
            let Some(id) = torrent["id"].as_i64() else {
                continue;
            };
            let Some(download_dir) = torrent["downloadDir"].as_str() else {
                continue;
            };
            completed.push(CompletedTorrent {
                id,
                name: torrent["name"]
                    .as_str()
                    .unwrap_or("completed movie")
                    .to_owned(),
                download_dir: PathBuf::from(download_dir),
            });
        }
        Ok(completed)
    }

    pub fn remove_torrent(&mut self, id: i64) -> Result<()> {
        self.call(
            "torrent-remove",
            json!({"ids": [id], "delete-local-data": false}),
        )?;
        Ok(())
    }
}

fn is_completed_torrent(status: &Value, percent_done: &Value) -> bool {
    let complete = percent_done.as_f64().unwrap_or_default() >= 0.999_999;
    matches!(status.as_i64(), Some(6))
        || (matches!(status.as_i64(), Some(0)) && complete)
        || matches!(status.as_str(), Some("seeding"))
        || (matches!(status.as_str(), Some("stopped")) && complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_standard_transmission_rpc_endpoints() {
        assert_eq!(
            transmission_endpoint("192.168.0.127", 9999),
            "http://192.168.0.127:9999/transmission/rpc"
        );
        assert_eq!(
            transmission_endpoint("fd00::2", 9091),
            "http://[fd00::2]:9091/transmission/rpc"
        );
    }
}
