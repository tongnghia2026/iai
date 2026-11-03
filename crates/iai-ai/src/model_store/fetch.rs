//! Fetching a model: each of its sources in the list's order (its author's,
//! the author of iAi's GitHub Releases, his Google Drive) until one gives the
//! very file its SHA-256 names. A file that is not it is thrown away and the
//! next source tried; a download cut short is kept as `<file>.part` and
//! resumed from there (HTTP Range) the next time. When no source could be
//! reached at all the machine is taken to have no network, which is said
//! apart from sources that answered with something else.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::list::{Entry, ModelList};

/// An answer to a GET.
pub struct Reply {
    pub status: u16,
    /// The body starts where it was asked to (206), not at the file's start.
    pub resumed: bool,
    /// The whole file's size, when the server says it.
    pub total: Option<u64>,
    /// The body is a web page (Drive's "virus scan" page, a login page).
    pub html: bool,
    pub body: Box<dyn Read + Send>,
}

/// Why a GET got no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The host could not be reached: no network, or the host is gone.
    Unreachable(String),
    Other(String),
}

/// How files are fetched: over HTTP, or from a test's table.
pub trait Transport: Sync {
    /// GET `url`, the bytes from `from` on when `from` is not 0.
    fn get(&self, url: &str, from: u64) -> Result<Reply, Failure>;
}

/// The network, through `reqwest`.
pub struct Http {
    client: reqwest::blocking::Client,
}

impl Http {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            // A large model on a slow line takes hours; one cut short is
            // resumed.
            .timeout(Duration::from_secs(6 * 3600))
            .user_agent("iAi")
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Self { client })
    }

    /// For a small file read before a download (the author's list): a host
    /// that does not answer soon is given up on, and the download goes on.
    pub fn quick() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent("iAi")
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Self { client })
    }
}

impl Transport for Http {
    fn get(&self, url: &str, from: u64) -> Result<Reply, Failure> {
        let mut request = self.client.get(url);
        if from > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={from}-"));
        }
        // A Hugging Face token, when the user has one, goes to Hugging Face
        // only.
        if url.starts_with("https://huggingface.co/")
            && let Ok(token) =
                std::env::var("HF_TOKEN").or_else(|_| std::env::var("HUGGINGFACE_TOKEN"))
            && !token.trim().is_empty()
        {
            request = request.bearer_auth(token.trim());
        }
        let response = request.send().map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                Failure::Unreachable(e.to_string())
            } else {
                Failure::Other(e.to_string())
            }
        })?;
        let status = response.status().as_u16();
        let resumed = status == 206;
        let header = |name: reqwest::header::HeaderName| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let total = if resumed {
            header(reqwest::header::CONTENT_RANGE)
                .and_then(|r| r.rsplit('/').next().and_then(|t| t.trim().parse().ok()))
        } else {
            response.content_length()
        };
        let html = header(reqwest::header::CONTENT_TYPE)
            .is_some_and(|t| t.to_ascii_lowercase().contains("text/html"));
        Ok(Reply {
            status,
            resumed,
            total,
            html,
            body: Box::new(response),
        })
    }
}

/// The whole body of `url`, no more than `max` bytes of it.
pub fn get_small(transport: &dyn Transport, url: &str, max: u64) -> Result<Vec<u8>, String> {
    let reply = transport.get(url, 0).map_err(|e| format!("{url}: {e:?}"))?;
    if !(200..300).contains(&reply.status) {
        return Err(format!("{url}: HTTP {}", reply.status));
    }
    let mut bytes = Vec::new();
    reply
        .body
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{url}: {e}"))?;
    if bytes.len() as u64 > max {
        return Err(format!("{url}: larger than {max} bytes"));
    }
    Ok(bytes)
}

/// Why a model was not fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// No source could be reached. `partial`: some of it came, and is kept.
    NoNetwork {
        partial: bool,
    },
    /// Every source answered, none with the right file: (source, why).
    SourcesFailed(Vec<(String, String)>),
    /// The app does not fetch this model, and why (for people).
    NotDownloadable(String),
    /// No such model in the list.
    Unknown(String),
    Cancelled,
    /// The file could not be written.
    Disk(String),
}

impl FetchError {
    /// For people.
    pub fn text(&self) -> String {
        match self {
            FetchError::NoNetwork { partial: false } => {
                "Máy chưa có mạng — nối mạng rồi bấm «Tải» lại".to_string()
            }
            FetchError::NoNetwork { partial: true } => {
                "Mất mạng giữa chừng — nối mạng rồi bấm «Tải» để tải tiếp từ chỗ dừng".to_string()
            }
            FetchError::SourcesFailed(tried) => format!(
                "Không tải được từ nguồn nào (đã thử {} nguồn) — liên hệ tác giả để nhận file",
                tried.len()
            ),
            FetchError::NotDownloadable(why) => why.clone(),
            FetchError::Unknown(id) => format!("Không có model {id} trong danh sách"),
            FetchError::Cancelled => "Đã dừng tải — lần sau tải tiếp từ chỗ dừng".to_string(),
            FetchError::Disk(why) => format!("Không ghi được file model: {why}"),
        }
    }

    /// The user should be shown how to reach the author for the file: the
    /// network is there, the sources are not.
    pub fn needs_contact(&self) -> bool {
        matches!(
            self,
            FetchError::SourcesFailed(_) | FetchError::NotDownloadable(_)
        )
    }
}

/// One file to fetch.
pub struct Wanted<'a> {
    pub dest: &'a Path,
    /// Lowercase hex.
    pub sha256: &'a str,
    /// Bytes, 0 when not known.
    pub size: u64,
    /// The sources, in order: (what for people, address).
    pub sources: Vec<(String, String)>,
}

/// `<dest>.part`: what has come of it so far.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// How a source failed.
enum Attempt {
    /// Could not be reached.
    Unreachable,
    /// Reached, and cut off before the file was whole.
    Interrupted,
    /// Answered with something else than the file.
    Broken(String),
}

/// Fetch one file to `wanted.dest`. `progress` hears the bytes there are and
/// the bytes there will be (0 when not known).
pub fn fetch_file(
    transport: &dyn Transport,
    wanted: &Wanted,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
) -> Result<(), FetchError> {
    let part = part_path(wanted.dest);
    if let Some(dir) = wanted.dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| FetchError::Disk(e.to_string()))?;
    }
    let mut failures: Vec<(String, Attempt)> = Vec::new();
    for (from, url) in &wanted.sources {
        let mut retried = false;
        let attempt = loop {
            match fetch_from(transport, wanted, url, &part, progress, cancel)? {
                Ok(()) => return Ok(()),
                // A resumed file that is not the file: the part kept was
                // spoilt; once more from the start.
                Err(Attempt::Broken(why)) if why == RESUMED_WRONG && !retried => {
                    retried = true;
                }
                Err(attempt) => break attempt,
            }
        };
        failures.push((format!("{from}: {url}"), attempt));
    }
    if failures.is_empty() {
        return Err(FetchError::NotDownloadable(
            "Chưa có link tải — xem hướng dẫn hoặc liên hệ tác giả".to_string(),
        ));
    }
    let partial = part.is_file();
    if failures
        .iter()
        .all(|(_, a)| matches!(a, Attempt::Unreachable | Attempt::Interrupted))
    {
        return Err(FetchError::NoNetwork { partial });
    }
    Err(FetchError::SourcesFailed(
        failures
            .into_iter()
            .map(|(source, attempt)| {
                let why = match attempt {
                    Attempt::Unreachable => "không vào được".to_string(),
                    Attempt::Interrupted => "đứt giữa chừng".to_string(),
                    Attempt::Broken(why) => why,
                };
                (source, why)
            })
            .collect(),
    ))
}

const RESUMED_WRONG: &str = "file tải tiếp không đúng";

fn verify_and_install(part: &Path, wanted: &Wanted) -> Result<bool, FetchError> {
    let actual =
        crate::ai::download::file_sha256_hex(part).map_err(|e| FetchError::Disk(e.to_string()))?;
    if actual != wanted.sha256 {
        let _ = std::fs::remove_file(part);
        return Ok(false);
    }
    std::fs::rename(part, wanted.dest).map_err(|e| FetchError::Disk(e.to_string()))?;
    Ok(true)
}

/// One try at one source: the outer error stops everything (the user
/// cancelled, the disk failed), the inner one moves on to the next source.
fn fetch_from(
    transport: &dyn Transport,
    wanted: &Wanted,
    url: &str,
    part: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
) -> Result<Result<(), Attempt>, FetchError> {
    let mut have = part.metadata().map_or(0, |m| m.len());
    if wanted.size > 0 && have > wanted.size {
        let _ = std::fs::remove_file(part);
        have = 0;
    }
    if wanted.size > 0 && have == wanted.size {
        if verify_and_install(part, wanted)? {
            return Ok(Ok(()));
        }
        have = 0;
    }
    let reply = match transport.get(url, have) {
        Ok(reply) => reply,
        Err(Failure::Unreachable(_)) => return Ok(Err(Attempt::Unreachable)),
        Err(Failure::Other(why)) => return Ok(Err(Attempt::Broken(why))),
    };
    if reply.status == 416 && have > 0 {
        // Nothing past what is there: it may be whole already.
        if verify_and_install(part, wanted)? {
            return Ok(Ok(()));
        }
        return Ok(Err(Attempt::Broken(RESUMED_WRONG.to_string())));
    }
    if !(200..300).contains(&reply.status) {
        let why = match reply.status {
            404 | 410 => "file đã bị gỡ".to_string(),
            401 | 403 => "không cho tải".to_string(),
            code => format!("lỗi HTTP {code}"),
        };
        return Ok(Err(Attempt::Broken(why)));
    }
    if reply.html {
        return Ok(Err(Attempt::Broken(
            "trả về trang web thay vì file".to_string(),
        )));
    }
    let start = if reply.resumed { have } else { 0 };
    if let (Some(total), true) = (reply.total, wanted.size > 0)
        && total != wanted.size
    {
        return Ok(Err(Attempt::Broken(format!(
            "cỡ file {total} byte, cần {} byte",
            wanted.size
        ))));
    }
    let total = reply.total.unwrap_or(wanted.size);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(start > 0)
        .write(true)
        .truncate(start == 0)
        .open(part)
        .map_err(|e| FetchError::Disk(e.to_string()))?;
    let mut body = reply.body;
    let mut done = start;
    let mut buf = vec![0u8; 1 << 16];
    progress(done, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = file.flush();
            return Err(FetchError::Cancelled);
        }
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                let _ = file.flush();
                return Ok(Err(Attempt::Interrupted));
            }
        };
        let chunk = buf.get(..n).unwrap_or_default();
        file.write_all(chunk)
            .map_err(|e| FetchError::Disk(e.to_string()))?;
        done += n as u64;
        progress(done, total);
    }
    file.flush().map_err(|e| FetchError::Disk(e.to_string()))?;
    drop(file);
    if total > 0 && done < total {
        return Ok(Err(Attempt::Interrupted));
    }
    if verify_and_install(part, wanted)? {
        return Ok(Ok(()));
    }
    Ok(Err(Attempt::Broken(if start > 0 {
        RESUMED_WRONG.to_string()
    } else {
        "sai mã kiểm tra SHA-256".to_string()
    })))
}

/// What fetching a model takes, said before it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub id: String,
    pub name: String,
    /// Bytes of everything to fetch (measured, or approximate).
    pub bytes: u64,
    /// Bytes already here: files in place and parts kept.
    pub have: u64,
    /// Whether the app fetches it, and if not, why not.
    pub downloadable: Result<(), String>,
}

impl Plan {
    /// "~200 MB, khoảng 3 phút với mạng 10 Mbit/giây".
    pub fn text(&self, megabits_per_second: f64) -> String {
        let left = self.bytes.saturating_sub(self.have);
        format!(
            "{}, khoảng {} với mạng {} Mbit/giây",
            size_text(left),
            duration_text(estimate_seconds(left, megabits_per_second)),
            megabits_per_second
        )
    }
}

/// Seconds `bytes` take at `megabits_per_second`.
pub fn estimate_seconds(bytes: u64, megabits_per_second: f64) -> u64 {
    if megabits_per_second <= 0.0 {
        return 0;
    }
    (bytes as f64 * 8.0 / (megabits_per_second * 1_000_000.0)).ceil() as u64
}

/// "~200 MB", "1,6 GB", "230 KB".
pub fn size_text(bytes: u64) -> String {
    const GB: u64 = 1_000_000_000;
    const MB: u64 = 1_000_000;
    if bytes >= GB {
        let tenths = (bytes + GB / 20) / (GB / 10);
        format!("{},{} GB", tenths / 10, tenths % 10)
    } else if bytes >= MB {
        format!("{} MB", (bytes + MB / 2) / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1000).max(1))
    }
}

fn duration_text(seconds: u64) -> String {
    if seconds < 60 {
        "dưới 1 phút".to_string()
    } else if seconds < 3600 {
        format!("{} phút", seconds.div_ceil(60))
    } else {
        format!("{} giờ {} phút", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// The files of a model, and where each goes under `folder`.
fn files_of(
    entry: &Entry,
    folder: &Path,
) -> Vec<(PathBuf, String, u64, Vec<(String, String)>, u64)> {
    let dir = folder.join(&entry.dir);
    let sources = |list: &[super::list::Source]| {
        list.iter()
            .filter(|s| s.usable())
            .map(|s| (s.from.clone(), s.url.trim().to_string()))
            .collect::<Vec<_>>()
    };
    let approx = |size: u64, approx_mb: u32| {
        if size > 0 {
            size
        } else {
            u64::from(approx_mb) * super::catalog::MB
        }
    };
    std::iter::once((
        dir.join(&entry.file),
        entry.sha256.clone(),
        entry.size,
        sources(&entry.sources),
        approx(entry.size, entry.approx_mb),
    ))
    .chain(entry.companions.iter().map(|c| {
        (
            dir.join(&c.file),
            c.sha256.clone(),
            c.size,
            sources(&c.sources),
            approx(c.size, c.approx_mb),
        )
    }))
    .collect()
}

/// What fetching the model `id` of `list` into `folder` takes.
pub fn plan_in(list: &ModelList, id: &str, folder: &Path) -> Option<Plan> {
    let entry = list.entry(id)?;
    let files = files_of(entry, folder);
    let have = files
        .iter()
        .map(|(dest, ..)| {
            dest.metadata()
                .or_else(|_| part_path(dest).metadata())
                .map_or(0, |m| m.len())
        })
        .sum();
    Some(Plan {
        id: entry.id.clone(),
        name: entry.name.clone(),
        bytes: entry.total_bytes(),
        have,
        downloadable: entry.downloadable(),
    })
}

/// Fetch the model `id` of `list` into `folder` (its subfolder there):
/// every file of it that is not already there and right. `progress` hears
/// the bytes there are and will be, over all its files. Returns the model's
/// path.
pub fn download_in(
    transport: &dyn Transport,
    list: &ModelList,
    id: &str,
    folder: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
) -> Result<PathBuf, FetchError> {
    let entry = list
        .entry(id)
        .ok_or_else(|| FetchError::Unknown(id.to_string()))?;
    entry.downloadable().map_err(FetchError::NotDownloadable)?;
    let files = files_of(entry, folder);
    let all: u64 = files.iter().map(|f| f.4).sum();
    let mut before = 0u64;
    for (dest, sha256, size, sources, expected) in &files {
        let in_place = dest.is_file()
            && super::with_cache(|cache| cache.sha256(dest).ok()).as_deref() == Some(sha256);
        if !in_place {
            let wanted = Wanted {
                dest,
                sha256,
                size: *size,
                sources: sources.clone(),
            };
            let report = |done: u64, _total: u64| progress(before + done, all);
            fetch_file(transport, &wanted, &report, cancel)?;
            super::with_cache(|cache| cache.remember_now(dest, sha256));
        }
        before += expected;
        progress(before.min(all), all);
    }
    super::forget_found();
    super::rescan();
    files
        .first()
        .map(|f| f.0.clone())
        .ok_or_else(|| FetchError::Unknown(id.to_string()))
}

/// What fetching `slot`'s model takes on this machine.
pub fn plan(slot: super::Slot) -> Option<Plan> {
    plan_in(
        &super::list::current(),
        slot.model().id,
        &super::main_folder(),
    )
}

/// Fetch `slot`'s model on this machine, from its sources in the author's
/// list as last read (read again first if it is old), into the main model
/// folder, on the calling thread.
pub fn download(
    slot: super::Slot,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
) -> Result<PathBuf, FetchError> {
    let http = Http::new().map_err(FetchError::Disk)?;
    if let Ok(quick) = Http::quick() {
        let _ = super::list::refresh(&quick);
    }
    download_in(
        &http,
        &super::list::current(),
        slot.model().id,
        &super::main_folder(),
        progress,
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A test's network: what each address answers, and what was asked.
    #[derive(Default)]
    struct Fake {
        files: HashMap<String, Served>,
        asked: Mutex<Vec<(String, u64)>>,
    }

    #[derive(Clone)]
    enum Served {
        File(Vec<u8>),
        /// The file, cut off after this many bytes of the body.
        CutAfter(Vec<u8>, usize),
        /// The file, ignoring Range.
        NoRange(Vec<u8>),
        Page,
        Status(u16),
        Unreachable,
    }

    /// A body that fails after its bytes.
    struct Cut(std::io::Cursor<Vec<u8>>);
    impl Read for Cut {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.read(buf)? {
                0 => Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "cut",
                )),
                n => Ok(n),
            }
        }
    }

    impl Transport for Fake {
        fn get(&self, url: &str, from: u64) -> Result<Reply, Failure> {
            self.asked.lock().unwrap().push((url.to_string(), from));
            let reply = |status, resumed, total, body: Box<dyn Read + Send>| Reply {
                status,
                resumed,
                total: Some(total),
                html: false,
                body,
            };
            match self.files.get(url).cloned() {
                None => Err(Failure::Unreachable("no such host".into())),
                Some(Served::Unreachable) => Err(Failure::Unreachable("down".into())),
                Some(Served::Status(code)) => Ok(Reply {
                    status: code,
                    resumed: false,
                    total: None,
                    html: false,
                    body: Box::new(std::io::empty()),
                }),
                Some(Served::Page) => Ok(Reply {
                    status: 200,
                    resumed: false,
                    total: None,
                    html: true,
                    body: Box::new(std::io::Cursor::new(b"<html>".to_vec())),
                }),
                Some(Served::File(bytes)) => {
                    let from = from as usize;
                    if from >= bytes.len() && from > 0 {
                        return Ok(Reply {
                            status: 416,
                            resumed: false,
                            total: None,
                            html: false,
                            body: Box::new(std::io::empty()),
                        });
                    }
                    let total = bytes.len() as u64;
                    Ok(reply(
                        if from > 0 { 206 } else { 200 },
                        from > 0,
                        total,
                        Box::new(std::io::Cursor::new(bytes[from..].to_vec())),
                    ))
                }
                Some(Served::NoRange(bytes)) => {
                    let total = bytes.len() as u64;
                    Ok(reply(
                        200,
                        false,
                        total,
                        Box::new(std::io::Cursor::new(bytes)),
                    ))
                }
                Some(Served::CutAfter(bytes, n)) => {
                    let from = from as usize;
                    let end = (from + n).min(bytes.len());
                    let total = bytes.len() as u64;
                    Ok(reply(
                        if from > 0 { 206 } else { 200 },
                        from > 0,
                        total,
                        Box::new(Cut(std::io::Cursor::new(bytes[from..end].to_vec()))),
                    ))
                }
            }
        }
    }

    fn sha(bytes: &[u8]) -> String {
        let mut h = hmac_sha256::Hash::new();
        h.update(bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn temp(name: &str) -> PathBuf {
        super::super::recognize::tests::temp(&format!("fetch_{name}"))
    }

    fn model() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn sources(urls: &[&str]) -> Vec<(String, String)> {
        urls.iter()
            .map(|u| ("test".to_string(), u.to_string()))
            .collect()
    }

    #[test]
    fn a_source_with_the_wrong_file_is_passed_over_for_the_next() {
        let dir = temp("next");
        let good = model();
        let mut bad = good.clone();
        bad[100] ^= 1;
        let fake = Fake {
            files: HashMap::from([
                ("https://author/m.onnx".to_string(), Served::Status(404)),
                ("https://github/m.onnx".to_string(), Served::File(bad)),
                (
                    "https://drive/m.onnx".to_string(),
                    Served::File(good.clone()),
                ),
            ]),
            ..Fake::default()
        };
        let dest = dir.join("m.onnx");
        let wanted = Wanted {
            dest: &dest,
            sha256: &sha(&good),
            size: good.len() as u64,
            sources: sources(&[
                "https://author/m.onnx",
                "https://github/m.onnx",
                "https://drive/m.onnx",
            ]),
        };
        fetch_file(&fake, &wanted, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), good);
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_download_cut_short_is_resumed_where_it_stopped() {
        let dir = temp("resume");
        let good = model();
        let dest = dir.join("m.onnx");
        let url = "https://author/m.onnx";
        let wanted = Wanted {
            dest: &dest,
            sha256: &sha(&good),
            size: good.len() as u64,
            sources: sources(&[url]),
        };
        let cut = Fake {
            files: HashMap::from([(url.to_string(), Served::CutAfter(good.clone(), 70_000))]),
            ..Fake::default()
        };
        // The line drops: that is no network, and what came is kept.
        let error = fetch_file(&cut, &wanted, &|_, _| {}, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error, FetchError::NoNetwork { partial: true });
        assert!(error.text().contains("tải tiếp"));
        assert_eq!(std::fs::metadata(part_path(&dest)).unwrap().len(), 70_000);
        // The next time it starts from there.
        let whole = Fake {
            files: HashMap::from([(url.to_string(), Served::File(good.clone()))]),
            ..Fake::default()
        };
        let seen = Mutex::new(Vec::new());
        fetch_file(
            &whole,
            &wanted,
            &|done, total| seen.lock().unwrap().push((done, total)),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            whole.asked.lock().unwrap().as_slice(),
            [(url.to_string(), 70_000)]
        );
        assert_eq!(std::fs::read(&dest).unwrap(), good);
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.first(), Some(&(70_000, good.len() as u64)));
        assert_eq!(seen.last(), Some(&(good.len() as u64, good.len() as u64)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_server_that_ignores_the_range_starts_the_file_over() {
        let dir = temp("norange");
        let good = model();
        let dest = dir.join("m.onnx");
        std::fs::write(part_path(&dest), &good[..1000]).unwrap();
        let fake = Fake {
            files: HashMap::from([("https://a/m".to_string(), Served::NoRange(good.clone()))]),
            ..Fake::default()
        };
        let wanted = Wanted {
            dest: &dest,
            sha256: &sha(&good),
            size: 0,
            sources: sources(&["https://a/m"]),
        };
        fetch_file(&fake, &wanted, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), good);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_spoilt_part_is_fetched_again_from_the_start() {
        let dir = temp("spoilt");
        let good = model();
        let dest = dir.join("m.onnx");
        let mut spoilt = good[..5000].to_vec();
        spoilt[10] ^= 0xff;
        std::fs::write(part_path(&dest), &spoilt).unwrap();
        let fake = Fake {
            files: HashMap::from([("https://a/m".to_string(), Served::File(good.clone()))]),
            ..Fake::default()
        };
        let wanted = Wanted {
            dest: &dest,
            sha256: &sha(&good),
            size: good.len() as u64,
            sources: sources(&["https://a/m"]),
        };
        fetch_file(&fake, &wanted, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            fake.asked.lock().unwrap().as_slice(),
            [
                ("https://a/m".to_string(), 5000),
                ("https://a/m".to_string(), 0)
            ]
        );
        assert_eq!(std::fs::read(&dest).unwrap(), good);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_network_is_told_apart_from_sources_that_are_broken() {
        let dir = temp("offline");
        let good = model();
        let dest = dir.join("m.onnx");
        let wanted = Wanted {
            dest: &dest,
            sha256: &sha(&good),
            size: good.len() as u64,
            sources: sources(&["https://a/m", "https://b/m", "https://c/m"]),
        };
        let offline = Fake::default();
        let error = fetch_file(&offline, &wanted, &|_, _| {}, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error, FetchError::NoNetwork { partial: false });
        assert!(error.text().contains("chưa có mạng"));
        assert!(!error.needs_contact());

        let broken = Fake {
            files: HashMap::from([
                ("https://a/m".to_string(), Served::Status(404)),
                ("https://b/m".to_string(), Served::Page),
                ("https://c/m".to_string(), Served::Unreachable),
            ]),
            ..Fake::default()
        };
        let error = fetch_file(&broken, &wanted, &|_, _| {}, &AtomicBool::new(false)).unwrap_err();
        match &error {
            FetchError::SourcesFailed(tried) => {
                assert_eq!(tried.len(), 3);
                assert!(tried[0].1.contains("gỡ"), "{tried:?}");
                assert!(tried[1].1.contains("trang web"), "{tried:?}");
            }
            other => panic!("{other:?}"),
        }
        assert!(error.needs_contact());
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cancelled_download_keeps_what_came() {
        let dir = temp("cancel");
        let good = model();
        let dest = dir.join("m.onnx");
        let fake = Fake {
            files: HashMap::from([("https://a/m".to_string(), Served::File(good.clone()))]),
            ..Fake::default()
        };
        let wanted = Wanted {
            dest: &dest,
            sha256: &sha(&good),
            size: good.len() as u64,
            sources: sources(&["https://a/m"]),
        };
        let cancel = AtomicBool::new(false);
        let error = fetch_file(
            &fake,
            &wanted,
            &|done, _| {
                if done > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert_eq!(error, FetchError::Cancelled);
        assert!(part_path(&dest).is_file());
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_model_of_the_list_is_fetched_with_its_weights_and_said_ahead() {
        let dir = temp("list");
        let (onnx, data) = (b"the model".to_vec(), model());
        let list = ModelList::parse(
            format!(
                r#"{{"schema": 1, "models": [{{
                  "id": "sapiens2-seg", "dir": "sapiens2-seg", "file": "s.onnx", "name": "S",
                  "sha256": "{}", "size": {}, "redistributable": true,
                  "sources": [{{"from": "github", "url": "TODO chủ điền"}},
                              {{"from": "author", "url": "https://drive/s.onnx"}}],
                  "companions": [{{"file": "s.onnx.data", "sha256": "{}", "approx_mb": 1,
                                  "sources": [{{"from": "author", "url": "https://drive/s.onnx.data"}}]}}]
                }}, {{"id": "gfpgan-1.4", "redistributable": false}}]}}"#,
                sha(&onnx),
                onnx.len(),
                sha(&data)
            )
            .as_bytes(),
        )
        .unwrap();
        let plan = plan_in(&list, "sapiens2-seg", &dir).unwrap();
        assert_eq!(plan.bytes, onnx.len() as u64 + super::super::catalog::MB);
        assert_eq!(plan.have, 0);
        assert!(plan.downloadable.is_ok());
        assert!(plan.text(10.0).contains("Mbit"), "{}", plan.text(10.0));
        let fake = Fake {
            files: HashMap::from([
                (
                    "https://drive/s.onnx".to_string(),
                    Served::File(onnx.clone()),
                ),
                (
                    "https://drive/s.onnx.data".to_string(),
                    Served::File(data.clone()),
                ),
            ]),
            ..Fake::default()
        };
        let path = download_in(
            &fake,
            &list,
            "sapiens2-seg",
            &dir,
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(path, dir.join("sapiens2-seg").join("s.onnx"));
        assert_eq!(
            std::fs::read(dir.join("sapiens2-seg").join("s.onnx.data")).unwrap(),
            data
        );
        // The placeholder was never asked for.
        assert!(
            fake.asked
                .lock()
                .unwrap()
                .iter()
                .all(|(u, _)| u.starts_with("https://drive/"))
        );

        // GFPGAN: pointed to, never fetched.
        let error = download_in(
            &fake,
            &list,
            "gfpgan-1.4",
            &dir,
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(matches!(error, FetchError::NotDownloadable(_)));
        assert!(error.needs_contact());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sizes_and_times_read_plainly() {
        assert_eq!(size_text(229_738), "230 KB");
        assert_eq!(size_text(200 * 1_000_000), "200 MB");
        assert_eq!(size_text(1_600_000_000), "1,6 GB");
        assert_eq!(estimate_seconds(200_000_000, 10.0), 160);
        assert_eq!(duration_text(160), "3 phút");
        assert_eq!(duration_text(30), "dưới 1 phút");
    }
}
