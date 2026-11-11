//! Track sources. `Progressive` downloads a Plex file to the local cache on a
//! background thread while the decoder reads from it, blocking only when it
//! gets ahead of the download.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use symphonia::core::io::MediaSource;

#[derive(Clone, Debug)]
pub enum Source {
    File(PathBuf),
    /// A Plex URL (absolute) fetched with the token, cached at `cache`.
    /// `prepare`, when set, is fetched first and must succeed (a transcode
    /// decision); `{session}` in both URLs becomes a fresh id per open.
    /// `len_ms` overrides the container's length, for streams that don't
    /// carry a reliable one (Plex's transcoded MP3 has no Xing header).
    Http { url: String, token: String, cache: PathBuf, prepare: Option<String>, len_ms: Option<u64> },
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::File(p) => p.file_name().unwrap_or_default().to_string_lossy().into(),
            Source::Http { cache, .. } => {
                format!("plex:{}", cache.file_name().unwrap_or_default().to_string_lossy())
            }
        }
    }

    /// Known length, when the container's own can't be trusted.
    pub fn len_ms(&self) -> Option<u64> {
        match self {
            Source::Http { len_ms, .. } => *len_ms,
            Source::File(_) => None,
        }
    }

    pub fn ext(&self) -> Option<String> {
        let p = match self {
            Source::File(p) => p.clone(),
            Source::Http { cache, .. } => cache.clone(),
        };
        p.extension().map(|e| e.to_string_lossy().into_owned())
    }

    /// Open for decoding. For HTTP, a complete cached copy is used if present.
    /// `stop` aborts a read that is waiting on the network.
    pub fn open(&self, stop: Arc<AtomicBool>) -> anyhow::Result<Box<dyn MediaSource>> {
        match self {
            Source::File(p) => Ok(Box::new(File::open(p)?)),
            Source::Http { cache, .. } if cache.exists() => Ok(Box::new(File::open(cache)?)),
            Source::Http { url, token, cache, prepare: None, .. } => Ok(Box::new(Progressive::start(url, token, CLIENT, cache, stop)?)),
            Source::Http { url, token, cache, prepare: Some(prepare), .. } => {
                // One transcode session per open, so a reopen (seek, restart)
                // never shares a server-side job with an attempt being dropped.
                let session = format!("crate-{}-{}", std::process::id(), ATTEMPT.fetch_add(1, Ordering::Relaxed));
                reqwest::blocking::Client::new()
                    .get(prepare.replace("{session}", &session))
                    .header("X-Plex-Token", token.as_str())
                    .header("X-Plex-Client-Identifier", TRANSCODE_CLIENT)
                    .header("Accept", "application/json")
                    .timeout(Duration::from_secs(10))
                    .send()?
                    .error_for_status()?;
                Ok(Box::new(Progressive::start(&url.replace("{session}", &session), token, TRANSCODE_CLIENT, cache, stop)?))
            }
        }
    }
}

/// Client id for direct file fetches.
const CLIENT: &str = "crate";
/// Transcodes go under their own client id: a transcode decision leaves
/// Plex denying that client direct play of the part ("session lacking
/// permission to direct play", 503) for a while, which would break
/// original-quality and offline fetches of the same track.
const TRANSCODE_CLIENT: &str = "crate-transcode";

struct DlState {
    written: AtomicU64,
    total: AtomicU64,
    done: AtomicBool,
    error: Mutex<Option<String>>,
    started: Instant,
    first_byte_ms: AtomicU64,
}

pub struct Progressive {
    file: File,
    pos: u64,
    st: Arc<DlState>,
    /// Engine stop: unblocks reads waiting for bytes.
    stop: Arc<AtomicBool>,
    /// Set on drop: the downloader abandons this attempt and deletes its temp file.
    cancel: Arc<AtomicBool>,
}

impl Drop for Progressive {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

static ATTEMPT: AtomicU64 = AtomicU64::new(0);

impl Progressive {
    fn start(url: &str, token: &str, client: &str, cache: &std::path::Path, stop: Arc<AtomicBool>) -> anyhow::Result<Self> {
        // One temp file per attempt: overlapping downloads of the same track
        // (seek, restart) never share or truncate each other's file.
        // Request first, so a 404 or auth failure surfaces as itself.
        let mut resp = reqwest::blocking::Client::new()
            .get(url)
            .header("X-Plex-Token", token)
            .header("X-Plex-Client-Identifier", client)
            .send()?
            .error_for_status()?;
        let n = ATTEMPT.fetch_add(1, Ordering::Relaxed);
        let part = cache.with_extension(format!("part.{}.{n}", std::process::id()));
        let writer = File::create(&part)?;
        let reader = File::open(&part)?;
        let st = Arc::new(DlState {
            written: AtomicU64::new(0),
            total: AtomicU64::new(0),
            done: AtomicBool::new(false),
            error: Mutex::new(None),
            started: Instant::now(),
            first_byte_ms: AtomicU64::new(0),
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let (cache, st2, cancel2) = (cache.to_path_buf(), st.clone(), cancel.clone());
        std::thread::Builder::new().name("download".into()).spawn(move || {
            let res = (|| -> anyhow::Result<()> {
                let mut w = writer;
                if let Some(len) = resp.content_length() {
                    st2.total.store(len, Ordering::Release);
                }
                let mut buf = vec![0u8; 256 * 1024];
                loop {
                    if cancel2.load(Ordering::Acquire) {
                        anyhow::bail!("cancelled");
                    }
                    let n = resp.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    w.write_all(&buf[..n])?;
                    if st2.written.load(Ordering::Relaxed) == 0 {
                        st2.first_byte_ms.store(st2.started.elapsed().as_millis() as u64, Ordering::Relaxed);
                    }
                    st2.written.fetch_add(n as u64, Ordering::Release);
                }
                w.flush()?;
                let total = st2.total.load(Ordering::Acquire);
                let got = st2.written.load(Ordering::Acquire);
                anyhow::ensure!(total == 0 || got == total, "short download: {got} of {total} bytes");
                if cache.exists() {
                    // Another attempt already published this track.
                    let _ = std::fs::remove_file(&part);
                } else {
                    std::fs::rename(&part, &cache)?;
                }
                eprintln!(
                    "[download] {} complete: {:.1} MB in {:?} (first byte {} ms)",
                    cache.display(),
                    st2.written.load(Ordering::Relaxed) as f64 / 1e6,
                    st2.started.elapsed(),
                    st2.first_byte_ms.load(Ordering::Relaxed)
                );
                Ok(())
            })();
            if let Err(e) = res {
                let _ = std::fs::remove_file(&part);
                *st2.error.lock().unwrap() = Some(format!("{e:#}"));
            }
            st2.done.store(true, Ordering::Release);
        })?;
        Ok(Self { file: reader, pos: 0, st, stop, cancel })
    }
}

impl Read for Progressive {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let written = self.st.written.load(Ordering::Acquire);
            if self.pos < written {
                let n = (written - self.pos).min(buf.len() as u64) as usize;
                let got = self.file.read_at(&mut buf[..n], self.pos)?;
                self.pos += got as u64;
                return Ok(got);
            }
            if self.stop.load(Ordering::Acquire) {
                return Err(std::io::Error::other("stopped"));
            }
            if self.st.done.load(Ordering::Acquire) {
                if let Some(e) = self.st.error.lock().unwrap().clone() {
                    return Err(std::io::Error::other(e));
                }
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Seek for Progressive {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let total = self.st.total.load(Ordering::Acquire);
        let new = match to {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => {
                if total == 0 {
                    return Err(std::io::Error::other("length unknown"));
                }
                total as i64 + d
            }
        };
        if new < 0 {
            return Err(std::io::Error::other("seek before start"));
        }
        // Seeking past the downloaded part just waits in read() for the bytes.
        self.pos = new as u64;
        Ok(self.pos)
    }
}

impl MediaSource for Progressive {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        match self.st.total.load(Ordering::Acquire) {
            0 => None,
            n => Some(n),
        }
    }
}
