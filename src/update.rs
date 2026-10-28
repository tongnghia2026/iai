//! The app updates itself. It asks the project's releases whether a build
//! newer than this one has been published; once the user agrees it downloads
//! that build, checks every file against the SHA-256 published with it, and
//! puts it in this exe's place as the app closes.
//!
//! A build is known by the last commit that touched its code (`build.rs`).
//! `.github/workflows/release.yml` publishes one for every push of code:
//! `update.json` (a [`Release`]) and, beside it, the files it names.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Where the newest published build describes itself. `IAI_UPDATE_FEED` names
/// another place, to rehearse an update; `IAI_UPDATE=0` switches the check
/// made at startup off.
const FEED_URL: &str = "https://github.com/tongnghia2026/iai/releases/latest/download/update.json";

/// The files a release may replace, by the names they are installed under.
const EXE_NAME: &str = "iai.exe";
/// The library the exe loads its RAW decoders from (`formats::raw_library`).
const RAW_NAME: &str = "iai_raw.dll";
const PDB_NAME: &str = "iai.pdb";

/// The check at startup waits for the window to be up.
const STARTUP_DELAY: Duration = Duration::from_secs(3);
/// A download nobody came back for is dropped after this long.
const STALE: Duration = Duration::from_secs(24 * 3600);

/// A build of the app: the last commit that touched its code, and when that
/// commit was made (seconds since 1970; 0 when the build does not know).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Build {
    pub commit: &'static str,
    pub time: u64,
}

impl Build {
    /// When its code was last changed, as the user is told it.
    pub fn day(&self) -> String {
        day_of(self.time)
    }
}

/// The build this exe is.
pub fn this_build() -> Build {
    Build {
        commit: env!("IAI_BUILD_COMMIT"),
        time: env!("IAI_BUILD_TIME").parse().unwrap_or(0),
    }
}

fn day_of(time: u64) -> String {
    if time == 0 {
        return "?".to_owned();
    }
    crate::diag::local_minute(SystemTime::UNIX_EPOCH + Duration::from_secs(time))
}

/// What is new, build by build, as its author wrote it for users: this
/// build's own, newest first.
pub const OWN_NEWS: &str = include_str!("../assets/whats_new.txt");
/// How many of its newest entries a build carries in its `update.json`.
const NEWS_CARRIED: usize = 8;

/// One entry of `assets/whats_new.txt`: its date, and its points.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct News {
    pub date: String,
    pub points: Vec<String>,
}

/// The entries of a `whats_new.txt`, newest first: a date line, then its
/// points ("- …"), a blank line between entries; `#` lines are notes to the
/// writer.
pub fn news_of(text: &str) -> Vec<News> {
    let mut out: Vec<News> = Vec::new();
    let mut open = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if line.is_empty() {
            open = false;
            continue;
        }
        match out.last_mut() {
            Some(news) if open => news
                .points
                .push(line.trim_start_matches('-').trim().to_string()),
            _ => {
                out.push(News {
                    date: line.to_string(),
                    points: Vec::new(),
                });
                open = true;
            }
        }
    }
    out
}

/// The entries `offered` (a published build's) that `own` (this build's)
/// does not have word for word: what the update brings.
pub fn news_unseen(offered: &[News], own: &[News]) -> Vec<News> {
    offered
        .iter()
        .filter(|news| !own.contains(news))
        .cloned()
        .collect()
}

/// A published build, as its `update.json` describes it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub commit: String,
    pub time: u64,
    pub files: Vec<ReleaseFile>,
    /// Its newest entries of what is new (none from builds made before they
    /// were carried).
    #[serde(default)]
    pub notes: Vec<News>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ReleaseFile {
    /// What it is once installed: `iai.exe`, `iai_raw.dll` or `iai.pdb`.
    pub name: String,
    /// Its file beside `update.json`, gzipped when the name ends in `.gz`.
    pub asset: String,
    /// Bytes to download.
    pub bytes: u64,
    /// SHA-256 of the installed file, in hex.
    pub sha256: String,
}

impl Release {
    /// When its code was last changed, as the user is told it.
    pub fn day(&self) -> String {
        day_of(self.time)
    }

    /// Megabytes to download.
    pub fn megabytes(&self) -> u64 {
        self.wanted()
            .map(|file| file.bytes)
            .sum::<u64>()
            .div_ceil(1_000_000)
    }

    /// The files this app installs, the exe first.
    fn wanted(&self) -> impl Iterator<Item = &ReleaseFile> {
        [EXE_NAME, RAW_NAME, PDB_NAME]
            .into_iter()
            .filter_map(|name| self.files.iter().find(|file| file.name == name))
    }

    fn newer_than(&self, build: Build) -> bool {
        self.time > build.time
    }
}

/// Where the update stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Nothing asked, or nothing to say.
    Idle,
    Checking,
    /// The user asked, and the published build is no newer than this one.
    Current,
    Available(Release),
    /// How much of it has come, 0 to 1.
    Downloading(Release, f32),
    /// Downloaded and checked: it takes this exe's place as the app closes.
    Ready(Release),
    /// The user asked and no answer could be had; why, in their words.
    CheckFailed(String),
    /// The build on offer did not come whole; why, in the user's words.
    DownloadFailed(String),
}

struct Shared {
    status: Status,
    /// `status` has changed since the window last followed it up.
    unseen: bool,
    /// The newer build the last check found, for a download that failed to
    /// fall back on.
    offered: Option<Release>,
    staged: Option<Staged>,
    /// The user asked for the app back once it has closed and updated.
    relaunch: bool,
}

fn shared() -> MutexGuard<'static, Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            Mutex::new(Shared {
                status: Status::Idle,
                unseen: false,
                offered: None,
                staged: None,
                relaunch: false,
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn status() -> Status {
    shared().status.clone()
}

/// The status, taken as followed up: what [`busy`] waits for.
pub fn observe() -> Status {
    let mut shared = shared();
    shared.unseen = false;
    shared.status.clone()
}

/// A check or a download is under way, or its outcome has not been followed
/// up yet: the window has to keep looking in.
pub fn busy() -> bool {
    let shared = shared();
    shared.unseen || matches!(shared.status, Status::Checking | Status::Downloading(..))
}

fn set_status(status: Status) {
    let mut shared = shared();
    shared.status = status;
    shared.unseen = true;
}

fn feed() -> String {
    std::env::var("IAI_UPDATE_FEED")
        .ok()
        .filter(|feed| !feed.trim().is_empty())
        .unwrap_or_else(|| FEED_URL.to_owned())
}

/// The check made as the app starts: it says something only when a newer
/// build is there.
pub fn check_at_startup() {
    if cfg!(test) || std::env::var_os("IAI_UPDATE").is_some_and(|v| v == "0") {
        return;
    }
    check(false);
}

/// Ask, off this thread, whether a newer build is published. `asked` is the
/// user asking: then "none" and "could not ask" are answers too.
pub fn check(asked: bool) {
    // Releases hold the Windows build only.
    if !cfg!(windows) {
        if asked {
            set_status(Status::CheckFailed(
                "Tự cập nhật hiện chỉ có trên Windows.".to_owned(),
            ));
        }
        return;
    }
    {
        let mut shared = shared();
        if matches!(
            shared.status,
            Status::Checking | Status::Downloading(..) | Status::Ready(_)
        ) {
            return;
        }
        shared.status = Status::Checking;
    }
    std::thread::spawn(move || {
        if !asked {
            std::thread::sleep(STARTUP_DELAY);
        }
        let found = fetch_release(&feed());
        if let Ok(Some(release)) = &found {
            mend_library(release);
        }
        let status = match found {
            Ok(Some(release)) if release.newer_than(this_build()) => {
                crate::diag::note("update", &format!("build {} is published", release.commit));
                Status::Available(release)
            }
            // Nothing newer, or nothing published at all.
            Ok(_) if asked => Status::Current,
            Ok(_) => Status::Idle,
            Err(why) if asked => Status::CheckFailed(why),
            Err(_) => Status::Idle,
        };
        let mut shared = shared();
        if let Status::Available(release) = &status {
            shared.offered = Some(release.clone());
        }
        shared.status = status;
        shared.unseen = true;
    });
}

/// The user has read an answer that asks nothing more of them ("none newer",
/// "could not ask"): it is not kept on show. A download that failed leaves
/// the build on offer.
pub fn dismiss() {
    let mut shared = shared();
    let left = match (&shared.status, &shared.offered) {
        (Status::DownloadFailed(_), Some(release)) => Status::Available(release.clone()),
        (Status::DownloadFailed(_) | Status::CheckFailed(_) | Status::Current, _) => Status::Idle,
        _ => return,
    };
    shared.status = left;
    shared.unseen = true;
}

/// Download the build [`check`] found, off this thread; again, when the
/// last attempt failed.
pub fn download() {
    let release = {
        let mut shared = shared();
        let release = match (&shared.status, &shared.offered) {
            (Status::Available(release), _) => release.clone(),
            (Status::DownloadFailed(_), Some(release)) => release.clone(),
            _ => return,
        };
        shared.status = Status::Downloading(release.clone(), 0.0);
        release
    };
    std::thread::spawn(move || {
        crate::diag::note("update", &format!("downloading {}", release.commit));
        let progress = |done: f32| set_status(Status::Downloading(release.clone(), done));
        let staged = std::env::current_exe()
            .map_err(|e| format!("Không tìm thấy file chạy của app: {e}"))
            .and_then(|exe| fetch_files(&feed(), &release, &exe, &progress));
        let mut shared = shared();
        match staged {
            Ok(staged) => {
                crate::diag::note("update", "downloaded and checked");
                shared.staged = Some(staged);
                shared.status = Status::Ready(release);
            }
            Err(why) => shared.status = Status::DownloadFailed(why),
        }
        shared.unseen = true;
    });
}

/// Whether the app is to come back once it has closed and updated.
pub fn set_relaunch(wanted: bool) {
    shared().relaunch = wanted;
}

pub(crate) fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("iAi/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("Không mở được kết nối mạng: {e}"))
}

/// What the user is told when the network lets the update down; the reason
/// goes to the journal.
fn offline(doing: &str, why: &dyn std::fmt::Display) -> String {
    crate::diag::note("update", &format!("{doing}: {why}"));
    "Không kết nối được mạng. Kiểm tra mạng rồi thử lại.".to_owned()
}

/// What the user is told when the server answers, but not with a release
/// this app can read: nothing their own network could mend.
fn odd_server(doing: &str, why: &dyn std::fmt::Display) -> String {
    crate::diag::note("update", &format!("{doing}: {why}"));
    "Máy chủ cập nhật đang trục trặc. Hãy thử lại sau.".to_owned()
}

/// The build published last; `None` while nothing has been published.
fn fetch_release(feed: &str) -> Result<Option<Release>, String> {
    let response = client(Duration::from_secs(20))?
        .get(feed)
        .send()
        .map_err(|e| offline("asking for the published build", &e))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        crate::diag::note("update", "nothing is published yet");
        return Ok(None);
    }
    let text = response
        .error_for_status()
        .map_err(|e| odd_server("asking for the published build", &e))?
        .text()
        .map_err(|e| offline("reading update.json", &e))?;
    let release: Release =
        serde_json::from_str(&text).map_err(|e| odd_server("reading update.json", &e))?;
    if release.wanted().next().map(|file| file.name.as_str()) != Some(EXE_NAME) {
        return Err(odd_server("reading update.json", &"it names no iai.exe"));
    }
    Ok(Some(release))
}

/// The address of `asset`, which lies beside the feed. An asset is a bare
/// file name: nothing in a release can send the download elsewhere.
fn beside(feed: &str, asset: &str) -> Result<String, String> {
    let bare = !asset.is_empty()
        && !asset.starts_with('.')
        && asset
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    match feed.rfind('/') {
        Some(slash) if bare => Ok(format!("{}{asset}", &feed[..=slash])),
        _ => Err(odd_server(
            "reading update.json",
            &format!("asset {asset:?}"),
        )),
    }
}

/// An app from before the RAW library was a file of its own knows nothing of
/// that file: once updated, it is this exe without its library. This build
/// then fetches the library itself, from the release that is this very build
/// (a newer release brings its own, with its exe).
fn mend_library(release: &Release) {
    let Some(target) = crate::formats::raw_library::missing_file() else {
        return;
    };
    match fetch_library(&feed(), release, this_build(), &target) {
        Ok(true) => crate::diag::note("update", "the RAW library was missing: fetched"),
        Ok(false) => crate::diag::note("update", "the RAW library is missing"),
        Err(why) => crate::diag::note(
            "update",
            &format!("the RAW library is missing and was not fetched: {why}"),
        ),
    }
}

/// Download the RAW library of `release` to `target`, where there is none.
/// `Ok(false)`: the release is not `build`, or holds no library.
fn fetch_library(
    feed: &str,
    release: &Release,
    build: Build,
    target: &Path,
) -> Result<bool, String> {
    let file = release.files.iter().find(|file| file.name == RAW_NAME);
    let Some(file) = file.filter(|_| release.commit == build.commit) else {
        return Ok(false);
    };
    let waiting = waiting_name(target, std::process::id());
    let client = client(Duration::from_secs(3600))?;
    let url = beside(feed, &file.asset)?;
    fetch_one(&client, &url, file, &waiting, &Arc::default(), &|| {})?;
    std::fs::rename(&waiting, target).map_err(|e| {
        let _ = std::fs::remove_file(&waiting);
        format!("Không ghi được file: {e}")
    })?;
    Ok(true)
}

/// A downloaded build: each file as (where it waits, the name it takes).
#[derive(Debug)]
struct Staged {
    exe: (PathBuf, PathBuf),
    library: Option<(PathBuf, PathBuf)>,
    pdb: Option<(PathBuf, PathBuf)>,
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Where a downloaded file waits for the app to close.
fn waiting_name(target: &Path, pid: u32) -> PathBuf {
    suffixed(target, &format!(".update-{pid}"))
}

/// Where a file that made way is kept until the new app has started.
fn old_name(file: &Path, pid: u32) -> PathBuf {
    suffixed(file, &format!(".old-{pid}"))
}

/// Where symbols wait when the old ones were still held open.
fn pending_name(pdb: &Path) -> PathBuf {
    suffixed(pdb, ".new")
}

struct Counted<R> {
    inner: R,
    done: Arc<AtomicU64>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.done.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

/// Download the files of `release` to wait beside `exe`. The exe has to come
/// whole, and so has the library it reads RAW files with; the app runs
/// without its symbols.
fn fetch_files(
    feed: &str,
    release: &Release,
    exe: &Path,
    progress: &dyn Fn(f32),
) -> Result<Staged, String> {
    let pid = std::process::id();
    let total = release.wanted().map(|file| file.bytes).sum::<u64>().max(1);
    let done = Arc::new(AtomicU64::new(0));
    let client = client(Duration::from_secs(3600))?;
    let mut staged: Option<Staged> = None;
    for file in release.wanted() {
        let name = file.name.as_str();
        let target = if name == EXE_NAME {
            exe.to_path_buf()
        } else {
            exe.with_file_name(name)
        };
        let waiting = waiting_name(&target, pid);
        let url = beside(feed, &file.asset)?;
        let report = || progress((done.load(Ordering::Relaxed) as f32 / total as f32).min(1.0));
        match fetch_one(&client, &url, file, &waiting, &done, &report) {
            Ok(()) if name == EXE_NAME => {
                staged = Some(Staged {
                    exe: (waiting, target),
                    library: None,
                    pdb: None,
                })
            }
            Ok(()) => {
                if let Some(staged) = &mut staged {
                    if name == RAW_NAME {
                        staged.library = Some((waiting, target));
                    } else {
                        staged.pdb = Some((waiting, target));
                    }
                }
            }
            Err(why) if name == PDB_NAME => {
                crate::diag::note("update", &format!("symbols not fetched: {why}"))
            }
            Err(why) => {
                if let Some(staged) = &staged {
                    let _ = std::fs::remove_file(&staged.exe.0);
                }
                return Err(why);
            }
        }
    }
    staged.ok_or_else(|| odd_server("reading update.json", &"it names no iai.exe"))
}

/// One file of a release, unpacked to `waiting` once it is the file its
/// SHA-256 names.
fn fetch_one(
    client: &reqwest::blocking::Client,
    url: &str,
    file: &ReleaseFile,
    waiting: &Path,
    done: &Arc<AtomicU64>,
    report: &dyn Fn(),
) -> Result<(), String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| offline("downloading", &e))?
        .error_for_status()
        .map_err(|e| odd_server("downloading", &e))?;
    let counted = Counted {
        inner: response,
        done: done.clone(),
    };
    let mut source: Box<dyn Read> = if file.asset.ends_with(".gz") {
        Box::new(flate2::read::GzDecoder::new(counted))
    } else {
        Box::new(counted)
    };
    let part = suffixed(waiting, ".part");
    let mut out = std::fs::File::create(&part).map_err(|e| {
        format!(
            "Không ghi được vào thư mục chứa app ({}): {e}",
            part.parent().unwrap_or(Path::new("?")).display()
        )
    })?;
    let mut hasher = hmac_sha256::Hash::new();
    let mut buffer = vec![0u8; 1 << 16];
    let written = loop {
        match source.read(&mut buffer) {
            Ok(0) => break out.flush().map_err(|e| format!("Không ghi được file: {e}")),
            Ok(read) => {
                hasher.update(&buffer[..read]);
                if let Err(e) = out.write_all(&buffer[..read]) {
                    break Err(format!("Không ghi được file (ổ đĩa đầy?): {e}"));
                }
                report();
            }
            Err(e) => break Err(offline("downloading", &e)),
        }
    };
    drop(out);
    let sha256: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let installed = written.and_then(|()| {
        if sha256 != file.sha256.to_ascii_lowercase() {
            crate::diag::note(
                "update",
                &format!("{} is not the file published", file.name),
            );
            return Err("File tải về không đúng — thử lại.".to_owned());
        }
        std::fs::rename(&part, waiting).map_err(|e| format!("Không ghi được file: {e}"))
    });
    if installed.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    installed
}

/// A file a new one has taken the place of. A running exe and a loaded
/// library cannot be deleted, but they can be renamed: what was there is set
/// aside under [`old_name`], for the next start to delete (see `tidy`).
struct Replaced<'a> {
    target: &'a Path,
    old: Option<PathBuf>,
}

impl<'a> Replaced<'a> {
    /// Put `new` at `target`; should it not go in, what was there stays.
    fn put(new: &Path, target: &'a Path, pid: u32) -> std::io::Result<Self> {
        let old = old_name(target, pid);
        let had = match std::fs::rename(target, &old) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        };
        if let Err(e) = std::fs::rename(new, target) {
            if had {
                let _ = std::fs::rename(&old, target);
            }
            return Err(e);
        }
        Ok(Self {
            target,
            old: had.then_some(old),
        })
    }

    /// Take the new file out again and put back what was there.
    fn undo(self) {
        let _ = std::fs::remove_file(self.target);
        if let Some(old) = self.old {
            let _ = std::fs::rename(old, self.target);
        }
    }
}

/// Put `staged` in the app's place. The RAW library goes in first and comes
/// back out should the exe not follow it, so that the app still starts, and
/// never beside the library of another build.
fn swap(staged: &Staged, pid: u32) -> Result<(), String> {
    let library = match &staged.library {
        Some((new, library)) => Some(
            Replaced::put(new, library, pid)
                .map_err(|e| format!("putting the new RAW library in place: {e}"))?,
        ),
        None => None,
    };
    let (new, exe) = &staged.exe;
    if let Err(e) = Replaced::put(new, exe, pid) {
        if let Some(library) = library {
            library.undo();
        }
        return Err(format!("putting the new exe in place: {e}"));
    }
    if let Some((new, pdb)) = &staged.pdb {
        // The flight recorder's watcher may still hold the old symbols open:
        // then the new ones wait for the next start (see `tidy`).
        if std::fs::rename(new, pdb).is_err() {
            let _ = std::fs::rename(new, pending_name(pdb));
        }
    }
    Ok(())
}

/// Install the downloaded build, if there is one. Call on the way out of
/// `main`, the window gone. Returns the exe to start when the user asked for
/// the app back (see [`relaunch`]).
pub fn apply_on_exit() -> Option<PathBuf> {
    let (staged, relaunch) = {
        let mut shared = shared();
        (shared.staged.take()?, shared.relaunch)
    };
    match swap(&staged, std::process::id()) {
        Ok(()) => {
            crate::diag::note("update", "installed; the next start runs it");
            relaunch.then(|| staged.exe.1.clone())
        }
        Err(why) => {
            crate::diag::note("update", &format!("not installed: {why}"));
            None
        }
    }
}

/// Start the app again. Call last, from `main`.
pub fn relaunch(exe: &Path) {
    let _ = std::process::Command::new(exe).spawn();
}

/// Tidy up after an update. Call as the app starts, before the flight
/// recorder opens the symbols.
pub fn finish_previous() {
    if let Ok(exe) = std::env::current_exe() {
        tidy(&exe, SystemTime::now());
    }
}

/// Beside `exe`: symbols that could not be replaced as the app closed are
/// now, the exe and the RAW library that made way are deleted, and downloads
/// nobody came back for are dropped.
fn tidy(exe: &Path, now: SystemTime) {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let pdb = dir.join(PDB_NAME);
    let pending = pending_name(&pdb);
    if pending.exists() {
        let _ = std::fs::rename(&pending, &pdb);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let old = [format!("{name}.old-"), format!("{RAW_NAME}.old-")];
    let waiting = [
        format!("{name}.update-"),
        format!("{RAW_NAME}.update-"),
        format!("{PDB_NAME}.update-"),
    ];
    let mut set_aside = Vec::new();
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else {
            continue;
        };
        if old.iter().any(|prefix| file.starts_with(prefix)) {
            set_aside.push(entry.path());
        } else if waiting.iter().any(|prefix| file.starts_with(prefix)) {
            // Younger ones may be another window of the app downloading now.
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| now.duration_since(at).ok())
                .is_some_and(|age| age > STALE);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    set_aside.retain(|path| std::fs::remove_file(path).is_err());
    if !set_aside.is_empty() {
        // The app that made way may still be closing.
        std::thread::spawn(move || {
            for _ in 0..10 {
                std::thread::sleep(Duration::from_secs(1));
                set_aside.retain(|path| std::fs::remove_file(path).is_err());
                if set_aside.is_empty() {
                    break;
                }
            }
        });
    }
}

/// `iai.exe --build-info` prints which build the exe is and the library file
/// it loads its RAW decoders from (`null` when they are linked in), and ends:
/// the release workflow describes what it publishes from the exe itself.
pub fn print_build_info_if_requested() -> bool {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--build-info")) {
        return false;
    }
    println!("{}", build_info());
    true
}

/// What `--build-info` prints: this build, the library it loads, and its
/// newest entries of what is new. Escaped to ASCII, so a console that reads
/// it in another code page cannot spoil the Vietnamese.
fn build_info() -> String {
    let build = this_build();
    let library = cfg!(feature = "raw-dll").then_some(RAW_NAME);
    let notes: Vec<News> = news_of(OWN_NEWS).into_iter().take(NEWS_CARRIED).collect();
    let info = serde_json::json!({
        "commit": build.commit,
        "time": build.time,
        "library": library,
        "notes": notes,
    });
    let mut out = String::new();
    for c in info.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::BufRead;

    /// A folder of its own under the temp directory, removed when dropped.
    pub(crate) struct TempFolder(pub(crate) PathBuf);

    impl TempFolder {
        pub(crate) fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("iai_update_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub(crate) fn file(&self, name: &str, content: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, content).unwrap();
            path
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A web server for one test: `files` by name, at the address returned,
    /// for as long as the test process lives.
    pub(crate) fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = std::io::BufReader::new(stream);
                let mut request = String::new();
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                    if request.is_empty() {
                        request = line.clone();
                    }
                    line.clear();
                }
                let asked = request
                    .split(' ')
                    .nth(1)
                    .unwrap_or("/")
                    .trim_start_matches('/');
                let mut stream = reader.into_inner();
                let _ = match files.iter().find(|(name, _)| *name == asked) {
                    Some((_, body)) => stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .and_then(|()| stream.write_all(body)),
                    None => stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    ),
                };
            }
        });
        address
    }

    fn gzipped(content: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    fn sha256(content: &[u8]) -> String {
        hmac_sha256::Hash::hash(content)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// A release of `exe` (gzipped) and `pdb` (as it is), and its update.json.
    fn published(exe: &[u8], pdb: &[u8], time: u64) -> (String, Vec<u8>, Vec<u8>) {
        let packed = gzipped(exe);
        let json = format!(
            r#"{{"commit":"abc123","time":{time},"files":[
                {{"name":"iai.pdb","asset":"iai.pdb","bytes":{},"sha256":"{}"}},
                {{"name":"iai.exe","asset":"iai.exe.gz","bytes":{},"sha256":"{}"}},
                {{"name":"readme.txt","asset":"readme.txt","bytes":5,"sha256":"00"}}]}}"#,
            pdb.len(),
            sha256(pdb),
            packed.len(),
            sha256(exe),
        );
        (json, packed, pdb.to_vec())
    }

    /// The same release, with the RAW `library` (gzipped) the exe loads.
    fn published_with_library(exe: &[u8], library: &[u8], time: u64) -> (String, Vec<u8>) {
        let (json, _, _) = published(exe, b"new symbols", time);
        let packed = gzipped(library);
        let entry = format!(
            r#""files":[{{"name":"iai_raw.dll","asset":"iai_raw.dll.gz","bytes":{},"sha256":"{}"}},"#,
            packed.len(),
            sha256(library),
        );
        (json.replacen(r#""files":["#, &entry, 1), packed)
    }

    #[test]
    fn entries_of_what_is_new_read_and_only_new_ones_are_offered() {
        let text = "# to the writer\n# more\n\n12/10/2026\n- one\n-  two \n\n11/10/2026\n- older\n";
        let news = news_of(text);
        assert_eq!(
            news,
            vec![
                News {
                    date: "12/10/2026".into(),
                    points: vec!["one".into(), "two".into()],
                },
                News {
                    date: "11/10/2026".into(),
                    points: vec!["older".into()],
                },
            ]
        );
        assert!(news_of("# nothing yet\n").is_empty());
        // The installed build has the older entry, and the newer one as it
        // was before a line was added to it the same day.
        let own = news_of("12/10/2026\n- one\n\n11/10/2026\n- older\n");
        assert_eq!(news_unseen(&news, &own), vec![news[0].clone()]);
        assert!(news_unseen(&news, &news).is_empty());
    }

    #[test]
    fn the_build_info_carries_the_newest_entries_in_ascii() {
        let info = build_info();
        assert!(info.is_ascii(), "{info}");
        let read: serde_json::Value = serde_json::from_str(&info).unwrap();
        assert_eq!(read["commit"], this_build().commit);
        let notes: Vec<News> = serde_json::from_value(read["notes"].clone()).unwrap();
        let own = news_of(OWN_NEWS);
        assert_eq!(
            notes,
            own.into_iter().take(NEWS_CARRIED).collect::<Vec<_>>()
        );
        assert!(notes.iter().any(|n| n.points.iter().any(|p| !p.is_ascii())));
    }

    #[test]
    fn a_release_reads_and_only_a_newer_one_is_offered() {
        let (json, packed, _) = published(b"new exe", b"new symbols", 2_000);
        let release: Release = serde_json::from_str(&json).unwrap();
        assert_eq!((release.commit.as_str(), release.time), ("abc123", 2_000));
        // The exe first, then its symbols; nothing else is installed.
        let wanted: Vec<&str> = release.wanted().map(|f| f.name.as_str()).collect();
        assert_eq!(wanted, ["iai.exe", "iai.pdb"]);
        assert_eq!(
            release.wanted().map(|f| f.bytes).sum::<u64>(),
            (packed.len() + b"new symbols".len()) as u64
        );
        assert_eq!(release.megabytes(), 1);

        let built = |time| Build {
            commit: "this",
            time,
        };
        assert!(release.newer_than(built(1_999)));
        assert!(!release.newer_than(built(2_000)));
        assert!(!release.newer_than(built(2_001)));
        // A build that does not know its own age takes whatever is published.
        assert!(release.newer_than(built(0)));
        assert_eq!(built(0).day(), "?");
        assert_eq!(release.day().len(), "12/10/2026 10:59".len());

        // This build knows itself wherever git could be asked.
        let this = this_build();
        if this.commit != "unknown" {
            assert!(this.time > 1_700_000_000, "{this:?}");
            assert_eq!(this.commit.len(), 40, "{this:?}");
        }
    }

    #[test]
    fn an_asset_lies_beside_the_feed_and_nowhere_else() {
        let feed = "https://example.org/releases/latest/download/update.json";
        assert_eq!(
            beside(feed, "iai.exe.gz").unwrap(),
            "https://example.org/releases/latest/download/iai.exe.gz"
        );
        for asset in [
            "",
            "../iai.exe",
            "a/b.gz",
            "https://elsewhere.example/iai.exe",
            ".hidden",
            "a b",
            "a\\b",
            "a?b=c",
        ] {
            assert!(beside(feed, asset).is_err(), "{asset:?}");
        }
        assert!(beside("update.json", "iai.exe").is_err());
    }

    #[test]
    fn a_build_is_downloaded_checked_and_takes_the_exes_place() {
        let (json, packed, pdb) = published(b"new exe", b"new symbols", 2_000);
        let feed = serve(vec![
            ("update.json", json.into_bytes()),
            ("iai.exe.gz", packed),
            ("iai.pdb", pdb),
        ]) + "update.json";
        let folder = TempFolder::new("install");
        let exe = folder.file("iai.exe", b"old exe");
        folder.file("iai.pdb", b"old symbols");
        folder.file("photo.iai", b"the user's work");

        let release = fetch_release(&feed).unwrap().expect("a release");
        let seen = Mutex::new(Vec::new());
        let staged = fetch_files(&feed, &release, &exe, &|done| {
            seen.lock().unwrap().push(done)
        })
        .unwrap();

        // Both wait under names of their own; what runs is untouched.
        let pid = std::process::id();
        assert_eq!(
            folder.names(),
            [
                "iai.exe".to_owned(),
                format!("iai.exe.update-{pid}"),
                "iai.pdb".to_owned(),
                format!("iai.pdb.update-{pid}"),
                "photo.iai".to_owned(),
            ]
        );
        assert_eq!(std::fs::read(&exe).unwrap(), b"old exe");
        assert_eq!(std::fs::read(&staged.exe.0).unwrap(), b"new exe");
        let seen = seen.into_inner().unwrap();
        assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
        assert_eq!(seen.last(), Some(&1.0));

        swap(&staged, pid).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new exe");
        assert_eq!(
            std::fs::read(folder.0.join("iai.pdb")).unwrap(),
            b"new symbols"
        );
        let old = folder.0.join(format!("iai.exe.old-{pid}"));
        assert_eq!(std::fs::read(&old).unwrap(), b"old exe");

        // The next start clears the exe that made way, and nothing else.
        tidy(&exe, SystemTime::now());
        assert_eq!(folder.names(), ["iai.exe", "iai.pdb", "photo.iai"]);
        assert_eq!(
            std::fs::read(folder.0.join("photo.iai")).unwrap(),
            b"the user's work"
        );
    }

    #[test]
    fn the_raw_library_comes_with_the_exe_and_takes_its_own_place() {
        let (json, library) = published_with_library(b"new exe", b"new library", 2_000);
        let feed = serve(vec![
            ("update.json", json.into_bytes()),
            ("iai.exe.gz", gzipped(b"new exe")),
            ("iai_raw.dll.gz", library),
            ("iai.pdb", b"new symbols".to_vec()),
        ]) + "update.json";
        let release = fetch_release(&feed).unwrap().expect("a release");
        // The exe, then what it cannot do without, then its symbols.
        let wanted: Vec<&str> = release.wanted().map(|f| f.name.as_str()).collect();
        assert_eq!(wanted, ["iai.exe", "iai_raw.dll", "iai.pdb"]);
        let pid = std::process::id();

        // Over an app that has the library, and over one from before it was a
        // file of its own.
        for had_library in [true, false] {
            let folder = TempFolder::new(if had_library { "library" } else { "no_library" });
            let exe = folder.file("iai.exe", b"old exe");
            if had_library {
                folder.file("iai_raw.dll", b"old library");
            }
            let staged = fetch_files(&feed, &release, &exe, &|_| {}).unwrap();
            assert!(staged.library.is_some() && staged.pdb.is_some());
            swap(&staged, pid).unwrap();
            assert_eq!(std::fs::read(&exe).unwrap(), b"new exe");
            assert_eq!(
                std::fs::read(folder.0.join("iai_raw.dll")).unwrap(),
                b"new library"
            );
            let set_aside = folder.0.join(format!("iai_raw.dll.old-{pid}"));
            assert_eq!(set_aside.exists(), had_library);

            tidy(&exe, SystemTime::now());
            assert_eq!(folder.names(), ["iai.exe", "iai.pdb", "iai_raw.dll"]);
        }
    }

    #[test]
    fn a_damaged_raw_library_stops_the_update_and_nothing_is_left_waiting() {
        let (json, _) = published_with_library(b"new exe", b"new library", 2_000);
        let feed = serve(vec![
            ("update.json", json.into_bytes()),
            ("iai.exe.gz", gzipped(b"new exe")),
            ("iai_raw.dll.gz", gzipped(b"new librarY")),
            ("iai.pdb", b"new symbols".to_vec()),
        ]) + "update.json";
        let folder = TempFolder::new("damaged_library");
        let exe = folder.file("iai.exe", b"old exe");
        folder.file("iai_raw.dll", b"old library");
        let release = fetch_release(&feed).unwrap().expect("a release");
        let error = fetch_files(&feed, &release, &exe, &|_| {}).unwrap_err();
        assert!(error.contains("không đúng"), "{error}");
        assert_eq!(folder.names(), ["iai.exe", "iai_raw.dll"]);
        assert_eq!(std::fs::read(&exe).unwrap(), b"old exe");
    }

    #[test]
    fn an_exe_left_without_its_raw_library_fetches_the_one_of_its_own_build() {
        let (json, library) = published_with_library(b"new exe", b"new library", 2_000);
        let feed = serve(vec![
            ("update.json", json.into_bytes()),
            ("iai_raw.dll.gz", library),
        ]) + "update.json";
        let release = fetch_release(&feed).unwrap().expect("a release");
        let folder = TempFolder::new("mend");
        folder.file("iai.exe", b"new exe");
        let target = folder.0.join("iai_raw.dll");
        let build = |commit| Build {
            commit,
            time: 2_000,
        };

        // The release is of another build: its library is not this exe's.
        assert_eq!(
            fetch_library(&feed, &release, build("def456"), &target),
            Ok(false)
        );
        assert_eq!(folder.names(), ["iai.exe"]);

        assert_eq!(
            fetch_library(&feed, &release, build("abc123"), &target),
            Ok(true)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new library");
        assert_eq!(folder.names(), ["iai.exe", "iai_raw.dll"]);

        // A release from before the library was a file of its own has none.
        let (json, _, _) = published(b"new exe", b"new symbols", 2_000);
        let old: Release = serde_json::from_str(&json).unwrap();
        assert_eq!(
            fetch_library(&feed, &old, build("abc123"), &target),
            Ok(false)
        );
    }

    #[test]
    fn a_damaged_download_is_refused_and_the_exe_stays() {
        // The exe published is not the one its SHA-256 names.
        let (json, _, pdb) = published(b"new exe", b"new symbols", 2_000);
        let feed = serve(vec![
            ("update.json", json.clone().into_bytes()),
            ("iai.exe.gz", gzipped(b"new exE")),
            ("iai.pdb", pdb),
        ]) + "update.json";
        let folder = TempFolder::new("damaged");
        let exe = folder.file("iai.exe", b"old exe");
        let release = fetch_release(&feed).unwrap().expect("a release");
        let error = fetch_files(&feed, &release, &exe, &|_| {}).unwrap_err();
        assert!(error.contains("không đúng"), "{error}");
        assert_eq!(folder.names(), ["iai.exe"]);
        assert_eq!(std::fs::read(&exe).unwrap(), b"old exe");

        // Damaged symbols: the exe is installed without them.
        let feed = serve(vec![
            ("update.json", json.into_bytes()),
            ("iai.exe.gz", gzipped(b"new exe")),
            ("iai.pdb", b"new symbolz".to_vec()),
        ]) + "update.json";
        let staged = fetch_files(&feed, &release, &exe, &|_| {}).unwrap();
        assert!(staged.pdb.is_none());
        assert_eq!(std::fs::read(&staged.exe.0).unwrap(), b"new exe");
    }

    #[test]
    fn no_release_no_network_and_an_odd_server_are_told_apart() {
        let empty = serve(vec![
            (
                "update.json",
                br#"{"commit":"c","time":9,"files":[]}"#.to_vec(),
            ),
            ("garbled.json", b"<html>not json".to_vec()),
        ]);
        // Nothing published yet: the build in use is the newest there is.
        assert_eq!(fetch_release(&(empty.clone() + "missing.json")), Ok(None));
        // A release this app cannot read is the server's trouble, not the
        // user's network.
        for odd in ["update.json", "garbled.json"] {
            let error = fetch_release(&(empty.clone() + odd)).unwrap_err();
            assert!(error.contains("Máy chủ cập nhật"), "{odd}: {error}");
        }
        // No server to be reached at all: the network.
        let error = fetch_release("http://127.0.0.1:9/update.json").unwrap_err();
        assert!(error.contains("Không kết nối được mạng"), "{error}");
    }

    #[test]
    fn a_swap_that_cannot_finish_leaves_the_app_able_to_start() {
        let folder = TempFolder::new("swap");
        let exe = folder.file("iai.exe", b"old exe");
        let staged = Staged {
            exe: (folder.0.join("iai.exe.update-7"), exe.clone()),
            library: None,
            pdb: None,
        };
        // The download is gone by the time the app closes.
        assert!(swap(&staged, 7).is_err());
        assert_eq!(folder.names(), ["iai.exe"]);
        assert_eq!(std::fs::read(&exe).unwrap(), b"old exe");

        // The library went in and the exe could not follow: the old library
        // is back beside the old exe.
        let library = folder.file("iai_raw.dll", b"old library");
        let staged = Staged {
            exe: (folder.0.join("iai.exe.update-7"), exe.clone()),
            library: Some((
                folder.file("iai_raw.dll.update-7", b"new library"),
                library.clone(),
            )),
            pdb: None,
        };
        assert!(swap(&staged, 7).is_err());
        assert_eq!(folder.names(), ["iai.exe", "iai_raw.dll"]);
        assert_eq!(std::fs::read(&exe).unwrap(), b"old exe");
        assert_eq!(std::fs::read(&library).unwrap(), b"old library");
    }

    #[test]
    fn a_start_tidies_what_an_update_left() {
        let folder = TempFolder::new("tidy");
        let exe = folder.file("iai.exe", b"exe");
        folder.file("iai.pdb", b"old symbols");
        folder.file("iai.pdb.new", b"new symbols");
        folder.file("iai.exe.old-11", b"made way");
        folder.file("iai_raw.dll", b"library");
        folder.file("iai_raw.dll.old-11", b"made way");
        folder.file("iai_raw.dll.update-12", b"being downloaded");
        folder.file("iai.exe.update-12", b"being downloaded");
        folder.file("iai.pdb.update-12.part", b"being downloaded");
        folder.file("iai.exe.WebView2", b"not the update's");
        folder.file("iai.execute", b"not the update's");

        // Just now: the downloads may be another window's.
        tidy(&exe, SystemTime::now());
        assert_eq!(
            folder.names(),
            [
                "iai.exe",
                "iai.exe.WebView2",
                "iai.exe.update-12",
                "iai.execute",
                "iai.pdb",
                "iai.pdb.update-12.part",
                "iai_raw.dll",
                "iai_raw.dll.update-12",
            ]
        );
        assert_eq!(
            std::fs::read(folder.0.join("iai.pdb")).unwrap(),
            b"new symbols"
        );

        // Two days on nobody is.
        tidy(&exe, SystemTime::now() + 2 * STALE);
        assert_eq!(
            folder.names(),
            [
                "iai.exe",
                "iai.exe.WebView2",
                "iai.execute",
                "iai.pdb",
                "iai_raw.dll"
            ]
        );
    }

    /// Opt-in: IAI_UPDATE_PROBE is a folder holding what
    /// `scripts/package_update.ps1` made (update.json and its assets). It is
    /// served and installed over a stand-in exe, as the app would.
    #[test]
    #[ignore]
    fn probe_packaged_update() {
        let Ok(dir) = std::env::var("IAI_UPDATE_PROBE") else {
            return;
        };
        let files: Vec<(&'static str, Vec<u8>)> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| {
                let name: &'static str = Box::leak(
                    entry
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                        .into_boxed_str(),
                );
                (name, std::fs::read(entry.path()).unwrap())
            })
            .collect();
        let feed = serve(files) + "update.json";
        let folder = TempFolder::new("probe");
        let exe = folder.file("iai.exe", b"old exe");
        let release = fetch_release(&feed).unwrap().expect("a release");
        println!(
            "{} ({}), {} MB; this build {:?}; newer: {}",
            release.commit,
            release.day(),
            release.megabytes(),
            this_build(),
            release.newer_than(this_build())
        );
        let started = std::time::Instant::now();
        let staged = fetch_files(&feed, &release, &exe, &|_| {}).unwrap();
        assert!(staged.pdb.is_some());
        swap(&staged, std::process::id()).unwrap();
        for file in release.wanted() {
            let name = &file.name;
            let size = std::fs::metadata(folder.0.join(name)).unwrap().len();
            println!("{name}: {size} bytes");
        }
        println!("installed in {:.1?}", started.elapsed());
    }
}
