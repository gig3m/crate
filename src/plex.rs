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
    /// Short format for Recently Added cards ("24/96", "FLAC", "DSD64"); empty when unknown.
    pub format: String,
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

#[derive(Clone, Debug, Default)]
pub struct PlaylistDetail {
    pub title: String,
    pub smart: bool,
    /// Items in the playlist (may exceed `tracks` when capped).
    pub count: u64,
    pub duration_ms: u64,
    pub thumb: String,
    pub tracks: Vec<Track>,
    /// Plex playlist item id per track, same order.
    pub item_ids: Vec<u64>,
    /// Items fetched, playable or not (to tell a cap from missing files).
    pub loaded: usize,
    /// The fetch hit its limit: there may be items past the last row.
    pub capped: bool,
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
    /// Mood mixes for "Mixes for you": (title, description, track filter,
    /// (thumb, album key) covers of albums with those moods).
    pub mixes: Vec<MoodMix>,
}

/// (title, description, track filter, (thumb, album key) covers).
pub type MoodMix = (String, String, String, Vec<(String, String)>);

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

/// The canvas's card format: bit depth/kHz for hi-res ("24/96", "24/44.1"),
/// DSD by its multiple of 44.1 kHz ("DSD64"), otherwise the codec ("FLAC", "MP3").
pub fn short_format(codec: &str, bit_depth: Option<u64>, rate: Option<u64>) -> String {
    let codec = codec.to_ascii_lowercase();
    let rate = rate.unwrap_or(0);
    if codec.starts_with("dsd") {
        // Only a native DSD bit rate names the multiple unambiguously; reported
        // PCM-equivalent rates differ by convention (/8, /16, /32).
        return if rate >= 2_822_400 && rate.is_multiple_of(44_100) { format!("DSD{}", rate / 44_100) } else { "DSD".into() };
    }
    match bit_depth {
        Some(b) if rate > 0 && (b > 16 || rate > 48_000) => {
            let khz = if rate.is_multiple_of(1000) { format!("{}", rate / 1000) } else { format!("{}", (rate as f64 / 100.0).floor() / 10.0) };
            format!("{b}/{khz}")
        }
        _ => codec.to_uppercase(),
    }
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

#[derive(Clone, Debug)]
pub struct HistoryItem {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_key: String,
    pub rating_key: String,
    pub viewed_at: i64,
    pub thumb: String,
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

/// Loudness analysis for one track (Plex's audio stream fields). Gains are
/// dB relative to ReplayGain 2's -18 LUFS; peaks are linear sample peaks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gain {
    pub track_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

#[derive(Clone, Debug, Default)]
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
    /// Plex playlist item id when this came from a playlist (0 otherwise):
    /// tells duplicate tracks in one playlist apart.
    pub playlist_item: u64,
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
            playlist_item: 0,
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
            .header("X-Plex-Product", "Crate")
            .header("X-Plex-Client-Identifier", client_id())
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
                        format: String::new(),
                    })
                    .collect(),
            })
            .collect())
    }

    /// Four random covers of albums matching each mood mix's filter, in order
    /// (empty for a filter no album matches). Fetched after Home shows.
    pub async fn mix_covers(&self, section: &str, filters: Vec<String>) -> anyhow::Result<Vec<Vec<(String, String)>>> {
        let mut set = tokio::task::JoinSet::new();
        for (i, q) in filters.iter().enumerate() {
            let (c, q, section) = (self.clone(), q.clone(), section.to_string());
            set.spawn(async move {
                let r = c
                    .http
                    .get(format!("{}/library/sections/{section}/all?type=9&{q}&sort=random", c.base))
                    .header("Accept", "application/json")
                    .header("X-Plex-Token", &c.token)
                    .header("X-Plex-Container-Start", "0")
                    .header("X-Plex-Container-Size", "4")
                    .send()
                    .await;
                let covers: Vec<(String, String)> = match r {
                    Ok(r) => r
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|v| v["MediaContainer"]["Metadata"].as_array().cloned())
                        .unwrap_or_default()
                        .iter()
                        .take(4)
                        .filter_map(|m| Some((m["thumb"].as_str()?.to_string(), m["ratingKey"].as_str()?.to_string())))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                (i, covers)
            });
        }
        let mut out = vec![Vec::new(); filters.len()];
        while let Some(joined) = set.join_next().await {
            if let Ok((i, covers)) = joined {
                out[i] = covers;
            }
        }
        Ok(out)
    }

    /// Edition facts for album pages: (album key, track count, card format).
    /// Track counts come only with per-album metadata (not the library list).
    pub async fn edition_info(&self, keys: Vec<String>) -> anyhow::Result<Vec<(String, u32, String)>> {
        let v: serde_json::Value = self.get(&format!("/library/metadata/{}", keys.join(","))).await?.json().await.context("editions")?;
        let counts: std::collections::HashMap<String, u32> = v["MediaContainer"]["Metadata"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| Some((m["ratingKey"].as_str()?.to_string(), m["leafCount"].as_u64().unwrap_or(0) as u32)))
            .collect();
        let formats: std::collections::HashMap<String, String> = self.album_formats(keys.clone()).await.unwrap_or_default().into_iter().collect();
        Ok(keys.into_iter().map(|k| (k.clone(), counts.get(&k).copied().unwrap_or(0), formats.get(&k).cloned().unwrap_or_default())).collect())
    }

    /// Card formats for these albums, as (album key, format), fetched
    /// concurrently after Home is up. Albums that don't answer within 10 s
    /// (or fail) are left out.
    pub async fn album_formats(&self, keys: Vec<String>) -> anyhow::Result<Vec<(String, String)>> {
        let mut set = tokio::task::JoinSet::new();
        for key in keys {
            let c = self.clone();
            set.spawn(async move {
                let f = c.album_format_short(&key).await;
                (key, f)
            });
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut out = Vec::new();
        while let Ok(Some(joined)) = tokio::time::timeout_at(deadline, set.join_next()).await {
            if let Ok((key, Ok(f))) = joined {
                out.push((key, f));
            }
        }
        set.abort_all();
        if std::env::var_os("CRATE_TRACE").is_some() {
            eprintln!("recent formats: {out:?}");
        }
        Ok(out)
    }

    /// Loudness analysis for these tracks, from batched metadata requests.
    /// Tracks Plex hasn't analyzed come back as `Gain::default()`.
    pub async fn track_gains(&self, keys: Vec<String>) -> anyhow::Result<Vec<(String, Gain)>> {
        let num = |v: &serde_json::Value| v.as_str().and_then(|x| x.parse::<f32>().ok()).or(v.as_f64().map(|f| f as f32)).filter(|f| f.is_finite());
        let mut out = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(50) {
            // A failed batch loses only its own tracks (they play at unity).
            let v: serde_json::Value = match async { anyhow::Ok(self.get(&format!("/library/metadata/{}", chunk.join(","))).await?.json().await?) }.await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[crate] loudness batch: {e:#}");
                    continue;
                }
            };
            let mut answered = std::collections::HashSet::new();
            for m in v["MediaContainer"]["Metadata"].as_array().into_iter().flatten() {
                let Some(key) = m["ratingKey"].as_str() else { continue };
                answered.insert(key.to_string());
                let streams = m["Media"][0]["Part"][0]["Stream"].as_array().cloned().unwrap_or_default();
                let g = streams.iter().find(|s| s["streamType"].as_i64() == Some(2)).map(|a| Gain {
                    // Plex's per-track `gain` repeats the album's; derive the track's from its loudness.
                    track_db: num(&a["loudness"]).map(|l| -18.0 - l),
                    track_peak: num(&a["peak"]),
                    album_db: num(&a["albumGain"]),
                    album_peak: num(&a["albumPeak"]),
                });
                out.push((key.to_string(), g.unwrap_or_default()));
            }
            // Keys Plex didn't return (deleted, unknown) count as known-unanalyzed,
            // so they aren't refetched on every start.
            out.extend(chunk.iter().filter(|k| !answered.contains(*k)).map(|k| (k.clone(), Gain::default())));
        }
        Ok(out)
    }

    /// An album's format, read from its first track's audio stream.
    pub async fn album_format_short(&self, album_key: &str) -> anyhow::Result<String> {
        let env: serde_json::Value = self
            .get(&format!("/library/metadata/{album_key}/children?X-Plex-Container-Start=0&X-Plex-Container-Size=1"))
            .await?
            .json()
            .await
            .context("tracks")?;
        let first = env["MediaContainer"]["Metadata"][0]["ratingKey"].as_str().context("no tracks")?.to_string();
        let v: serde_json::Value = self.get(&format!("/library/metadata/{first}")).await?.json().await.context("track")?;
        let streams = v["MediaContainer"]["Metadata"][0]["Media"][0]["Part"][0]["Stream"].as_array().cloned().unwrap_or_default();
        let st = streams.iter().find(|s| s["streamType"].as_i64() == Some(2)).context("no audio stream")?;
        Ok(short_format(st["codec"].as_str().unwrap_or_default(), st["bitDepth"].as_u64(), st["samplingRate"].as_u64()))
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
            .header("X-Plex-Product", "Crate")
            .header("X-Plex-Client-Identifier", client_id())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "playQueues -> {}", res.status());
        let env: Envelope = res.json().await.context("play queue")?;
        Ok((env.mc.id.unwrap_or(0), env.mc.metadata.into_iter().filter_map(Meta::into_track).collect()))
    }

    fn write_req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()).unwrap_or_else(|_| "Linux".into());
        self.http
            .request(method, format!("{}{path}", self.base))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Product", "Crate")
            .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
            .header("X-Plex-Client-Identifier", client_id())
            .header("X-Plex-Platform", "Linux")
            .header("X-Plex-Device-Name", host)
    }

    /// Report playback to the server (`state` = playing | paused | stopped).
    /// The server shows it as a session, and records a play (and any Last.fm
    /// scrobble) when a track stops past ~90%.
    pub async fn timeline(&self, rating_key: &str, state: &str, time_ms: u64, duration_ms: u64) -> anyhow::Result<()> {
        let path = format!(
            "/:/timeline?ratingKey={rating_key}&key=%2Flibrary%2Fmetadata%2F{rating_key}&state={state}&time={time_ms}&duration={duration_ms}"
        );
        let res = self.write_req(reqwest::Method::GET, &path).send().await?;
        anyhow::ensure!(res.status().is_success(), "timeline -> {}", res.status());
        Ok(())
    }

    /// Set your rating (0–10) on a track or album, or clear it with None.
    pub async fn rate(&self, rating_key: &str, rating: Option<u8>) -> anyhow::Result<()> {
        let r = rating.map(|r| r.min(10) as i32).unwrap_or(-1);
        let path = format!("/:/rate?key={rating_key}&identifier=com.plexapp.plugins.library&rating={r}");
        let res = self.write_req(reqwest::Method::PUT, &path).send().await?;
        anyhow::ensure!(res.status().is_success(), "rate -> {}", res.status());
        Ok(())
    }

    /// Download a part (original file) to `dest` via a temp file, counting
    /// bytes into `progress`. Stops early (Err) when `cancel` is set.
    pub async fn download(
        &self,
        part_key: &str,
        dest: &std::path::Path,
        progress: std::sync::Arc<std::sync::atomic::AtomicU64>,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<u64> {
        use std::sync::atomic::Ordering;
        use tokio::io::AsyncWriteExt;
        let send = self
            .http
            .get(format!("{}{part_key}", self.base))
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", client_id())
            .send();
        let mut res = tokio::time::timeout(std::time::Duration::from_secs(30), send).await.map_err(|_| anyhow::anyhow!("server didn't answer"))??.error_for_status()?;
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        // Read before the body: reqwest reports what's left once chunks are consumed.
        let expected = res.content_length();
        let tmp = partial_path(dest);
        let mut f = tokio::fs::File::create(&tmp).await?;
        let mut n = 0u64;
        let r: anyhow::Result<()> = async {
            // Wait for each chunk in 1 s slices so cancel takes effect even when
            // the server stalls; 30 s with no data is a failure.
            let mut idle = 0;
            loop {
                anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
                let chunk = match tokio::time::timeout(std::time::Duration::from_secs(1), res.chunk()).await {
                    Err(_) => {
                        idle += 1;
                        anyhow::ensure!(idle < 30, "the server stopped sending data");
                        continue;
                    }
                    Ok(c) => match c? {
                        Some(c) => c,
                        None => break,
                    },
                };
                idle = 0;
                f.write_all(&chunk).await?;
                n += chunk.len() as u64;
                progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            }
            if let Some(len) = expected {
                anyhow::ensure!(n == len, "short download ({n} of {len} bytes)");
            }
            f.flush().await?;
            f.sync_all().await?;
            Ok(())
        }
        .await;
        drop(f);
        match r {
            Ok(()) => {
                tokio::fs::rename(&tmp, dest).await?;
                Ok(n)
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                Err(e)
            }
        }
    }

    /// `n` random tracks matching a filter (`genre=..`, `mood=..`, `album.decade=..`).
    pub async fn random_tracks(&self, section: &str, filter: &str, n: usize) -> anyhow::Result<Vec<Track>> {
        let res = self
            .http
            .get(format!("{}/library/sections/{section}/all?type=10&{filter}&sort=random", self.base))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Container-Start", "0")
            .header("X-Plex-Container-Size", n.to_string())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "random tracks -> {}", res.status());
        let env: Envelope = res.json().await.context("random tracks")?;
        Ok(env.mc.metadata.into_iter().filter_map(Meta::into_track).collect())
    }

    /// Progress (percent) of the server's sonic analysis, if it's running.
    pub async fn sonic_progress(&self) -> anyhow::Result<Option<u32>> {
        let v: serde_json::Value = self.get("/activities").await?.json().await.context("activities")?;
        Ok(v["MediaContainer"]["Activity"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["type"].as_str() == Some("media.generate.music.analysis")))
            .and_then(|x| x["progress"].as_u64())
            .map(|p| p as u32))
    }

    /// Recent plays from every client: (title, artist, album title, album key,
    /// track key, viewed at, album cover).
    pub async fn history(&self, section: &str, n: usize) -> anyhow::Result<Vec<HistoryItem>> {
        let res = self
            .http
            .get(format!("{}/status/sessions/history/all?librarySectionID={section}&sort=viewedAt:desc", self.base))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Container-Start", "0")
            .header("X-Plex-Container-Size", n.to_string())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "history -> {}", res.status());
        let v: serde_json::Value = res.json().await.context("history")?;
        Ok(v["MediaContainer"]["Metadata"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|m| HistoryItem {
                        title: m["title"].as_str().unwrap_or_default().into(),
                        artist: m["grandparentTitle"].as_str().unwrap_or_default().into(),
                        album: m["parentTitle"].as_str().unwrap_or_default().into(),
                        album_key: m["parentKey"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default().into(),
                        rating_key: m["ratingKey"].as_str().unwrap_or_default().into(),
                        viewed_at: m["viewedAt"].as_i64().unwrap_or(0),
                        thumb: m["parentThumb"].as_str().unwrap_or_default().into(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The library's most played tracks with their play counts.
    pub async fn top_tracks(&self, section: &str, n: usize) -> anyhow::Result<Vec<(Track, u64)>> {
        let res = self
            .http
            .get(format!("{}/library/sections/{section}/all?type=10&sort=viewCount:desc", self.base))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Container-Start", "0")
            .header("X-Plex-Container-Size", n.to_string())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "top tracks -> {}", res.status());
        let env: Envelope = res.json().await.context("top tracks")?;
        Ok(env
            .mc
            .metadata
            .into_iter()
            .filter_map(|m| {
                let plays = m.view_count.unwrap_or(0);
                Some((m.into_track()?, plays))
            })
            .filter(|(_, p)| *p > 0)
            .collect())
    }

    /// An artist's albums by kind, from the library's Format (EP, Single) and
    /// Subformat (Live) tags: (singles & EPs, live) as album keys.
    pub async fn artist_album_kinds(
        &self,
        section: &str,
        artist_key: &str,
    ) -> anyhow::Result<(std::collections::HashSet<String>, std::collections::HashSet<String>)> {
        let fp = format!("/library/sections/{section}/format?type=9");
        let sp = format!("/library/sections/{section}/subformat?type=9");
        let (formats, subformats) = tokio::join!(self.directory(&fp), self.directory(&sp));
        let pick = |dir: &[(String, String)], names: &[&str]| -> Vec<String> {
            dir.iter().filter(|(_, t)| names.contains(&t.as_str())).map(|(k, _)| k.clone()).collect()
        };
        // Errors propagate: a failed lookup must not be cached as "no EPs".
        let small = pick(&formats?, &["EP", "Single"]);
        let live = pick(&subformats?, &["Live"]);
        let keys = |filter: String| async move {
            if filter.is_empty() {
                return Ok::<_, anyhow::Error>(std::collections::HashSet::new());
            }
            Ok(self.album_keys_where(section, &format!("artist.id={artist_key}&{filter}")).await?.into_iter().collect())
        };
        let small_f = if small.is_empty() { String::new() } else { format!("format={}", small.join(",")) };
        let live_f = if live.is_empty() { String::new() } else { format!("subformat={}", live.join(",")) };
        let (a, b) = tokio::join!(keys(small_f), keys(live_f));
        Ok((a?, b?))
    }

    /// Sonically nearest items to a track, album or artist (needs the
    /// server's sonic analysis). Distances run 0 (same) upward.
    async fn nearest_meta(&self, key: &str, limit: usize) -> anyhow::Result<Vec<Meta>> {
        let env: Envelope =
            self.get(&format!("/library/metadata/{key}/nearest?limit={limit}&maxDistance=0.3")).await?.json().await.context("nearest")?;
        Ok(env.mc.metadata)
    }

    pub async fn nearest_tracks(&self, key: &str, limit: usize) -> anyhow::Result<Vec<Track>> {
        Ok(self.nearest_meta(key, limit).await?.into_iter().filter_map(Meta::into_track).collect())
    }

    pub async fn nearest_albums(&self, key: &str, limit: usize) -> anyhow::Result<Vec<Album>> {
        Ok(self.nearest_meta(key, limit).await?.into_iter().map(Meta::into_album).collect())
    }

    /// (artist key, name, photo)
    pub async fn nearest_artists(&self, key: &str, limit: usize) -> anyhow::Result<Vec<(String, String, String)>> {
        Ok(self.nearest_meta(key, limit).await?.into_iter().map(|m| (m.rating_key, m.title, m.thumb.unwrap_or_default())).collect())
    }

    /// Sonic Adventure: a path of tracks from `start` to `end`, step by sonic step.
    pub async fn sonic_path(&self, section: &str, start: &str, end: &str) -> anyhow::Result<Vec<Track>> {
        let env: Envelope = self
            .get(&format!("/library/sections/{section}/computePath?startID={start}&endID={end}"))
            .await?
            .json()
            .await
            .context("sonic path")?;
        Ok(env.mc.metadata.into_iter().filter_map(Meta::into_track).collect())
    }

    /// Another sonic path between the same tracks. Plex's computePath is
    /// deterministic, so this detours through a waypoint: a track sonically
    /// near the middle of `current` that isn't on it, joining start→waypoint
    /// and waypoint→end. Errs if no waypoint gives a path.
    pub async fn sonic_path_reroll(&self, section: &str, start: &str, end: &str, current: Vec<String>) -> anyhow::Result<Vec<Track>> {
        anyhow::ensure!(current.len() >= 3, "path too short to reroll");
        let mid = current[current.len() / 2].clone();
        let mut near: Vec<Track> = self.nearest_tracks(&mid, 40).await?.into_iter().filter(|t| !current.contains(&t.rating_key)).collect();
        anyhow::ensure!(!near.is_empty(), "nothing near the path to detour through");
        crate::shuffle(&mut near);
        let mut last_err = None;
        for w in near.iter().take(3) {
            let (a, b) = tokio::join!(self.sonic_path(section, start, &w.rating_key), self.sonic_path(section, &w.rating_key, end));
            let (a, b) = match (a, b) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) => {
                    last_err = Some(e);
                    continue;
                }
            };
            if a.is_empty() || b.is_empty() {
                continue;
            }
            let mut seen = std::collections::HashSet::new();
            let path: Vec<Track> = a.into_iter().chain(b).filter(|t| seen.insert(t.rating_key.clone())).collect();
            if path.first().is_some_and(|t| t.rating_key == start) && path.last().is_some_and(|t| t.rating_key == end) {
                return Ok(path);
            }
        }
        match last_err {
            Some(e) => Err(e.context("no other path found")),
            None => anyhow::bail!("no other path found"),
        }
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

    /// A playlist's header and items (up to `limit`), with each item's
    /// playlist item id (what removing it needs).
    pub async fn playlist_detail(&self, key: &str, limit: usize) -> anyhow::Result<PlaylistDetail> {
        let (mp, ip) = (format!("/playlists/{key}"), format!("/playlists/{key}/items"));
        let items = async {
            let res = self
                .http
                .get(format!("{}{ip}", self.base))
                .header("Accept", "application/json")
                .header("X-Plex-Token", &self.token)
                .header("X-Plex-Container-Start", "0")
                .header("X-Plex-Container-Size", limit.to_string())
                .header("X-Plex-Client-Identifier", client_id())
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await?;
            anyhow::ensure!(res.status().is_success(), "playlist {key} -> {}", res.status());
            anyhow::Ok(res.json::<serde_json::Value>().await.context("playlist items")?)
        };
        let (meta, items) = tokio::join!(self.get(&mp), items);
        let meta: serde_json::Value = meta?.json().await.context("playlist")?;
        let items = items?;
        let m = &meta["MediaContainer"]["Metadata"][0];
        let (mut tracks, mut item_ids) = (Vec::new(), Vec::new());
        let raw = items["MediaContainer"]["Metadata"].as_array().cloned().unwrap_or_default();
        let loaded = raw.len();
        for it in raw {
            let id = it["playlistItemID"].as_u64().unwrap_or(0);
            if let Some(mut t) = serde_json::from_value::<Meta>(it).ok().and_then(Meta::into_track) {
                t.playlist_item = id;
                tracks.push(t);
                item_ids.push(id);
            }
        }
        Ok(PlaylistDetail {
            title: m["title"].as_str().unwrap_or_default().to_string(),
            smart: m["smart"].as_bool().unwrap_or(false),
            count: m["leafCount"].as_u64().unwrap_or(tracks.len() as u64),
            duration_ms: m["duration"].as_u64().unwrap_or(0),
            thumb: m["thumb"].as_str().or(m["composite"].as_str()).unwrap_or_default().to_string(),
            tracks,
            item_ids,
            capped: loaded >= limit && m["leafCount"].as_u64().is_some_and(|c| c > loaded as u64),
            loaded,
        })
    }

    /// Rename a playlist.
    pub async fn rename_playlist(&self, playlist: &str, title: &str) -> anyhow::Result<()> {
        let res = self
            .http
            .put(format!("{}/playlists/{playlist}?title={}", self.base, urlencode(title)))
            .timeout(std::time::Duration::from_secs(30))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", client_id())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "Plex answered {}", res.status());
        Ok(())
    }

    /// Delete a playlist (its tracks stay in the library).
    pub async fn delete_playlist(&self, playlist: &str) -> anyhow::Result<()> {
        let res = self
            .http
            .delete(format!("{}/playlists/{playlist}", self.base))
            .timeout(std::time::Duration::from_secs(30))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", client_id())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "Plex answered {}", res.status());
        Ok(())
    }

    /// Move one item of a regular playlist to just after `after` (another
    /// item id), or to the top when `None`. Plex also puts it at the top
    /// for an `after` it doesn't know, so pass ids from the current order.
    pub async fn move_playlist_item(&self, playlist: &str, item_id: u64, after: Option<u64>) -> anyhow::Result<()> {
        let q = after.map(|a| format!("?after={a}")).unwrap_or_default();
        let res = self
            .http
            .put(format!("{}/playlists/{playlist}/items/{item_id}/move{q}", self.base))
            .timeout(std::time::Duration::from_secs(30))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", client_id())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "Plex answered {}", res.status());
        Ok(())
    }

    /// Remove one item (by playlist item id) from a regular playlist.
    pub async fn remove_from_playlist(&self, playlist: &str, item_id: u64) -> anyhow::Result<()> {
        let res = self
            .http
            .delete(format!("{}/playlists/{playlist}/items/{item_id}", self.base))
            .timeout(std::time::Duration::from_secs(30))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", client_id())
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "Plex answered {}", res.status());
        Ok(())
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

    /// `server://<machine id>/com.plexapp.plugins.library<path>`, URL-encoded:
    /// how Plex names library items for playlists and play queues.
    async fn library_uri(&self, path: &str) -> anyhow::Result<String> {
        let root: serde_json::Value = tokio::time::timeout(std::time::Duration::from_secs(15), async { anyhow::Ok(self.get("/").await?.json().await?) })
            .await
            .context("server didn't answer")??;
        let mid = root["MediaContainer"]["machineIdentifier"].as_str().context("no machine id")?;
        Ok(urlencode(&format!("server://{mid}/com.plexapp.plugins.library{path}")))
    }

    async fn send_playlist(&self, req: reqwest::RequestBuilder) -> anyhow::Result<serde_json::Value> {
        let res = req
            .timeout(std::time::Duration::from_secs(30))
            .header("Accept", "application/json")
            .header("X-Plex-Token", &self.token)
            .header("X-Plex-Client-Identifier", client_id())
            .header("X-Plex-Product", "Crate")
            .send()
            .await?;
        anyhow::ensure!(res.status().is_success(), "Plex answered {}", res.status());
        res.json().await.context("playlist")
    }

    /// A new playlist of these items (albums add all their tracks).
    /// Returns (rating key, track count).
    pub async fn create_playlist(&self, title: &str, keys: &[String]) -> anyhow::Result<(String, u64)> {
        anyhow::ensure!(!keys.is_empty(), "nothing to add");
        let uri = self.library_uri(&format!("/library/metadata/{}", keys.join(","))).await?;
        let v = self
            .send_playlist(self.http.post(format!("{}/playlists?type=audio&smart=0&title={}&uri={uri}", self.base, urlencode(title))))
            .await?;
        let m = &v["MediaContainer"]["Metadata"][0];
        Ok((m["ratingKey"].as_str().context("no playlist key")?.to_string(), m["leafCount"].as_u64().unwrap_or(0)))
    }

    /// Append these items to a (non-smart) playlist; returns tracks added.
    pub async fn add_to_playlist(&self, playlist: &str, keys: &[String]) -> anyhow::Result<u64> {
        anyhow::ensure!(!keys.is_empty(), "nothing to add");
        let uri = self.library_uri(&format!("/library/metadata/{}", keys.join(","))).await?;
        let v = self.send_playlist(self.http.put(format!("{}/playlists/{playlist}/items?uri={uri}", self.base))).await?;
        Ok(v["MediaContainer"]["leafCountAdded"].as_u64().unwrap_or(0))
    }

    /// A smart playlist of the tracks on albums matching `album_query`
    /// (the library's album filters, e.g. `decade=1990&genre=12`).
    /// Returns (rating key, track count).
    pub async fn create_smart_playlist(&self, section: &str, title: &str, album_query: &str) -> anyhow::Result<(String, u64)> {
        let uri = self.library_uri(&format!("/library/sections/{section}/all?type=10&{}", track_query(album_query))).await?;
        let v = self
            .send_playlist(self.http.post(format!("{}/playlists?type=audio&smart=1&title={}&uri={uri}", self.base, urlencode(title))))
            .await?;
        let key = v["MediaContainer"]["Metadata"][0]["ratingKey"].as_str().context("no playlist key")?.to_string();
        // The create answer doesn't carry the count; ask.
        let n = async {
            let v: serde_json::Value = self.get(&format!("/playlists/{key}")).await.ok()?.json().await.ok()?;
            v["MediaContainer"]["Metadata"][0]["leafCount"].as_u64()
        }
        .await
        .unwrap_or(0);
        Ok((key, n))
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
                // The playlist's own poster; the auto collage only when there's none.
                thumb: m["thumb"].as_str().or(m["composite"].as_str()).unwrap_or_default().to_string(),
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
        // Mood mixes (canvas "Mixes for you"): each one any of a few library
        // moods, as random-track radio; only those the library has.
        const MIXES: &[(&str, &str, &[&str])] = &[
            ("Late Night Mix", "Slow, warm, low-energy picks", &["Nocturnal", "Laid-Back/Mellow", "Soothing", "Calm/Peaceful"]),
            ("Sunday Morning", "Hymns, gentle and reverent", &["Devotional", "Hymn-like", "Reverent", "Spiritual"]),
            ("High Energy", "Fast, bright and driving", &["Energetic", "Kinetic", "Driving", "Rousing"]),
        ];
        let mixes: Vec<MoodMix> = MIXES
            .iter()
            .filter_map(|(title, desc, names)| {
                let ids: Vec<&str> = names.iter().filter_map(|n| moods.iter().find(|(_, t)| t == n)).map(|(k, _)| k.as_str()).collect();
                (!ids.is_empty()).then(|| (title.to_string(), desc.to_string(), format!("mood={}", ids.join(",")), Vec::new()))
            })
            .collect();
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
            mixes,
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

    /// Albums by other artists where this artist is credited on a track (the
    /// track artist, or "feat. <name>" in the title). Plex has no hub for
    /// this, so it comes from search, which matches track titles only (so in
    /// practice: "(feat. <name>)" credits). Best effort, up to 50 matches.
    pub async fn appears_on(&self, key: &str, name: &str, section: &str) -> anyhow::Result<Vec<String>> {
        if name.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v: serde_json::Value =
            self.get(&format!("/hubs/search?query={}&sectionId={section}&limit=50", urlencode(name))).await?.json().await.context("appears on")?;
        let lname = name.to_lowercase();
        let mut out: Vec<String> = Vec::new();
        for hub in v["MediaContainer"]["Hub"].as_array().into_iter().flatten().filter(|h| h["type"] == "track") {
            for t in hub["Metadata"].as_array().into_iter().flatten() {
                if t["grandparentRatingKey"].as_str() == Some(key) {
                    continue;
                }
                let credited = t["originalTitle"].as_str().is_some_and(|o| credit_has(&o.to_lowercase(), &lname))
                    || t["title"].as_str().is_some_and(|s| title_credits(s, &lname));
                if credited
                    && let Some(album) = t["parentRatingKey"].as_str()
                    && !out.iter().any(|a| a == album)
                {
                    out.push(album.to_string());
                }
            }
        }
        Ok(out)
    }

    /// Mix builder: each artist's most-played tracks, interleaved
    /// (this artist, then each similar one in turn). Fetched together; a
    /// similar artist that fails is skipped, the main one failing is an error.
    pub async fn artist_mix(&self, section: &str, keys: Vec<String>, per: usize) -> anyhow::Result<Vec<Track>> {
        let mut set = tokio::task::JoinSet::new();
        for (i, k) in keys.iter().enumerate() {
            let (c, k, section) = (self.clone(), k.clone(), section.to_string());
            set.spawn(async move {
                let r: anyhow::Result<Vec<Track>> = async {
                    let env: Envelope = c
                        .get(&format!("/library/sections/{section}/all?type=10&artist.id={k}&sort=viewCount:desc&X-Plex-Container-Start=0&X-Plex-Container-Size={per}"))
                        .await?
                        .json()
                        .await
                        .context("artist mix")?;
                    Ok(env.mc.metadata.into_iter().filter_map(Meta::into_track).take(per).collect())
                }
                .await;
                (i, r)
            });
        }
        let mut lists: Vec<Vec<Track>> = vec![Vec::new(); keys.len()];
        while let Some(joined) = set.join_next().await {
            let (i, r) = joined.map_err(|e| anyhow::anyhow!("artist mix: {e}"))?;
            match r {
                Ok(l) => lists[i] = l,
                Err(e) if i == 0 => return Err(e),
                Err(_) => {}
            }
        }
        let mut out = Vec::new();
        for i in 0..per {
            for l in &lists {
                if let Some(t) = l.get(i) {
                    out.push(t.clone());
                }
            }
        }
        Ok(out)
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

/// `name` (lowercase) appears in `credit` (lowercase) as a whole credited
/// name: preceded by the start or a credit separator and followed by the end
/// or one. Works for names that contain separators themselves ("earth, wind
/// & fire", "ac/dc"), and checks every occurrence ("drakeo & drake").
fn credit_has(credit: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    const BEFORE: [&str; 13] = [", ", "& ", " and ", " x ", "/", "/ ", "; ", "(", "[", "feat. ", "feat ", "ft. ", "featuring "];
    const AFTER: [&str; 13] = [",", " & ", " and ", " x ", "/", " /", ";", ")", "]", " feat", " ft.", " with ", " - "];
    credit.match_indices(name).any(|(i, _)| {
        let (pre, post) = (&credit[..i], &credit[i + name.len()..]);
        let pre_ok = pre.trim_end().is_empty() || BEFORE.iter().any(|b| pre.ends_with(b)) || pre.ends_with("with ");
        let post_ok = post.trim().is_empty() || AFTER.iter().any(|a| post.starts_with(a));
        pre_ok && post_ok
    })
}

/// A title credits `name` (lowercase) as a guest: "(feat. Name)", "[ft. Name]",
/// "- feat. Name", the name anywhere in that credit list; a bare "with" only
/// inside brackets ("(with Name)"), since "Stay with Me" isn't a credit.
fn title_credits(title: &str, name: &str) -> bool {
    let t = title.to_lowercase();
    let mut starts = Vec::new();
    for f in ["feat. ", "feat ", "ft. ", "featuring "] {
        for (i, _) in t.match_indices(f) {
            // A word boundary before the marker ("defeat " isn't one).
            if i == 0 || t[..i].ends_with([' ', '(', '[', '-']) {
                starts.push(i + f.len());
            }
        }
    }
    for f in ["(with ", "[with "] {
        starts.extend(t.match_indices(f).map(|(i, _)| i + f.len()));
    }
    starts.into_iter().any(|start| {
        // The credit runs to a closing bracket or a " - " suffix.
        let tail = &t[start..];
        let end = [tail.find([')', ']']), tail.find(" - ")].into_iter().flatten().min().unwrap_or(tail.len());
        credit_has(&tail[..end], name)
    })
}

#[cfg(test)]
mod credit_tests {
    use super::{credit_has, title_credits};

    #[test]
    fn credits() {
        assert!(credit_has("jay-z & kanye west feat. frank ocean", "kanye west"));
        assert!(credit_has("earth, wind & fire", "earth, wind & fire"));
        assert!(credit_has("ac/dc", "ac/dc"));
        assert!(!credit_has("blair", "air"));
        assert!(!credit_has("drake bell", "drake"));
        assert!(credit_has("artist a / jon batiste", "jon batiste"));
        assert!(credit_has("jon batiste / artist b", "jon batiste"));
        assert!(credit_has("cory wong feat jon batiste", "jon batiste"));
        assert!(title_credits("Home (feat. Jon Batiste)", "jon batiste"));
        assert!(title_credits("Song (feat. A, B & Jon Batiste)", "jon batiste"));
        assert!(title_credits("September (feat. Earth, Wind & Fire)", "earth, wind & fire"));
        assert!(title_credits("Song (feat. Tyler, The Creator)", "tyler, the creator"));
        assert!(title_credits("Song (feat. Drakeo & Drake)", "drake"));
        assert!(title_credits("Song - feat. Common - Remastered", "common"));
        assert!(!title_credits("Track (feat. Drakeo the Ruler)", "drake"));
        assert!(!title_credits("Stay with Me", "me"));
        assert!(!title_credits("To Sir with Love", "love"));
        assert!(!title_credits("Defeat Me", "me"));
        assert!(title_credits("Glory (with Common)", "common"));
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

/// Where a download is written before it's complete: `<file>.part`.
pub fn partial_path(dest: &std::path::Path) -> std::path::PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// A per-install client id, so two machines running Crate are two clients
/// to the server (their sessions and plays don't collide). Kept in cache_dir().
pub fn client_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let path = cache_dir().join("client-id");
        if let Ok(id) = std::fs::read_to_string(&path)
            && !id.trim().is_empty()
        {
            return id.trim().to_string();
        }
        let id = std::fs::read_to_string("/proc/sys/kernel/random/uuid")
            .map(|u| format!("crate-{}", u.trim()))
            .unwrap_or_else(|_| format!("crate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(cache_dir());
        let _ = std::fs::write(&path, &id);
        id
    })
}

/// ~/.cache/crate (or $XDG_CACHE_HOME/crate).
pub fn cache_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache"));
    base.join("crate")
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
        eprintln!("[crate] cached library: {} albums in {:?}", cached.len(), t.elapsed());
        let _ = albums_tx.send(("cache", Ok(cached)));
    }
    let t = std::time::Instant::now();
    let albums = client.albums(&section).await;
    if let Ok(a) = &albums {
        eprintln!("[crate] fetched {} albums in {:?}", a.len(), t.elapsed());
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
                eprintln!("[crate] art {}: {e:#}", req.thumb);
            }
            let _ = tx.send(ArtResult { thumb: req.thumb, bytes: bytes.ok() });
            drop(permit);
        });
    }
}

#[cfg(test)]
mod format_tests {
    use super::short_format;

    #[test]
    fn card_formats() {
        assert_eq!(short_format("flac", Some(16), Some(44_100)), "FLAC");
        assert_eq!(short_format("flac", Some(24), Some(96_000)), "24/96");
        assert_eq!(short_format("flac", Some(24), Some(48_000)), "24/48");
        assert_eq!(short_format("alac", Some(24), Some(44_100)), "24/44.1");
        assert_eq!(short_format("mp3", None, Some(44_100)), "MP3");
        assert_eq!(short_format("dsd_lsbf_planar", Some(1), Some(2_822_400)), "DSD64");
        assert_eq!(short_format("dsd_msbf", None, Some(5_644_800)), "DSD128");
        // PCM-equivalent DSD rates are ambiguous across conventions.
        assert_eq!(short_format("dsd_msbf", None, Some(176_400)), "DSD");
        assert_eq!(short_format("dsd_lsbf", Some(1), Some(352_800)), "DSD");
        assert_eq!(short_format("flac", Some(24), None), "FLAC");
        assert_eq!(short_format("flac", Some(24), Some(22_050)), "24/22");
    }
}

/// The library's album filter (`decade=1990&userRating>>=8`) as a track
/// query: each field moves to the album (`album.decade=1990&album.userRating>>=8`).
pub(crate) fn track_query(album_query: &str) -> String {
    album_query.split('&').filter(|t| !t.is_empty()).map(|t| format!("album.{t}")).collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod playlist_tests {
    #[test]
    fn album_filters_become_track_filters() {
        assert_eq!(super::track_query("decade=1990&userRating>>=8"), "album.decade=1990&album.userRating>>=8");
        assert_eq!(super::track_query("mood=1,2"), "album.mood=1,2");
        assert_eq!(super::track_query(""), "");
    }
}
