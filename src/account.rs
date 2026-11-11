//! Crate's own Plex sign-in: server URL, token and music library, stored in
//! ~/.config/crate/account.toml (mode 0600). PLEX_URL / PLEX_TOKEN /
//! PLEX_SECTION in the environment override the stored values (handy for
//! testing); without either, Crate shows its sign-in screen.

use std::{io::Write as _, os::unix::fs::OpenOptionsExt, path::PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    /// Server base URL, e.g. http://plex-server:32400.
    pub url: String,
    pub token: String,
    /// Music library section key.
    pub section: String,
    /// Server name, for display.
    #[serde(default)]
    pub server: String,
    /// The server's machine identifier and every address Plex listed for it,
    /// so Crate can find it again when `url` stops answering (another network).
    #[serde(default)]
    pub server_id: String,
    #[serde(default)]
    pub connections: Vec<Conn>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Conn {
    pub uri: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
    pub relay: bool,
}

impl Account {
    /// The stored addresses as a Server for pick_connection.
    pub fn as_server(&self) -> Server {
        Server {
            name: self.server.clone(),
            id: self.server_id.clone(),
            token: self.token.clone(),
            connections: self.connections.iter().map(|c| (c.uri.clone(), c.local, c.relay)).collect(),
        }
    }

    pub fn remember(&mut self, s: &Server) {
        self.server = s.name.clone();
        self.server_id = s.id.clone();
        self.connections = s.connections.iter().map(|(uri, local, relay)| Conn { uri: uri.clone(), local: *local, relay: *relay }).collect();
    }
}

/// At launch: if the saved address doesn't answer (another network, a
/// changed IP), find the server again among its known addresses and save
/// the one that works. Returns the account to use.
pub async fn ensure_reachable(mut a: Account) -> Account {
    let http = reqwest::Client::new();
    let ok = http
        .get(format!("{}/identity", a.url))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());
    if ok || a.connections.is_empty() {
        return a;
    }
    match pick_connection(&http, &a.as_server()).await {
        Ok((url, _)) if url != a.url => {
            eprintln!("[crate] {} didn't answer; using {url}", a.url);
            a.url = url;
            if from_env().is_none() {
                let _ = save(&a);
            }
            a
        }
        _ => a,
    }
}

fn path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("crate/account.toml")
}

/// The account from the environment, used only when PLEX_TOKEN, PLEX_URL and
/// PLEX_SECTION are all set (never mixed with the stored account).
pub fn from_env() -> Option<Account> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let token = env("PLEX_TOKEN")?;
    match (env("PLEX_URL"), env("PLEX_SECTION")) {
        (Some(url), Some(section)) => Some(Account { url, token, section, ..Default::default() }),
        _ => {
            eprintln!("[crate] PLEX_TOKEN is set without PLEX_URL and PLEX_SECTION; ignoring the environment");
            None
        }
    }
}

/// The account to use: a complete environment override, else the stored one.
pub fn load() -> Option<Account> {
    from_env().or_else(|| stored().filter(|a| !a.token.is_empty() && !a.url.is_empty() && !a.section.is_empty()))
}

/// The saved account alone, without environment overrides.
pub fn stored() -> Option<Account> {
    std::fs::read_to_string(path()).ok().and_then(|r| toml::from_str(&r).ok())
}

/// Save with owner-only permissions: the temp file is created new with mode
/// 0600 before any bytes are written (no window where the token is
/// readable), under a unique name, then renamed over the old file.
pub fn save(a: &Account) -> anyhow::Result<()> {
    let p = path();
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(p.parent().context("config dir")?)?;
    }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = p.with_extension(format!("toml.{}.{nonce}.tmp", std::process::id()));
    let res = (|| -> anyhow::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(toml::to_string(a)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Drop caches tied to a server (library list, cover art) when the account
/// changes, so a new server never shows the old one's library.
pub fn clear_server_caches() {
    let dir = crate::plex::cache_dir();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("albums-") && name.ends_with(".json") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir.join("art"));
}

/// Relaunch Crate as a fresh process that reads the stored account, without
/// the PLEX_* overrides this process may have been started with.
pub fn relaunch() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    // After a reinstall the running binary shows as "<path> (deleted)": start the new one.
    let exe = std::path::PathBuf::from(exe.to_string_lossy().trim_end_matches(" (deleted)").to_string());
    std::process::Command::new(exe).env_remove("PLEX_URL").env_remove("PLEX_TOKEN").env_remove("PLEX_SECTION").spawn()?;
    Ok(())
}

/// Forget the sign-in.
pub fn remove() -> std::io::Result<()> {
    match std::fs::remove_file(path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

// ---- plex.tv sign-in (PIN flow) and server discovery ----

const PLEX_TV: &str = "https://plex.tv/api/v2";

fn tv(http: &reqwest::Client, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
    http.request(method, format!("{PLEX_TV}{path}"))
        .header("Accept", "application/json")
        .header("X-Plex-Product", "Crate")
        .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
        .header("X-Plex-Client-Identifier", crate::plex::client_id())
        .header("X-Plex-Platform", "Linux")
}

/// Start a sign-in: (pin id, code). The user approves it at `auth_url`.
pub async fn create_pin(http: &reqwest::Client) -> anyhow::Result<(u64, String)> {
    let v: serde_json::Value =
        tv(http, reqwest::Method::POST, "/pins?strong=true").send().await?.error_for_status()?.json().await.context("pin")?;
    Ok((v["id"].as_u64().context("pin id")?, v["code"].as_str().context("pin code")?.to_string()))
}

pub fn auth_url(code: &str) -> String {
    format!(
        "https://app.plex.tv/auth#?clientID={}&code={code}&context%5Bdevice%5D%5Bproduct%5D=Crate",
        crate::plex::client_id()
    )
}

/// The account token once the user has approved the PIN.
/// Err(PinExpired) once plex.tv no longer knows the PIN.
pub async fn poll_pin(http: &reqwest::Client, id: u64, code: &str) -> anyhow::Result<Option<String>> {
    let res = tv(http, reqwest::Method::GET, &format!("/pins/{id}?code={code}")).send().await?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow::Error::new(PinExpired));
    }
    let v: serde_json::Value = res.error_for_status()?.json().await?;
    Ok(v["authToken"].as_str().filter(|t| !t.is_empty()).map(String::from))
}

#[derive(Debug)]
pub struct PinExpired;
impl std::fmt::Display for PinExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "the sign-in code expired")
    }
}
impl std::error::Error for PinExpired {}

#[derive(Clone, Debug)]
pub struct Server {
    pub name: String,
    /// The server's machine identifier, checked against /identity.
    pub id: String,
    pub token: String,
    /// (uri, local, relay)
    pub connections: Vec<(String, bool, bool)>,
}

/// Plex Media Servers this account can reach.
pub async fn servers(http: &reqwest::Client, account_token: &str) -> anyhow::Result<Vec<Server>> {
    let v: serde_json::Value = tv(http, reqwest::Method::GET, "/resources?includeHttps=1&includeRelay=1")
        .header("X-Plex-Token", account_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["provides"].as_str().unwrap_or_default().split(',').any(|p| p == "server"))
        // A server without its own access token would be sent the account token.
        .filter(|r| r["accessToken"].as_str().is_some_and(|t| !t.is_empty()))
        .map(|r| Server {
            name: r["name"].as_str().unwrap_or("Plex").to_string(),
            id: r["clientIdentifier"].as_str().unwrap_or_default().to_string(),
            token: r["accessToken"].as_str().unwrap_or_default().to_string(),
            connections: r["connections"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| Some((c["uri"].as_str()?.to_string(), c["local"].as_bool().unwrap_or(false), c["relay"].as_bool().unwrap_or(false))))
                .collect(),
        })
        .collect())
}

/// Try every connection at once and keep one that is really this server
/// (its /identity machineIdentifier matches) and lets this token list
/// libraries; prefer local over remote over relay. Returns (url, music
/// sections). A local address that belongs to some other host on this
/// network never wins, and a working remote route is kept as the fallback.
pub async fn pick_connection(http: &reqwest::Client, s: &Server) -> anyhow::Result<(String, Vec<(String, String)>)> {
    let mut set = tokio::task::JoinSet::new();
    for (uri, local, relay) in s.connections.clone() {
        let https = uri.starts_with("https://");
        // Never send the token in cleartext beyond the local network.
        if !https && !local {
            continue;
        }
        let (http, token, want) = (http.clone(), s.token.clone(), s.id.clone());
        set.spawn(async move {
            // local https, local http, remote https, relay
            let rank = match (local, https, relay) {
                (_, _, true) => 3,
                (true, true, _) => 0,
                (true, false, _) => 1,
                _ => 2,
            };
            let check = async {
                let id: serde_json::Value = http
                    .get(format!("{uri}/identity"))
                    .header("Accept", "application/json")
                    .timeout(std::time::Duration::from_secs(4))
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?; // unauthenticated: the token goes only to a verified server
                anyhow::ensure!(
                    want.is_empty() || id["MediaContainer"]["machineIdentifier"].as_str() == Some(want.as_str()),
                    "a different server answered at {uri}"
                );
                music_sections(&http, &uri, &token).await
            };
            let res = check.await;
            (rank, uri, res)
        });
    }
    type Found = (u8, String, Vec<(String, String)>);
    let mut best: Option<Found> = None;
    let mut last_err = None;
    while let Some(Ok((rank, uri, res))) = set.join_next().await {
        match res {
            Ok(sections) => {
                if best.as_ref().is_none_or(|(r, _, _)| rank < *r) {
                    best = Some((rank, uri, sections));
                }
                if rank == 0 {
                    break;
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    match best {
        Some((_, uri, sections)) => Ok((uri, sections)),
        None => Err(last_err.unwrap_or_else(|| anyhow::anyhow!("the server has no addresses")).context("couldn't reach the server")),
    }
}

/// Music libraries on a server: (section key, title).
pub async fn music_sections(http: &reqwest::Client, url: &str, token: &str) -> anyhow::Result<Vec<(String, String)>> {
    let v: serde_json::Value = http
        .get(format!("{url}/library/sections"))
        .header("Accept", "application/json")
        .header("X-Plex-Token", token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v["MediaContainer"]["Directory"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["type"].as_str() == Some("artist"))
        .filter_map(|d| Some((d["key"].as_str()?.to_string(), d["title"].as_str()?.to_string())))
        .collect())
}
