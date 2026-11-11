//! Minimal Plex client for the spike. Runs entirely on the tokio thread.

use std::sync::Arc;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, mpsc, oneshot};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub rating_key: String,
    /// The artist's rating key (Plex parentRatingKey).
    #[serde(default)]
    pub artist_key: String,
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

#[derive(Clone, Debug)]
pub struct Playlist {
    pub rating_key: String,
    pub title: String,
    pub smart: bool,
    pub count: u64,
    pub thumb: String,
}

/// Everything the shell and Home need at startup.
#[derive(Clone, Debug, Default)]
pub struct Home {
    pub rows: Vec<HubRow>,
    pub playlists: Vec<Playlist>,
    pub server: String,
    /// Most recently played album and when (unix seconds).
    pub last_played: Option<(Album, i64)>,
    /// (filter value, label) for the mood and decade chips.
    pub moods: Vec<(String, String)>,
    pub decades: Vec<(String, String)>,
    pub genres: Vec<(String, String)>,
}

/// Album page details beyond what the library list carries.
#[derive(Clone, Debug, Default)]
pub struct AlbumDetail {
    pub summary: String,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
    pub moods: Vec<String>,
    pub studio: String,
    /// originallyAvailableAt, e.g. 2016-02-19.
    pub released: String,
    pub added_at: i64,
    /// Your rating, 0–10.
    pub rating: Option<f32>,
    /// UltraBlur corner colors: top-left, top-right, bottom-right, bottom-left.
    pub blur: Option<[u32; 4]>,
    /// e.g. "FLAC 24/96", from the first track.
    pub format: String,
    /// Integrated album loudness in LUFS (ReplayGain 2 reference −18 minus album gain).
    pub lufs: Option<f32>,
    /// Track rating key -> (plays, your rating 0–10).
    pub stats: std::collections::HashMap<String, (u64, Option<f32>)>,
}

#[derive(Clone, Debug, Default)]
pub struct TrackDetail {
    pub levels: Vec<f32>,
    pub file: String,
    pub codec: String,
    pub bitrate: Option<u64>,
    pub plays: u64,
    pub last_viewed: i64,
    pub added: i64,
    pub rating: Option<f32>,
    pub moods: Vec<String>,
    /// Integrated loudness (LUFS) and ReplayGain-style track gain (dB).
    pub loudness: Option<f32>,
    pub gain: Option<f32>,
    /// (start ms, line); empty when the track has no timed lyrics.
    pub lyrics: Vec<(u64, String)>,
    pub lyrics_by: String,
}

/// `[mm:ss.xx]text` lines; tags like `[au:...]` are skipped.
pub fn parse_lrc(text: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut stamps = Vec::new();
        while let Some(r) = rest.strip_prefix('[') {
            let Some(end) = r.find(']') else { break };
            let tag = &r[..end];
            if let Some((mm, ss)) = tag.split_once(':')
                && let (Ok(m), Ok(s)) = (mm.parse::<u64>(), ss.parse::<f64>())
            {
                stamps.push(m * 60_000 + (s * 1000.0).round() as u64);
            }
            rest = &r[end + 1..];
        }
        for t in stamps {
            out.push((t, rest.trim().to_string()));
        }
    }
    out.sort_by_key(|l| l.0);
    out
}

pub struct HubsRequest {
    pub reply: oneshot::Sender<anyhow::Result<Home>>,
}

#[derive(Clone, Debug, Default)]
pub struct ArtistInfo {
    pub title: String,
    pub summary: String,
    pub thumb: String,
    /// Most-played tracks in this library: (track, album key, album title, plays).
    pub popular: Vec<(Track, String, String, u64)>,
    pub genres: Vec<String>,
    /// Plex's similar-artist names (from metadata, not sonic analysis).
    pub similar: Vec<String>,
    pub blur: Option<[u32; 4]>,
    pub plays: u64,
}

pub struct ArtistRequest {
    pub key: String,
    pub reply: oneshot::Sender<anyhow::Result<ArtistInfo>>,
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
    #[serde(rename = "playQueueID", default)]
    id: Option<u64>,
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
    pub album_key: String,
    pub album_title: String,
    /// Album cover path (same key as `Album::thumb`).
    pub thumb: String,
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
    #[serde(rename = "parentRatingKey", default)]
    parent_rating_key: String,
    #[serde(rename = "viewCount", default)]
    view_count: Option<u64>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    thumb: Option<String>,
    #[serde(rename = "parentThumb", default)]
    parent_thumb: Option<String>,
    #[serde(rename = "lastViewedAt", default)]
    last_viewed_at: Option<i64>,
}

impl Meta {
    fn into_album(self) -> Album {
        Album {
            artist_key: self.parent_rating_key,
            rating_key: self.rating_key,
            title: self.title,
            artist: self.parent_title,
            year: self.year.map(|y| y.to_string()).unwrap_or_default(),
            thumb: self.thumb.unwrap_or_default(),
        }
    }

    fn into_track(self) -> Option<Track> {
        let part_key = self.media.first()?.part.first()?.key.clone();
        Some(Track {
            rating_key: self.rating_key,
            title: self.title,
            artist: self.grandparent_title,
            duration_ms: self.duration.unwrap_or(0),
            part_key,
            album_key: self.parent_rating_key,
            album_title: self.parent_title,
            thumb: self.parent_thumb.unwrap_or_default(),
        })
    }
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
            .map(Meta::into_album)
            .collect())
    }

    /// Album rating keys matching a library filter, e.g. `decade=1990` or `mood=17889`.
    pub async fn album_keys_where(&self, section: &str, filter: &str) -> anyhow::Result<Vec<String>> {
        let env: Envelope =
            self.get(&format!("/library/sections/{section}/all?type=9&{filter}")).await?.json().await.context("filter")?;
        Ok(env.mc.metadata.into_iter().map(|m| m.rating_key).collect())
    }

    async fn last_played(&self, section: &str) -> anyhow::Result<Option<(Album, i64)>> {
        let env: Envelope = self
            .get(&format!("/library/sections/{section}/all?type=9&sort=lastViewedAt:desc&X-Plex-Container-Start=0&X-Plex-Container-Size=1"))
            .await?
            .json()
            .await
            .context("last played")?;
        Ok(env.mc.metadata.into_iter().next().and_then(|m| {
            let at = m.last_viewed_at?;
            Some((m.into_album(), at))
        }))
    }

    /// (key, title) directory entries such as `/library/sections/5/mood`.
    async fn directory(&self, path: &str) -> anyhow::Result<Vec<(String, String)>> {
        let v: serde_json::Value = self.get(path).await?.json().await.context("directory")?;
        Ok(v["MediaContainer"]["Directory"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| Some((d["key"].as_str()?.to_string(), d["title"].as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default())
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
            .filter_map(Meta::into_track)
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

    /// Start a station (or any library URI) as a Plex play queue. Creates a
    /// temporary queue on the server; it doesn't touch play counts.
    /// Returns (queue id, first window of tracks).
    pub async fn play_queue(&self, key: &str) -> anyhow::Result<(u64, Vec<Track>)> {
        let root: serde_json::Value = self.get("/").await?.json().await.context("server")?;
        let mid = root["MediaContainer"]["machineIdentifier"].as_str().unwrap_or_default();
        let uri = urlencode(&format!("server://{mid}/com.plexapp.plugins.library{key}"));
        let res = self
            .http
            .post(format!("{}/playQueues?type=audio&uri={uri}&repeat=0&own=1&includeChapters=0", self.base))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Product", "Aulos")
            .header("X-Plex-Client-Identifier", "aulos-spike")
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "playQueues -> {}", res.status());
        let env: Envelope = res.json().await.context("play queue")?;
        Ok((env.mc.id.unwrap_or(0), env.mc.metadata.into_iter().filter_map(Meta::into_track).collect()))
    }

    /// The first `limit` tracks of a playlist.
    pub async fn playlist_tracks(&self, rating_key: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        let res = self
            .http
            .get(format!("{}/playlists/{rating_key}/items", self.base))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Container-Start", "0")
            .header("X-Plex-Container-Size", limit.to_string())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "playlist {rating_key} -> {}", res.status());
        let env: Envelope = res.json().await.context("playlist")?;
        Ok(env.mc.metadata.into_iter().filter_map(Meta::into_track).collect())
    }

    pub async fn album_detail(&self, key: &str) -> anyhow::Result<AlbumDetail> {
        let (mp, kp) = (format!("/library/metadata/{key}"), format!("/library/metadata/{key}/children"));
        let (meta, kids) = tokio::join!(self.get(&mp), self.get(&kp));
        let meta: serde_json::Value = meta?.json().await.context("album")?;
        let kids: serde_json::Value = kids?.json().await.context("album tracks")?;
        let m = &meta["MediaContainer"]["Metadata"][0];
        let tags = |k: &str| -> Vec<String> {
            m[k].as_array().map(|a| a.iter().filter_map(|t| t["tag"].as_str().map(String::from)).collect()).unwrap_or_default()
        };
        let hex = |k: &str| u32::from_str_radix(m["UltraBlurColors"][k].as_str()?, 16).ok();
        let blur = (|| Some([hex("topLeft")?, hex("topRight")?, hex("bottomRight")?, hex("bottomLeft")?]))();
        let tracks = kids["MediaContainer"]["Metadata"].as_array().cloned().unwrap_or_default();
        let stats = tracks
            .iter()
            .filter_map(|t| {
                Some((t["ratingKey"].as_str()?.to_string(), (t["viewCount"].as_u64().unwrap_or(0), t["userRating"].as_f64().map(|r| r as f32))))
            })
            .collect();
        let mut d = AlbumDetail {
            summary: m["summary"].as_str().unwrap_or_default().replace("\\&", "&"),
            genres: tags("Genre"),
            styles: tags("Style"),
            moods: tags("Mood"),
            studio: m["studio"].as_str().unwrap_or_default().to_string(),
            released: m["originallyAvailableAt"].as_str().unwrap_or_default().to_string(),
            added_at: m["addedAt"].as_i64().unwrap_or(0),
            rating: m["userRating"].as_f64().map(|r| r as f32),
            blur,
            stats,
            ..Default::default()
        };
        // Format and loudness from the first track's audio stream.
        if let Some(first) = tracks.first().and_then(|t| t["ratingKey"].as_str()) {
            let v: serde_json::Value = self.get(&format!("/library/metadata/{first}")).await?.json().await.context("track")?;
            let streams = v["MediaContainer"]["Metadata"][0]["Media"][0]["Part"][0]["Stream"].as_array().cloned().unwrap_or_default();
            if let Some(st) = streams.iter().find(|s| s["streamType"].as_i64() == Some(2)) {
                let codec = st["codec"].as_str().unwrap_or_default().to_uppercase();
                let rate = st["samplingRate"].as_u64().unwrap_or(0);
                let khz = if rate % 1000 == 0 { format!("{}", rate / 1000) } else { format!("{:.1}", rate as f64 / 1000.0) };
                d.format = match st["bitDepth"].as_u64() {
                    Some(b) => format!("{codec} {b}/{khz}"),
                    None => format!("{codec} {khz} kHz"),
                };
                d.lufs = st["albumGain"].as_str().and_then(|g| g.parse::<f32>().ok()).map(|g| -18.0 - g);
            }
        }
        Ok(d)
    }

    /// Everything Now Playing shows about one track: loudness envelope
    /// (0..1 over `points`), file, plays, loudness and timed lyrics.
    pub async fn track_detail(&self, rating_key: &str, points: usize) -> anyhow::Result<TrackDetail> {
        let v: serde_json::Value = self.get(&format!("/library/metadata/{rating_key}")).await?.json().await.context("track")?;
        let m = &v["MediaContainer"]["Metadata"][0];
        let media = &m["Media"][0];
        let part = &media["Part"][0];
        let streams = part["Stream"].as_array().cloned().unwrap_or_default();
        let audio = streams.iter().find(|s| s["streamType"].as_i64() == Some(2));
        let num = |s: &serde_json::Value, k: &str| s[k].as_str().and_then(|x| x.parse::<f32>().ok()).or(s[k].as_f64().map(|f| f as f32));
        let mut d = TrackDetail {
            file: part["file"].as_str().unwrap_or_default().to_string(),
            codec: media["audioCodec"].as_str().unwrap_or_default().to_uppercase(),
            bitrate: media["bitrate"].as_u64(),
            plays: m["viewCount"].as_u64().unwrap_or(0),
            last_viewed: m["lastViewedAt"].as_i64().unwrap_or(0),
            added: m["addedAt"].as_i64().unwrap_or(0),
            rating: m["userRating"].as_f64().map(|r| r as f32),
            moods: m["Mood"].as_array().map(|a| a.iter().filter_map(|t| t["tag"].as_str().map(String::from)).collect()).unwrap_or_default(),
            loudness: audio.and_then(|a| num(a, "loudness")),
            gain: audio.and_then(|a| num(a, "gain")),
            ..Default::default()
        };
        if let Some(id) = audio.and_then(|s| s["id"].as_i64())
            && let Ok(res) = self.get(&format!("/library/streams/{id}/levels?subsample={points}")).await
            && let Ok(l) = res.json::<serde_json::Value>().await
        {
            let db: Vec<f64> = l["MediaContainer"]["Level"].as_array().map(|a| a.iter().filter_map(|x| x["v"].as_f64()).collect()).unwrap_or_default();
            if let Some(max) = db.iter().cloned().reduce(f64::max) {
                // Show the top 24 dB of the track's own range.
                let floor = max - 24.0;
                d.levels = db.iter().map(|&x| (((x - floor) / (max - floor)).clamp(0.0, 1.0)) as f32).collect();
            }
        }
        // Timed lyrics: a streamType 4 LRC stream (LyricFind via the server agent).
        if let Some(key) = streams.iter().find(|s| s["streamType"].as_i64() == Some(4)).and_then(|s| s["key"].as_str())
            && let Ok(res) = self.http.get(format!("{}{key}", self.base)).header("X-Plex-Token", &self.token).send().await
            && res.status().is_success()
            && let Ok(text) = res.text().await
        {
            d.lyrics = parse_lrc(&text);
            d.lyrics_by = streams.iter().find(|s| s["streamType"].as_i64() == Some(4)).and_then(|s| s["provider"].as_str()).unwrap_or_default().to_string();
        }
        Ok(d)
    }

    /// Audio playlists, most recently played first.
    pub async fn playlists(&self) -> anyhow::Result<Vec<Playlist>> {
        let v: serde_json::Value =
            self.get("/playlists?playlistType=audio&sort=lastViewedAt:desc").await?.json().await.context("playlists")?;
        let items = v["MediaContainer"]["Metadata"].as_array().cloned().unwrap_or_default();
        Ok(items
            .iter()
            .map(|m| Playlist {
                rating_key: m["ratingKey"].as_str().unwrap_or_default().to_string(),
                title: m["title"].as_str().unwrap_or_default().to_string(),
                smart: m["smart"].as_bool().unwrap_or(false),
                count: m["leafCount"].as_u64().unwrap_or(0),
                thumb: m["composite"].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    pub async fn server_name(&self) -> anyhow::Result<String> {
        let v: serde_json::Value = self.get("/").await?.json().await.context("server")?;
        Ok(v["MediaContainer"]["friendlyName"].as_str().unwrap_or("Plex").to_string())
    }

    pub async fn home(&self, section: &str) -> anyhow::Result<Home> {
        let (mood_path, decade_path, genre_path) = (
            format!("/library/sections/{section}/mood"),
            format!("/library/sections/{section}/decade"),
            format!("/library/sections/{section}/genre"),
        );
        let (rows, playlists, server, last, moods, decades, genres) = tokio::join!(
            self.home_hubs(section),
            self.playlists(),
            self.server_name(),
            self.last_played(section),
            self.directory(&mood_path),
            self.directory(&decade_path),
            self.directory(&genre_path),
        );
        // The canvas's six moods, when the library has them.
        const MOODS: &[&str] = &["Wistful", "Brooding", "Confident", "Playful", "Cathartic", "Swaggering"];
        let moods = moods.unwrap_or_default();
        let moods = MOODS
            .iter()
            .filter_map(|w| moods.iter().find(|(_, t)| t == w))
            .map(|(k, t)| (format!("mood={k}"), t.clone()))
            .collect();
        let mut decades: Vec<(String, String)> = decades
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| k.parse::<u32>().is_ok_and(|y| y >= 1960))
            .map(|(k, t)| (format!("decade={k}"), t.get(2..).unwrap_or(&t).to_string()))
            .collect();
        decades.sort();
        Ok(Home {
            rows: rows?,
            playlists: playlists.unwrap_or_default(),
            server: server.unwrap_or_else(|_| "Plex".into()),
            last_played: last.ok().flatten(),
            moods,
            decades,
            genres: genres.unwrap_or_default().into_iter().map(|(k, t)| (format!("genre={k}"), t)).collect(),
        })
    }

    pub async fn artist(&self, key: &str, section: &str) -> anyhow::Result<ArtistInfo> {
        let meta: serde_json::Value = self.get(&format!("/library/metadata/{key}")).await?.json().await.context("artist")?;
        let m = &meta["MediaContainer"]["Metadata"][0];
        anyhow::ensure!(m.is_object(), "no such artist");
        let tags = |k: &str| -> Vec<String> {
            m[k].as_array().map(|a| a.iter().filter_map(|t| t["tag"].as_str().map(String::from)).collect()).unwrap_or_default()
        };
        let hex = |k: &str| u32::from_str_radix(m["UltraBlurColors"][k].as_str()?, 16).ok();
        let top: Envelope = self
            .get(&format!(
                "/library/sections/{section}/all?type=10&artist.id={key}&sort=viewCount:desc&X-Plex-Container-Start=0&X-Plex-Container-Size=10"
            ))
            .await?
            .json()
            .await
            .context("artist tracks")?;
        let popular = top
            .mc
            .metadata
            .into_iter()
            .filter_map(|t| {
                let plays = t.view_count.unwrap_or(0);
                let (key, title) = (t.parent_rating_key.clone(), t.parent_title.clone());
                Some((t.into_track()?, key, title, plays))
            })
            .collect();
        Ok(ArtistInfo {
            title: m["title"].as_str().unwrap_or_default().to_string(),
            summary: m["summary"].as_str().unwrap_or_default().replace("\\&", "&"),
            thumb: m["thumb"].as_str().unwrap_or_default().to_string(),
            popular,
            genres: tags("Genre"),
            similar: tags("Similar"),
            blur: (|| Some([hex("topLeft")?, hex("topRight")?, hex("bottomRight")?, hex("bottomLeft")?]))(),
            plays: m["viewCount"].as_u64().unwrap_or(0),
        })
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

/// ~/.cache/aulos (or $XDG_CACHE_HOME/aulos).
pub fn cache_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache"));
    base.join("aulos")
}

fn art_cache_path(thumb: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    thumb.hash(&mut h);
    cache_dir().join("art").join(format!("{:016x}.jpg", h.finish()))
}

/// Write via a temp file + rename so a crash never leaves a torn file.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: Client,
    section: String,
    albums_tx: mpsc::UnboundedSender<(&'static str, anyhow::Result<Vec<Album>>)>,
    mut art_rx: mpsc::UnboundedReceiver<ArtRequest>,
    art_tx: mpsc::UnboundedSender<ArtResult>,
    mut tracks_rx: mpsc::UnboundedReceiver<TracksRequest>,
    mut search_rx: mpsc::UnboundedReceiver<SearchRequest>,
    mut hubs_rx: mpsc::UnboundedReceiver<HubsRequest>,
    mut artist_rx: mpsc::UnboundedReceiver<ArtistRequest>,
) {
    {
        let client = client.clone();
        let section = section.clone();
        tokio::spawn(async move {
            while let Some(req) = artist_rx.recv().await {
                let _ = req.reply.send(client.artist(&req.key, &section).await);
            }
        });
    }
    {
        let client = client.clone();
        let section = section.clone();
        tokio::spawn(async move {
            while let Some(req) = hubs_rx.recv().await {
                let _ = req.reply.send(client.home(&section).await);
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
    // Cache first: paint from disk, then refresh from the server.
    let lib_cache = cache_dir().join(format!("albums-{section}.json"));
    let t = std::time::Instant::now();
    if let Ok(bytes) = std::fs::read(&lib_cache)
        && let Ok(cached) = serde_json::from_slice::<Vec<Album>>(&bytes)
    {
        eprintln!("[aulos] cached library: {} albums in {:?}", cached.len(), t.elapsed());
        let _ = albums_tx.send(("cache", Ok(cached)));
    }
    let t = std::time::Instant::now();
    let albums = client.albums(&section).await;
    if let Ok(a) = &albums {
        eprintln!("[aulos] fetched {} albums in {:?}", a.len(), t.elapsed());
        if let Ok(json) = serde_json::to_vec(a) {
            let _ = write_atomic(&lib_cache, &json);
        }
    }
    let _ = albums_tx.send(("plex", albums));

    let limit = Arc::new(Semaphore::new(8));
    while let Some(req) = art_rx.recv().await {
        let client = client.clone();
        let tx = art_tx.clone();
        let permit = limit.clone().acquire_owned().await.unwrap();
        tokio::spawn(async move {
            let path = art_cache_path(&req.thumb);
            let bytes = match tokio::fs::read(&path).await {
                Ok(b) if !b.is_empty() => Ok(b),
                _ => {
                    let r = client.art(&req.thumb).await;
                    if let Ok(b) = &r {
                        let _ = write_atomic(&path, b);
                    }
                    r
                }
            };
            if let Err(e) = &bytes {
                eprintln!("[aulos] art {}: {e:#}", req.thumb);
            }
            let _ = tx.send(ArtResult { thumb: req.thumb, bytes: bytes.ok() });
            drop(permit);
        });
    }
}
