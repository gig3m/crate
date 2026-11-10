//! Minimal Plex client for the spike. Runs entirely on the tokio thread.

use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use tokio::sync::{Semaphore, mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct Album {
    pub title: String,
    pub artist: String,
    pub year: String,
    pub thumb: String,
}

pub struct ArtRequest {
    pub thumb: String,
}

pub struct ArtResult {
    pub thumb: String,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "MediaContainer")]
    mc: Container,
}

#[derive(Deserialize)]
struct Container {
    #[serde(rename = "Metadata", default)]
    metadata: Vec<Meta>,
}

#[derive(Deserialize)]
struct Meta {
    title: String,
    #[serde(rename = "parentTitle", default)]
    parent_title: String,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    thumb: Option<String>,
}

impl Client {
    pub fn new(base: String, token: String) -> Self {
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(16)
            .build()
            .unwrap();
        Self { base, token, http }
    }

    async fn get(&self, path: &str) -> anyhow::Result<reqwest::Response> {
        let res = self
            .http
            .get(format!("{}{}", self.base, path))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Product", "Aulos")
            .header("X-Plex-Client-Identifier", "aulos-spike")
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "{} -> {}", path, res.status());
        Ok(res)
    }

    pub async fn albums(&self, section: &str) -> anyhow::Result<Vec<Album>> {
        let env: Envelope = self
            .get(&format!("/library/sections/{section}/all?type=9&sort=addedAt:desc"))
            .await?
            .json()
            .await
            .context("decoding albums")?;
        Ok(env
            .mc
            .metadata
            .into_iter()
            .map(|m| Album {
                title: m.title,
                artist: m.parent_title,
                year: m.year.map(|y| y.to_string()).unwrap_or_default(),
                thumb: m.thumb.unwrap_or_default(),
            })
            .collect())
    }

    pub async fn art(&self, thumb: &str) -> anyhow::Result<Vec<u8>> {
        // Server-side resize to the size we draw at (2x for HiDPI).
        let url = format!(
            "/photo/:/transcode?width=368&height=368&minSize=1&upscale=1&url={}",
            urlencode(thumb)
        );
        Ok(self.get(&url).await?.bytes().await?.to_vec())
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub async fn run(
    client: Client,
    section: String,
    albums_tx: oneshot::Sender<anyhow::Result<Vec<Album>>>,
    mut art_rx: mpsc::UnboundedReceiver<ArtRequest>,
    art_tx: mpsc::UnboundedSender<ArtResult>,
) {
    let t = std::time::Instant::now();
    let albums = client.albums(&section).await;
    if let Ok(a) = &albums {
        eprintln!("[aulos] fetched {} albums in {:?}", a.len(), t.elapsed());
    }
    let _ = albums_tx.send(albums);

    let limit = Arc::new(Semaphore::new(8));
    while let Some(req) = art_rx.recv().await {
        let client = client.clone();
        let tx = art_tx.clone();
        let permit = limit.clone().acquire_owned().await.unwrap();
        tokio::spawn(async move {
            let bytes = client.art(&req.thumb).await;
            if let Err(e) = &bytes {
                eprintln!("[aulos] art {}: {e:#}", req.thumb);
            }
            let _ = tx.send(ArtResult { thumb: req.thumb, bytes: bytes.ok() });
            drop(permit);
        });
    }
}
