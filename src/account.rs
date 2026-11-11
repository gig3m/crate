//! Crate's own Plex sign-in: server URL, token and music library, stored in
//! ~/.config/crate/account.toml (mode 0600). PLEX_URL / PLEX_TOKEN /
//! PLEX_SECTION in the environment override the stored values (handy for
//! testing); without either, Crate shows its sign-in screen.

use std::{os::unix::fs::PermissionsExt, path::PathBuf};

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
}

fn path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("crate/account.toml")
}

/// The stored account with any environment overrides, if complete.
pub fn load() -> Option<Account> {
    let mut a: Account = std::fs::read_to_string(path()).ok().and_then(|r| toml::from_str(&r).ok()).unwrap_or_default();
    if let Ok(v) = std::env::var("PLEX_URL") {
        a.url = v;
    }
    if let Ok(v) = std::env::var("PLEX_TOKEN") {
        a.token = v;
    }
    if let Ok(v) = std::env::var("PLEX_SECTION") {
        a.section = v;
    }
    if a.token.is_empty() {
        return None;
    }
    if a.url.is_empty() {
        a.url = "http://localhost:32400".into();
    }
    if a.section.is_empty() {
        a.section = "1".into();
    }
    Some(a)
}

/// Save with owner-only permissions (written to a temp file, then renamed).
pub fn save(a: &Account) -> anyhow::Result<()> {
    let p = path();
    std::fs::create_dir_all(p.parent().context("config dir")?)?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(a)?)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, &p)?;
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
pub async fn poll_pin(http: &reqwest::Client, id: u64) -> anyhow::Result<Option<String>> {
    let v: serde_json::Value = tv(http, reqwest::Method::GET, &format!("/pins/{id}")).send().await?.error_for_status()?.json().await?;
    Ok(v["authToken"].as_str().filter(|t| !t.is_empty()).map(String::from))
}

#[derive(Clone, Debug)]
pub struct Server {
    pub name: String,
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
        .map(|r| Server {
            name: r["name"].as_str().unwrap_or("Plex").to_string(),
            token: r["accessToken"].as_str().unwrap_or(account_token).to_string(),
            connections: r["connections"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| Some((c["uri"].as_str()?.to_string(), c["local"].as_bool().unwrap_or(false), c["relay"].as_bool().unwrap_or(false))))
                .collect(),
        })
        .collect())
}

/// Try every connection at once; return the first that answers, preferring
/// local over remote over relay.
pub async fn pick_connection(http: &reqwest::Client, s: &Server) -> anyhow::Result<String> {
    let mut set = tokio::task::JoinSet::new();
    for (uri, local, relay) in s.connections.clone() {
        let (http, token) = (http.clone(), s.token.clone());
        set.spawn(async move {
            let ok = http
                .get(format!("{uri}/identity"))
                .header("X-Plex-Token", token)
                .timeout(std::time::Duration::from_secs(4))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            (ok, uri, if local { 0 } else if relay { 2 } else { 1 })
        });
    }
    let mut best: Option<(u8, String)> = None;
    while let Some(Ok((ok, uri, rank))) = set.join_next().await {
        if ok && best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, uri));
            if rank == 0 {
                break;
            }
        }
    }
    best.map(|(_, u)| u).context("none of the server's addresses answered")
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
