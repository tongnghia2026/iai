//! What a user writes to the app's author: a bug report, a request for a
//! feature of their own, a sponsor's details. It goes to the form his news
//! names (`crate::author_news`; a Google Apps Script that mails it to him,
//! `docs/author/form.gs`). What cannot be sent is kept as a file on the
//! Desktop, for the user to send.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

use base64::Engine;

/// Where a file kept on the Desktop is to be sent by hand.
pub const AUTHOR_EMAIL: &str = "minhnghia718@gmail.com";

/// No more than this is sent of a message's text, of a logo, of one log, and
/// of the logs' index.
pub const MAX_TEXT: usize = 4000;
pub const MAX_LOGO_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 512 * 1024;
const MAX_INDEX_BYTES: u64 = 64 * 1024;
/// A bug report carries the logs of this many sessions, the newest first:
/// the one that went wrong is often the one before this.
const SESSIONS_SENT: usize = 2;
const SESSION_LOGS: [&str; 3] = ["journal.log", "incidents.log", "watcher.log"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A company or an organization that sponsors: its name, a line of
    /// contact and its logo, for the sponsors list.
    Sponsor,
    /// A suggestion, or a bug with the logs the user agreed to send.
    Feedback,
    /// A feature the user needs for their own work.
    Feature,
}

impl Kind {
    /// As the form knows it.
    fn key(self) -> &'static str {
        match self {
            Kind::Sponsor => "sponsor",
            Kind::Feedback => "feedback",
            Kind::Feature => "feature",
        }
    }

    /// As the user and the author read it.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Sponsor => "Tài trợ dưới tên công ty, tổ chức",
            Kind::Feedback => "Góp ý, báo lỗi",
            Kind::Feature => "Cần tính năng riêng",
        }
    }
}

/// What the user wrote, and what they agreed to send with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Outgoing {
    pub kind: Kind,
    /// Who writes; for a sponsor, the name to list.
    pub name: String,
    /// How to reach them; for a sponsor, the line to show in the list.
    pub contact: String,
    pub text: String,
    /// A sponsor's logo, as a file on this machine.
    pub logo: Option<PathBuf>,
    /// The user agreed to send the flight recorder's latest logs.
    pub diagnostics: bool,
}

/// A file that goes with a message.
#[derive(Clone, Debug, PartialEq)]
struct Attachment {
    name: String,
    mime: &'static str,
    bytes: Vec<u8>,
}

/// A message ready to leave: the text and its files.
#[derive(Clone, Debug, PartialEq)]
struct Message {
    kind: Kind,
    name: String,
    contact: String,
    text: String,
    build: String,
    files: Vec<Attachment>,
}

/// Where a message stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Sending,
    Sent,
    /// It could not be sent: it waits in this file on the Desktop.
    Kept(PathBuf),
    /// Neither sent nor kept; why, in the user's words.
    Failed(String),
}

struct Shared {
    status: Status,
    /// `status` has changed since the window last looked.
    unseen: bool,
}

fn shared() -> MutexGuard<'static, Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            Mutex::new(Shared {
                status: Status::Idle,
                unseen: false,
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn status() -> Status {
    shared().status.clone()
}

/// A message is on its way, or its outcome has not been looked at yet: the
/// window has to keep looking in.
pub fn busy() -> bool {
    let shared = shared();
    shared.unseen || shared.status == Status::Sending
}

/// The status, taken as looked at; `true` with it when it is news.
pub fn observe() -> (Status, bool) {
    let mut shared = shared();
    let news = std::mem::take(&mut shared.unseen);
    (shared.status.clone(), news)
}

/// The outcome has been read: the next message starts clean.
pub fn reset() {
    let mut shared = shared();
    if shared.status != Status::Sending {
        shared.status = Status::Idle;
    }
}

/// Send `outgoing` to `form`, off this thread; with no form, or when it does
/// not go through, keep it on the Desktop.
pub fn send(outgoing: Outgoing, form: Option<String>) {
    // A test neither reaches the network nor writes to the Desktop.
    if cfg!(test) {
        return;
    }
    {
        let mut shared = shared();
        if shared.status == Status::Sending {
            return;
        }
        shared.status = Status::Sending;
    }
    std::thread::spawn(move || {
        let status = match message(&outgoing, &crate::diag::root_dir()) {
            Ok(message) => deliver(&message, form.as_deref(), &desktop_dir()),
            Err(why) => Status::Failed(why),
        };
        crate::diag::note("mail", &format!("{}: {status:?}", outgoing.kind.key()));
        let mut shared = shared();
        shared.status = status;
        shared.unseen = true;
    });
}

/// The build this exe is, as the author is told it.
fn build_line() -> String {
    let build = crate::update::this_build();
    format!(
        "{} ({})",
        build.commit.get(..10).unwrap_or(build.commit),
        build.day()
    )
}

/// `outgoing` with its files read: the logo, and the logs under
/// `diagnostics` when the user agreed to them.
fn message(outgoing: &Outgoing, diagnostics: &Path) -> Result<Message, String> {
    let mut files = Vec::new();
    if let Some(logo) = &outgoing.logo {
        files.push(logo_file(logo)?);
    }
    if outgoing.diagnostics
        && let Some(bytes) = pack_sessions(diagnostics)
    {
        files.push(Attachment {
            name: "nhat_ky_loi.zip".to_owned(),
            mime: "application/zip",
            bytes,
        });
    }
    Ok(Message {
        kind: outgoing.kind,
        name: outgoing.name.trim().to_owned(),
        contact: outgoing.contact.trim().to_owned(),
        text: outgoing.text.trim().chars().take(MAX_TEXT).collect(),
        build: build_line(),
        files,
    })
}

/// Whether `path` can be a sponsor's logo: a PNG or JPEG picture, not too
/// large to send. Why not, in the user's words.
pub fn check_logo(path: &Path) -> Result<(), String> {
    logo_file(path).map(|_| ())
}

fn logo_file(path: &Path) -> Result<Attachment, String> {
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
    let (name, mime) = match extension.as_deref() {
        Some("png") => ("logo.png", "image/png"),
        Some("jpg" | "jpeg") => ("logo.jpg", "image/jpeg"),
        _ => return Err("Logo phải là ảnh PNG hoặc JPG.".to_owned()),
    };
    let size = std::fs::metadata(path)
        .map_err(|e| format!("Không đọc được file logo: {e}"))?
        .len();
    if size > MAX_LOGO_BYTES {
        return Err("Logo lớn quá 2 MB. Hãy chọn ảnh nhỏ hơn.".to_owned());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Không đọc được file logo: {e}"))?;
    if image::load_from_memory(&bytes).is_err() {
        return Err("File logo không phải là ảnh đọc được.".to_owned());
    }
    Ok(Attachment {
        name: name.to_owned(),
        mime,
        bytes,
    })
}

/// The last `max` bytes of the file at `path`.
fn tail(path: &Path, max: u64) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The flight recorder's logs under `root` as one zip: the newest sessions'
/// and the index of incidents, each no longer than its end. `None` when
/// there is nothing to send.
fn pack_sessions(root: &Path) -> Option<Vec<u8>> {
    let mut sessions: Vec<String> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    sessions.sort_unstable_by(|a, b| b.cmp(a));
    let mut logs: Vec<(String, Vec<u8>)> = Vec::new();
    for session in sessions.iter().take(SESSIONS_SENT) {
        for log in SESSION_LOGS {
            if let Some(bytes) = tail(&root.join(session).join(log), MAX_LOG_BYTES) {
                logs.push((format!("{session}/{log}"), bytes));
            }
        }
    }
    if let Some(bytes) = tail(&root.join("INDEX.log"), MAX_INDEX_BYTES) {
        logs.push(("INDEX.log".to_owned(), bytes));
    }
    if logs.is_empty() {
        return None;
    }
    zipped(&logs).ok()
}

fn zipped(files: &[(String, Vec<u8>)]) -> std::io::Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in files {
        zip.start_file(name.as_str(), options)?;
        zip.write_all(bytes)?;
    }
    Ok(zip.finish()?.into_inner())
}

/// What the form is sent.
fn form_body(message: &Message) -> serde_json::Value {
    let files: Vec<serde_json::Value> = message
        .files
        .iter()
        .map(|file| {
            serde_json::json!({
                "name": file.name,
                "type": file.mime,
                "data": base64::engine::general_purpose::STANDARD.encode(&file.bytes),
            })
        })
        .collect();
    serde_json::json!({
        "app": "iAi",
        "kind": message.kind.key(),
        "name": message.name,
        "contact": message.contact,
        "text": message.text,
        "build": message.build,
        "files": files,
    })
}

/// What the author reads, as plain text.
fn letter(message: &Message) -> String {
    let or_none = |text: &str| {
        if text.is_empty() {
            "(không ghi)".to_owned()
        } else {
            text.to_owned()
        }
    };
    format!(
        "{}\r\n\r\nTên: {}\r\nLiên hệ: {}\r\nBản app: {}\r\n\r\n{}\r\n",
        message.kind.label(),
        or_none(&message.name),
        or_none(&message.contact),
        message.build,
        message.text.replace('\n', "\r\n"),
    )
}

/// Hand `message` to the form at `form`: it answers `{"ok": true}` once the
/// author has it.
fn post(form: &str, message: &Message) -> Result<(), String> {
    let answer = crate::update::client(Duration::from_secs(90))?
        .post(form)
        .json(&form_body(message))
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text())
        .map_err(|e| e.to_string())?;
    let taken = serde_json::from_str::<serde_json::Value>(&answer)
        .ok()
        .and_then(|answer| answer.get("ok")?.as_bool());
    match taken {
        Some(true) => Ok(()),
        _ => Err(format!(
            "the form answered {}",
            answer.chars().take(200).collect::<String>()
        )),
    }
}

/// `message` as a file in `dir` for the user to send by hand: its text as
/// `noi_dung.txt`, and its files.
fn keep(message: &Message, dir: &Path, now: SystemTime) -> std::io::Result<PathBuf> {
    let stamp: String = crate::diag::local_minute(now)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut files = vec![("noi_dung.txt".to_owned(), letter(message).into_bytes())];
    files.extend(
        message
            .files
            .iter()
            .map(|file| (file.name.clone(), file.bytes.clone())),
    );
    // A second message in the same minute does not take the first one's
    // place.
    let mut path = dir.join(format!("iAi gui tac gia {stamp}.zip"));
    let mut count = 1;
    while path.exists() {
        count += 1;
        path = dir.join(format!("iAi gui tac gia {stamp} ({count}).zip"));
    }
    std::fs::write(&path, zipped(&files)?)?;
    Ok(path)
}

/// Send `message` to `form`; failing that, or with no form, keep it in
/// `desktop`.
fn deliver(message: &Message, form: Option<&str>, desktop: &Path) -> Status {
    match form.map(|form| post(form, message)) {
        Some(Ok(())) => return Status::Sent,
        Some(Err(why)) => crate::diag::note("mail", &format!("not sent: {why}")),
        None => crate::diag::note("mail", "no form to send to"),
    }
    match keep(message, desktop, SystemTime::now()) {
        Ok(path) => Status::Kept(path),
        Err(why) => {
            crate::diag::note("mail", &format!("not kept: {why}"));
            Status::Failed(format!(
                "Không gửi được, và cũng không lưu được ra Desktop. \
                 Hãy viết thẳng tới {AUTHOR_EMAIL}."
            ))
        }
    }
}

/// The user's Desktop: where they will find a file the app leaves them.
fn desktop_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(dir) = crate::windows_integration::desktop_dir().filter(|dir| dir.is_dir()) {
        return dir;
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // OneDrive may have taken the Desktop over.
    [home.join("Desktop"), home.join("OneDrive").join("Desktop")]
        .into_iter()
        .find(|dir| dir.is_dir())
        .unwrap_or(home)
}

/// Show `path` in the file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(path)
        .spawn();
    #[cfg(not(windows))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::tests::TempFolder;
    use std::io::BufRead;
    use std::sync::mpsc;

    /// A form for one test: it answers every message with `answer` and
    /// passes on what it was sent.
    fn form(answer: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/exec", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = std::io::BufReader::new(stream);
                let mut length = 0;
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|read| read > 2) {
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let mut body = vec![0; length];
                if reader.read_exact(&mut body).is_err() {
                    continue;
                }
                let _ = tx.send(String::from_utf8_lossy(&body).into_owned());
                let _ = reader.into_inner().write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (address, rx)
    }

    fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 120, 200, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    /// The names in a zip, and the text of the one called `name`.
    fn unzipped(bytes: &[u8], name: &str) -> (Vec<String>, String) {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names = zip.file_names().map(str::to_owned).collect();
        let mut text = String::new();
        zip.by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        (names, text)
    }

    fn outgoing(kind: Kind) -> Outgoing {
        Outgoing {
            kind,
            name: "  Tiệm ảnh Hoa Mai ".to_owned(),
            contact: "0900 000 000".to_owned(),
            text: "Bấm in thì app đứng.\nMáy in Canon.".to_owned(),
            logo: None,
            diagnostics: false,
        }
    }

    #[test]
    fn a_bug_report_carries_the_newest_logs_only_when_the_user_agreed() {
        let dir = TempFolder::new("mail_logs");
        for (session, journal) in [
            ("2026-10-10_080000_000_pid1", "oldest"),
            ("2026-10-11_080000_000_pid2", "before"),
            ("2026-10-12_080000_000_pid3", "now"),
        ] {
            std::fs::create_dir_all(dir.0.join(session)).unwrap();
            std::fs::write(dir.0.join(session).join("journal.log"), journal).unwrap();
        }
        let long = "x".repeat(MAX_LOG_BYTES as usize) + "the end";
        std::fs::write(
            dir.0
                .join("2026-10-12_080000_000_pid3")
                .join("incidents.log"),
            &long,
        )
        .unwrap();
        dir.file("INDEX.log", b"one line an incident");

        let mut report = outgoing(Kind::Feedback);
        assert!(message(&report, &dir.0).unwrap().files.is_empty());
        report.diagnostics = true;
        let sent = message(&report, &dir.0).unwrap();
        assert_eq!(sent.name, "Tiệm ảnh Hoa Mai");
        assert_eq!(sent.files.len(), 1);
        assert_eq!(sent.files[0].name, "nhat_ky_loi.zip");
        let (mut names, journal) = unzipped(
            &sent.files[0].bytes,
            "2026-10-12_080000_000_pid3/journal.log",
        );
        names.sort();
        assert_eq!(
            names,
            vec![
                "2026-10-11_080000_000_pid2/journal.log",
                "2026-10-12_080000_000_pid3/incidents.log",
                "2026-10-12_080000_000_pid3/journal.log",
                "INDEX.log",
            ]
        );
        assert_eq!(journal, "now");
        // A long log is sent by its end.
        let (_, incidents) = unzipped(
            &sent.files[0].bytes,
            "2026-10-12_080000_000_pid3/incidents.log",
        );
        assert_eq!(incidents.len(), MAX_LOG_BYTES as usize);
        assert!(incidents.ends_with("the end"));
        // No logs at all: the report goes without.
        let empty = TempFolder::new("mail_no_logs");
        assert!(message(&report, &empty.0).unwrap().files.is_empty());
    }

    #[test]
    fn a_sponsors_logo_is_a_picture_of_a_size_that_can_be_sent() {
        let dir = TempFolder::new("mail_logo");
        let logo = dir.file("Logo Công Ty.PNG", &png());
        assert_eq!(check_logo(&logo), Ok(()));
        let mut sponsor = outgoing(Kind::Sponsor);
        sponsor.logo = Some(logo);
        let sent = message(&sponsor, &dir.0).unwrap();
        assert_eq!(
            (sent.files[0].name.as_str(), sent.files[0].mime),
            ("logo.png", "image/png")
        );
        assert_eq!(sent.files[0].bytes, png());

        let refused =
            |name: &str, content: &[u8]| check_logo(&dir.file(name, content)).unwrap_err();
        assert!(refused("logo.pdf", &png()).contains("PNG hoặc JPG"));
        assert!(refused("logo.jpg", b"not a picture").contains("không phải là ảnh"));
        let large = vec![0; MAX_LOGO_BYTES as usize + 1];
        assert!(refused("large.png", &large).contains("2 MB"));
        assert!(check_logo(&dir.0.join("gone.png")).is_err());
        // A logo that cannot go stops the message: it says why.
        sponsor.logo = Some(dir.0.join("gone.png"));
        assert!(message(&sponsor, &dir.0).is_err());
    }

    #[test]
    fn a_message_goes_to_the_form_and_one_that_cannot_is_kept_on_the_desktop() {
        let dir = TempFolder::new("mail_send");
        let mut sponsor = outgoing(Kind::Sponsor);
        sponsor.logo = Some(dir.file("logo.png", &png()));
        let sent = message(&sponsor, &dir.0).unwrap();
        let desktop = dir.0.join("Desktop");
        std::fs::create_dir_all(&desktop).unwrap();
        let on_desktop = || std::fs::read_dir(&desktop).unwrap().count();

        // The form takes it: nothing is left on the Desktop.
        let (address, got) = form(r#"{"ok":true}"#);
        assert_eq!(deliver(&sent, Some(&address), &desktop), Status::Sent);
        let body: serde_json::Value = serde_json::from_str(&got.recv().unwrap()).unwrap();
        assert_eq!(body["app"], "iAi");
        assert_eq!(body["kind"], "sponsor");
        assert_eq!(body["name"], "Tiệm ảnh Hoa Mai");
        assert_eq!(body["contact"], "0900 000 000");
        assert_eq!(body["text"], "Bấm in thì app đứng.\nMáy in Canon.");
        assert!(
            body["build"]
                .as_str()
                .is_some_and(|build| !build.is_empty())
        );
        assert_eq!(body["files"][0]["name"], "logo.png");
        assert_eq!(body["files"][0]["type"], "image/png");
        let data = body["files"][0]["data"].as_str().unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap(),
            png()
        );
        assert_eq!(on_desktop(), 0);

        // The form refuses, is not there, or there is none: the message is
        // kept whole, with how to read it.
        let (refusing, _got) = form(r#"{"ok":false,"why":"busy"}"#);
        let gone = "http://127.0.0.1:9/exec".to_owned();
        for form in [Some(&refusing), Some(&gone), None] {
            let before = on_desktop();
            let Status::Kept(path) = deliver(&sent, form.map(String::as_str), &desktop) else {
                panic!("not kept with {form:?}");
            };
            assert!(path.starts_with(&desktop) && path.extension().is_some_and(|e| e == "zip"));
            let (names, text) = unzipped(&std::fs::read(&path).unwrap(), "noi_dung.txt");
            assert_eq!(names, vec!["noi_dung.txt", "logo.png"]);
            assert!(text.starts_with("Tài trợ dưới tên công ty, tổ chức\r\n"));
            assert!(text.contains("Tên: Tiệm ảnh Hoa Mai\r\n"));
            assert!(text.contains("Liên hệ: 0900 000 000\r\n"));
            assert!(text.contains("Bấm in thì app đứng.\r\nMáy in Canon."));
            std::fs::remove_file(&path).unwrap();
            assert_eq!(on_desktop(), before);
        }

        // Two in the same minute are two files.
        let now = SystemTime::now();
        let first = keep(&sent, &desktop, now).unwrap();
        let second = keep(&sent, &desktop, now).unwrap();
        assert_ne!(first, second);
        assert!(first.is_file() && second.is_file());

        // Nowhere to keep it either: the user is told where to write.
        let nowhere = dir.0.join("no").join("such").join("folder");
        let Status::Failed(why) = deliver(&sent, None, &nowhere) else {
            panic!("kept in a folder that is not there");
        };
        assert!(why.contains(AUTHOR_EMAIL));
    }

    #[test]
    fn a_long_text_is_cut_and_a_name_left_empty_says_so() {
        let mut feature = outgoing(Kind::Feature);
        feature.name = String::new();
        feature.text = "ă".repeat(MAX_TEXT + 50);
        let sent = message(&feature, Path::new("nowhere")).unwrap();
        assert_eq!(sent.text.chars().count(), MAX_TEXT);
        let text = letter(&sent);
        assert!(text.starts_with("Cần tính năng riêng\r\n"));
        assert!(text.contains("Tên: (không ghi)\r\n"));
        assert_eq!(form_body(&sent)["kind"], "feature");
        assert_eq!(form_body(&sent)["files"], serde_json::json!([]));
    }
}
