//! Minimal Plex client for the spike. Runs entirely on the tokio thread.

use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use tokio::sync::{Semaphore, mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct Album {
    pub rating_key: String,
    pub title: String,
    pub artist: String,
    pub year: String,
    pub thumb: String,
}

pub struct ArtRequest {
    pub thumb: String,
}

pub struct TracksRequest {
    pub album_key: String,
    pub reply: oneshot::Sender<anyhow::Result<Vec<Track>>>,
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    /// "artist" | "album" | "track"
    pub kind: String,
    pub rating_key: String,
    pub title: String,
    pub subtitle: String,
    /// Album key for a track, artist key for an album.
    pub parent_key: String,
    pub thumb: String,
}

pub struct SearchRequest {
    pub query: String,
    pub reply: oneshot::Sender<anyhow::Result<Vec<SearchHit>>>,
}

#[derive(Deserialize)]
struct HubEnvelope {
    #[serde(rename = "MediaContainer")]
    mc: HubContainer,
}

#[derive(Deserialize)]
struct HubContainer {
    #[serde(rename = "Hub", default)]
    hubs: Vec<Hub>,
}

#[derive(Deserialize)]
struct Hub {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "hubIdentifier", default)]
    id: String,
    #[serde(rename = "Metadata", default)]
    metadata: Vec<HubMeta>,
}

#[derive(Deserialize)]
struct HubMeta {
    #[serde(rename = "ratingKey", default)]
    rating_key: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "parentTitle", default)]
    parent_title: String,
    #[serde(rename = "grandparentTitle", default)]
    grandparent_title: String,
    #[serde(rename = "parentRatingKey", default)]
    parent_rating_key: String,
    #[serde(rename = "librarySectionID", default)]
    section: Option<serde_json::Value>,
    #[serde(default)]
    thumb: Option<String>,
    #[serde(default)]
    key: String,
}

#[derive(Clone, Debug)]
pub struct HubItem {
    pub rating_key: String,
    pub title: String,
    pub subtitle: String,
    pub thumb: String,
    /// Plex key (a station's play URL, an album's children); used once stations play.
    #[allow(dead_code)]
    pub key: String,
}

#[derive(Clone, Debug)]
pub struct HubRow {
    #[allow(dead_code)]
    pub id: String,
    pub title: String,
    /// "album" | "artist" | "station"
    pub kind: String,
    pub items: Vec<HubItem>,
}

pub struct HubsRequest {
    pub reply: oneshot::Sender<anyhow::Result<Vec<HubRow>>>,
}

pub struct ArtResult {
    pub thumb: String,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub token: String,
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
struct Media {
    #[serde(rename = "Part", default)]
    part: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    key: String,
}

#[derive(Clone, Debug)]
pub struct Track {
    pub rating_key: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    pub part_key: String,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(rename = "ratingKey", default)]
    rating_key: String,
    title: String,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(rename = "grandparentTitle", default)]
    grandparent_title: String,
    #[serde(rename = "Media", default)]
    media: Vec<Media>,
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
                rating_key: m.rating_key,
                title: m.title,
                artist: m.parent_title,
                year: m.year.map(|y| y.to_string()).unwrap_or_default(),
                thumb: m.thumb.unwrap_or_default(),
            })
            .collect())
    }

    pub async fn tracks(&self, album_key: &str) -> anyhow::Result<Vec<Track>> {
        let env: Envelope = self
            .get(&format!("/library/metadata/{album_key}/children"))
            .await?
            .json()
            .await
            .context("decoding tracks")?;
        Ok(env
            .mc
            .metadata
            .into_iter()
            .filter_map(|m| {
                let part_key = m.media.first()?.part.first()?.key.clone();
                Some(Track {
                    rating_key: m.rating_key,
                    title: m.title,
                    artist: m.grandparent_title,
                    duration_ms: m.duration.unwrap_or(0),
                    part_key,
                })
            })
            .collect())
    }

    pub async fn search(&self, query: &str, section: &str) -> anyhow::Result<Vec<SearchHit>> {
        let q: String = query
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        let env: HubEnvelope = self
            .get(&format!("/hubs/search?query={q}&sectionId={section}&limit=8"))
            .await?
            .json()
            .await
            .context("decoding search")?;
        let mut out = Vec::new();
        // Order: artists, albums, tracks. Plex mixes in other libraries; keep music only.
        for want in ["artist", "album", "track"] {
            for h in env.mc.hubs.iter().filter(|h| h.kind == want) {
                for m in &h.metadata {
                    let in_section = m.section.as_ref().map(|v| v.to_string().trim_matches('"') == section).unwrap_or(true);
                    if !in_section {
                        continue;
                    }
                    let subtitle = match want {
                        "artist" => "Artist".to_string(),
                        "album" => format!("Album · {}", m.parent_title),
                        _ => format!("{} · {}", m.grandparent_title, m.parent_title),
                    };
                    out.push(SearchHit {
                        kind: want.into(),
                        rating_key: m.rating_key.clone(),
                        title: m.title.clone(),
                        subtitle,
                        parent_key: m.parent_rating_key.clone(),
                        thumb: m.thumb.clone().unwrap_or_default(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// The music section's home hubs (recently played/added, stations, ...).
    pub async fn home_hubs(&self, section: &str) -> anyhow::Result<Vec<HubRow>> {
        let env: HubEnvelope = self
            .get(&format!("/hubs/sections/{section}?count=12&includeStations=1"))
            .await?
            .json()
            .await
            .context("decoding hubs")?;
        Ok(env
            .mc
            .hubs
            .into_iter()
            .filter(|h| matches!(h.kind.as_str(), "album" | "artist" | "station") && !h.metadata.is_empty())
            .map(|h| HubRow {
                id: h.id,
                title: h.title,
                kind: h.kind.clone(),
                items: h
                    .metadata
                    .into_iter()
                    .map(|m| HubItem {
                        subtitle: if h.kind == "album" { m.parent_title.clone() } else { String::new() },
                        rating_key: m.rating_key,
                        title: m.title,
                        thumb: m.thumb.unwrap_or_default(),
                        key: m.key,
                    })
                    .collect(),
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

#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: Client,
    section: String,
    albums_tx: oneshot::Sender<anyhow::Result<Vec<Album>>>,
    mut art_rx: mpsc::UnboundedReceiver<ArtRequest>,
    art_tx: mpsc::UnboundedSender<ArtResult>,
    mut tracks_rx: mpsc::UnboundedReceiver<TracksRequest>,
    mut search_rx: mpsc::UnboundedReceiver<SearchRequest>,
    mut hubs_rx: mpsc::UnboundedReceiver<HubsRequest>,
) {
    {
        let client = client.clone();
        let section = section.clone();
        tokio::spawn(async move {
            while let Some(req) = hubs_rx.recv().await {
                let _ = req.reply.send(client.home_hubs(&section).await);
            }
        });
    }
    {
        let client = client.clone();
        let section = section.clone();
        tokio::spawn(async move {
            while let Some(req) = search_rx.recv().await {
                let _ = req.reply.send(client.search(&req.query, &section).await);
            }
        });
    }
    {
        let client = client.clone();
        tokio::spawn(async move {
            while let Some(req) = tracks_rx.recv().await {
                let _ = req.reply.send(client.tracks(&req.album_key).await);
            }
        });
    }
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
