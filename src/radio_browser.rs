use crate::models::{RadioBrowserServer, Station};
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use rand::seq::SliceRandom;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use std::time::Duration;
use url::Url;

const BOOTSTRAP_BASE: &str = "https://all.api.radio-browser.info";
const MAX_BODY_BYTES: usize = 1_000_000;

#[derive(Debug, Clone)]
pub struct RadioBrowserClient {
    http: reqwest::Client,
    last_server: Option<String>,
}

impl RadioBrowserClient {
    pub fn new(last_server: Option<String>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "RadioWidget/0.1 (COSMIC applet; +https://github.com/xinia/cosmic-ext-radio)",
            ),
        );
        let http = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self { http, last_server })
    }

    pub fn last_server(&self) -> Option<&str> {
        self.last_server.as_deref()
    }

    pub async fn discover_servers(&self) -> Result<Vec<String>> {
        let url = format!("{BOOTSTRAP_BASE}/json/servers");
        let resp = self.http.get(url).send().await.context("Server discovery failed")?;
        let bytes = read_limited(resp, MAX_BODY_BYTES).await?;
        let servers: Vec<RadioBrowserServer> =
            serde_json::from_slice(&bytes).context("Invalid /json/servers response")?;
        let mut names: Vec<String> = servers.into_iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        if names.is_empty() {
            return Err(anyhow!("Radio Browser server list was empty"));
        }
        Ok(names)
    }

    pub async fn search(&mut self, query: &str, limit: u32) -> Result<Vec<Station>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(vec![]);
        }

        let http = self.http.clone();
        let query = query.to_string();
        self.with_server_retry("search", move |base| {
            let http = http.clone();
            let query = query.clone();
            async move {
            let mut url = Url::parse(&format!("{base}/json/stations/search"))
                .context("Invalid Radio Browser base URL")?;
            url.query_pairs_mut()
                .append_pair("name", &urlencoding::encode(&query))
                .append_pair("hidebroken", "true")
                .append_pair("limit", &limit.to_string())
                .append_pair("order", "votes")
                .append_pair("reverse", "true");
            eprintln!("[RadioWidget][search] GET {}", url);
            let resp = http.get(url).send().await?;
            eprintln!("[RadioWidget][search] Response: status = {}", resp.status());
            let bytes = read_limited(resp, MAX_BODY_BYTES).await?;
            let stations: Vec<Station> =
                serde_json::from_slice(&bytes).context("Invalid stations search response")?;
            eprintln!("[RadioWidget][search] Got {} stations", stations.len());
            Ok(stations)
            }
        })
        .await
    }

    pub async fn resolve_station_url(&mut self, stationuuid: &str) -> Result<Url> {
        let stationuuid = stationuuid.trim();
        if stationuuid.is_empty() {
            return Err(anyhow!("Missing station UUID"));
        }

        let http = self.http.clone();
        let stationuuid = stationuuid.to_string();
        self.with_server_retry("resolve", move |base| {
            let http = http.clone();
            let stationuuid = stationuuid.clone();
            async move {
            let url = format!("{base}/json/url/{stationuuid}");
            eprintln!("[RadioWidget][resolve] GET {}", url);
            let resp = http.get(url).send().await?;
            eprintln!("[RadioWidget][resolve] Response: status = {}", resp.status());
            if resp.status().is_redirection() {
                if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
                    let loc = loc.to_str().context("Invalid redirect Location header")?;
                    eprintln!("[RadioWidget][resolve] Redirected to {}", loc);
                    return parse_stream_url(loc);
                }
            }
            let bytes = read_limited(resp, 64 * 1024).await?;
            let text = String::from_utf8_lossy(&bytes);
            eprintln!("[RadioWidget][resolve] Body: {}", &text);
            // Try to parse as JSON and extract the url field
            if let Ok(json) = serde_json::from_str::<UrlResponse>(&text) {
                eprintln!("[RadioWidget][resolve] Extracted stream URL: {}", json.url);
                return parse_stream_url(&json.url);
            }
            // fallback: try to parse as plain URL
            parse_stream_url(text.trim())
            }
        })
        .await
    }

    async fn with_server_retry<F, Fut, T>(&mut self, action: &str, mut f: F) -> Result<T>
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut servers = self.discover_servers().await?;
        servers.shuffle(&mut rand::thread_rng());
        if let Some(last) = self.last_server.clone() {
            if let Some(pos) = servers.iter().position(|s| *s == last) {
                servers.swap(0, pos);
            }
        }

        let max_attempts = 4usize;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..max_attempts {
            let server = servers
                .get(attempt % servers.len())
                .cloned()
                .unwrap_or_else(|| BOOTSTRAP_BASE.trim_start_matches("https://").to_string());
            let base = format!("https://{server}");

            match f(base.clone()).await {
                Ok(v) => {
                    self.last_server = Some(server);
                    return Ok(v);
                }
                Err(e) => {
                    last_err = Some(e.context(format!("{action} attempt {attempt} failed")));
                    let backoff_ms = 200u64.saturating_mul(2u64.saturating_pow(attempt as u32));
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow!("{action} failed")))
    }
}

#[derive(Debug, Deserialize)]
struct UrlResponse {
    url: String,
}

fn parse_stream_url(s: &str) -> Result<Url> {
    let url = Url::parse(s).context("Invalid stream URL")?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(anyhow!("Unsupported stream URL scheme: {other}")),
    }
}

async fn read_limited(resp: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len as usize > limit {
            return Err(anyhow!("HTTP response too large ({len} bytes)"));
        }
    }

    let mut data: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("HTTP body read error")?;
        if data.len().saturating_add(chunk.len()) > limit {
            return Err(anyhow!("HTTP response exceeded size limit"));
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_servers() {
        let body = r#"[{"name":"de1.api.radio-browser.info"},{"name":"fr1.api.radio-browser.info"}]"#;
        let servers: Vec<RadioBrowserServer> = serde_json::from_str(body).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "de1.api.radio-browser.info");
    }

    #[test]
    fn parses_search_results() {
        let body =
            r#"[{"stationuuid":"u1","name":"Test FM","country":"US","codec":"MP3","bitrate":128,"votes":42}]"#;
        let stations: Vec<Station> = serde_json::from_str(body).unwrap();
        assert_eq!(stations[0].stationuuid, "u1");
        assert_eq!(stations[0].bitrate, Some(128));
    }

    #[test]
    fn validates_stream_url_schemes() {
        assert!(parse_stream_url("https://example.com/stream").is_ok());
        assert!(parse_stream_url("http://example.com/stream").is_ok());
        assert!(parse_stream_url("file:///etc/passwd").is_err());
    }
}
