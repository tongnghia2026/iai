//! Word from the app's author, which changes without a new build of the app:
//! the addresses "Ủng hộ tác giả" opens, and the sponsors it thanks. It is one
//! small file in the project's repository, `docs/author/news.json`, with the
//! sponsors' logos beside it. The app reads it as it starts and keeps the last
//! copy under `%APPDATA%\IAI\author` for a start without a network.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;

/// Where the file is read, the first that answers: the branch the app is
/// developed on, then the repository's own. `IAI_NEWS_FEED` names another
/// place, an address or a file on this machine, to rehearse.
const FEEDS: [&str; 2] = [
    "https://raw.githubusercontent.com/tongnghia2026/iai/feat/vector-core-foundation/docs/author/news.json",
    "https://raw.githubusercontent.com/tongnghia2026/iai/main/docs/author/news.json",
];
const NEWS_NAME: &str = "news.json";
/// What a user writes goes to the author's own form (a Google Apps Script)
/// and nowhere else, whatever the file says.
const FORM_HOST: &str = "https://script.google.com/macros/s/";
/// The news waits for the window, and for the update check before it.
const STARTUP_DELAY: Duration = Duration::from_secs(5);
/// No more than this is taken of the file, of one logo, of each list and of
/// each line.
const MAX_NEWS_BYTES: usize = 256 * 1024;
const MAX_LOGO_BYTES: usize = 1024 * 1024;
const MAX_LISTED: usize = 500;
const MAX_LINE: usize = 120;
/// A price of Pro the file may name, in đồng.
const PRO_PRICES: std::ops::RangeInclusive<u64> = 10_000..=50_000_000;
/// A logo is kept no larger than this a side, in pixels.
const LOGO_SIDE: u32 = 256;

/// The file, as its author writes it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct News {
    /// What "Nghe nhạc" opens, in place of the address built into the app.
    pub music_url: Option<String>,
    /// The author's post to pass on to friends.
    pub share_url: Option<String>,
    /// Where the app sends what a user writes to the author.
    pub form_url: Option<String>,
    /// What Pro costs, in đồng, in place of the price built into the app.
    pub pro_price: Option<u64>,
    pub sponsors: Sponsors,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Sponsors {
    pub organizations: Vec<Organization>,
    /// Names, the newest first.
    pub people: Vec<String>,
    /// How many gave without a name.
    pub anonymous: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Organization {
    pub name: String,
    /// One line: how to reach it.
    pub contact: String,
    /// Its logo's file, beside the news file.
    pub logo: Option<String>,
}

/// An address the app will open or send to.
fn is_https(url: &str) -> bool {
    url.len() > "https://".len()
        && url.starts_with("https://")
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A logo is a bare image file's name: nothing in the file can send the app
/// elsewhere for it, or have it write outside its folder.
fn is_logo_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.starts_with('.')
        && [".png", ".jpg", ".jpeg"]
            .iter()
            .any(|ext| lower.ends_with(ext))
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// One line of the file, no longer than a line.
fn line(text: &str) -> String {
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().take(MAX_LINE).collect()
}

impl News {
    /// The file with what the app does not take dropped: an address that is
    /// not `https://`, a logo that is not a bare image file's name, an entry
    /// without a name, and whatever runs past the limits.
    fn checked(self) -> Self {
        let link = |url: Option<String>| {
            url.map(|url| url.trim().to_owned())
                .filter(|url| is_https(url))
        };
        let organizations = self
            .sponsors
            .organizations
            .into_iter()
            .map(|org| Organization {
                name: line(&org.name),
                contact: line(&org.contact),
                logo: org.logo.filter(|logo| is_logo_name(logo)),
            })
            .filter(|org| !org.name.is_empty())
            .take(MAX_LISTED)
            .collect();
        let people = self
            .sponsors
            .people
            .iter()
            .map(|name| line(name))
            .filter(|name| !name.is_empty())
            .take(MAX_LISTED)
            .collect();
        Self {
            music_url: link(self.music_url),
            share_url: link(self.share_url),
            form_url: link(self.form_url).filter(|url| url.starts_with(FORM_HOST)),
            pro_price: self.pro_price.filter(|price| PRO_PRICES.contains(price)),
            sponsors: Sponsors {
                organizations,
                people,
                anonymous: self.sponsors.anonymous,
            },
        }
    }

    /// The logos the file names, each once.
    fn logo_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .sponsors
            .organizations
            .iter()
            .filter_map(|org| org.logo.as_deref())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

/// The file's text read: `None` for one that is too long or does not parse.
fn parse(bytes: &[u8]) -> Option<News> {
    // An editor may have put a byte-order mark in front.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    if bytes.len() > MAX_NEWS_BYTES {
        return None;
    }
    serde_json::from_slice::<News>(bytes)
        .ok()
        .map(News::checked)
}

/// A logo, decoded: RGBA, row by row.
#[derive(Clone, PartialEq)]
pub struct Logo {
    pub size: [usize; 2],
    pub rgba: Arc<Vec<u8>>,
}

impl std::fmt::Debug for Logo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Logo({}x{})", self.size[0], self.size[1])
    }
}

fn decode_logo(bytes: &[u8]) -> Option<Logo> {
    let image = image::load_from_memory(bytes).ok()?;
    // A small one stays as it was drawn.
    let image = if image.width().max(image.height()) > LOGO_SIDE {
        image.thumbnail(LOGO_SIDE, LOGO_SIDE)
    } else {
        image
    }
    .into_rgba8();
    Some(Logo {
        size: [image.width() as usize, image.height() as usize],
        rgba: Arc::new(image.into_raw()),
    })
}

/// The news the app has, and the logos it names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    /// A copy has been read: this start's, or the one kept from the last.
    pub known: bool,
    pub news: News,
    pub logos: HashMap<String, Logo>,
    /// Goes up whenever the rest changes.
    pub revision: u64,
}

fn shared() -> &'static Mutex<Arc<Loaded>> {
    static SHARED: OnceLock<Mutex<Arc<Loaded>>> = OnceLock::new();
    SHARED.get_or_init(|| Mutex::new(Arc::new(Loaded::default())))
}

/// The news as it stands; empty until a copy has been read.
pub fn current() -> Arc<Loaded> {
    shared()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn publish(news: News, logos: HashMap<String, Logo>) {
    let mut shared = shared()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *shared = Arc::new(Loaded {
        known: true,
        news,
        logos,
        revision: shared.revision + 1,
    });
}

/// The place `IAI_NEWS_FEED` names, when this start is a rehearsal.
fn rehearsed() -> Option<String> {
    std::env::var("IAI_NEWS_FEED")
        .ok()
        .map(|feed| feed.trim().to_owned())
        .filter(|feed| !feed.is_empty())
}

fn feeds() -> Vec<String> {
    match rehearsed() {
        Some(feed) => vec![feed],
        None => FEEDS.iter().map(|feed| feed.to_string()).collect(),
    }
}

/// Where the last copy is kept: `%APPDATA%\IAI\author`. A rehearsal keeps
/// its own beside it: what it showed must not pass for the author's news at
/// the next start.
fn kept_dir() -> Option<PathBuf> {
    let name = if rehearsed().is_some() {
        "author_rehearsal"
    } else {
        "author"
    };
    Some(crate::core::prefs_store::prefs_path().parent()?.join(name))
}

/// As the app starts, off this thread: the copy kept from the last start,
/// then the one published now. `IAI_UPDATE=0` keeps the app off the network.
pub fn load_at_startup() {
    if cfg!(test) {
        return;
    }
    std::thread::spawn(|| {
        let Some(dir) = kept_dir() else { return };
        if let Some((news, logos)) = read_kept(&dir) {
            publish(news, logos);
        }
        if std::env::var_os("IAI_UPDATE").is_some_and(|v| v == "0") {
            return;
        }
        std::thread::sleep(STARTUP_DELAY);
        match fetch(&feeds(), &dir) {
            Ok((news, logos)) => publish(news, logos),
            Err(why) => crate::diag::note("news", &why),
        }
    });
}

/// What lies at `source`, an address or a file on this machine, up to `max`
/// bytes of it.
fn get(source: &str, max: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let limit = max as u64 + 1;
    if source.starts_with("http://") || source.starts_with("https://") {
        crate::update::client(Duration::from_secs(20))?
            .get(source)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|e| format!("{source}: {e}"))?
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{source}: {e}"))?;
    } else {
        std::fs::File::open(source)
            .and_then(|file| file.take(limit).read_to_end(&mut bytes))
            .map_err(|e| format!("{source}: {e}"))?;
    }
    if bytes.len() > max {
        return Err(format!("{source}: more than {max} bytes"));
    }
    Ok(bytes)
}

/// Where `name` lies beside `feed`.
fn beside(feed: &str, name: &str) -> String {
    match feed.rfind(['/', '\\']) {
        Some(cut) => format!("{}{name}", &feed[..=cut]),
        None => name.to_owned(),
    }
}

/// `bytes` written as `path`, whole or not at all.
fn keep(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let coming = path.with_extension("coming");
    std::fs::write(&coming, bytes)?;
    std::fs::rename(&coming, path)
}

/// The news published at the first of `feeds` that answers, with its logos;
/// both are kept in `dir`, and logos no longer named leave it.
fn fetch(feeds: &[String], dir: &Path) -> Result<(News, HashMap<String, Logo>), String> {
    let mut found = Err("no feed".to_owned());
    for feed in feeds {
        found = get(feed, MAX_NEWS_BYTES).map(|bytes| (feed, bytes));
        if found.is_ok() {
            break;
        }
    }
    let (feed, bytes) = found?;
    let news = parse(&bytes).ok_or_else(|| format!("{feed}: not the author's news"))?;
    std::fs::create_dir_all(dir)
        .and_then(|()| keep(&dir.join(NEWS_NAME), &bytes))
        .map_err(|e| format!("keeping the news: {e}"))?;

    let names = news.logo_names();
    let mut logos = HashMap::new();
    for &name in &names {
        let path = dir.join(name);
        // A logo already here is not asked for again: a new picture comes
        // under a new name.
        let bytes = std::fs::read(&path).or_else(|_| {
            let bytes = get(&beside(feed, name), MAX_LOGO_BYTES)?;
            keep(&path, &bytes).map_err(|e| format!("keeping {name}: {e}"))?;
            Ok::<_, String>(bytes)
        });
        match bytes.map(|bytes| decode_logo(&bytes)) {
            Ok(Some(logo)) => {
                logos.insert(name.to_owned(), logo);
            }
            Ok(None) => crate::diag::note("news", &format!("{name} is no picture")),
            Err(why) => crate::diag::note("news", &why),
        }
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != NEWS_NAME && !names.contains(&name.as_str()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok((news, logos))
}

/// The copy kept in `dir` by the last [`fetch`].
fn read_kept(dir: &Path) -> Option<(News, HashMap<String, Logo>)> {
    let news = parse(&std::fs::read(dir.join(NEWS_NAME)).ok()?)?;
    let logos = news
        .logo_names()
        .into_iter()
        .filter_map(|name| {
            let logo = decode_logo(&std::fs::read(dir.join(name)).ok()?)?;
            Some((name.to_owned(), logo))
        })
        .collect();
    Some((news, logos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::tests::{TempFolder, serve};

    /// A picture file of one colour, `width × height`.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    const PUBLISHED: &str = r#"{
        "music_url": "https://www.youtube.com/watch?v=abc",
        "share_url": "https://www.facebook.com/someone/posts/1",
        "sponsors": {
            "organizations": [
                {"name": "Tiệm ảnh Hoa Mai", "contact": "0900 000 000 · Huế", "logo": "hoa_mai.png"},
                {"name": "Công ty In Nhanh"}
            ],
            "people": ["Nguyễn Văn A", "Trần Thị B"],
            "anonymous": 3
        }
    }"#;

    #[test]
    fn what_the_app_does_not_take_is_dropped_from_the_file() {
        let news = parse(
            "\u{feff}{
                \"music_url\": \"http://plain.example/x\",
                \"share_url\": \"  https://www.facebook.com/p/1  \",
                \"form_url\": \"javascript:alert(1)\",
                \"something_new\": [1, 2],
                \"sponsors\": {
                    \"organizations\": [
                        {\"name\": \"  A   B  \", \"logo\": \"../prefs.json\"},
                        {\"name\": \"C\", \"logo\": \"c.exe\"},
                        {\"name\": \"D\", \"logo\": \"news.json\"},
                        {\"name\": \"E\", \"logo\": \"e.JPG\", \"contact\": \"e\\nf\"},
                        {\"name\": \"   \", \"logo\": \"f.png\"}
                    ],
                    \"people\": [\"\", \"  G \", \"H\"]
                }
            }"
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(news.music_url, None);
        assert_eq!(
            news.share_url.as_deref(),
            Some("https://www.facebook.com/p/1")
        );
        assert_eq!(news.form_url, None);
        let orgs: Vec<(&str, Option<&str>)> = news
            .sponsors
            .organizations
            .iter()
            .map(|org| (org.name.as_str(), org.logo.as_deref()))
            .collect();
        assert_eq!(
            orgs,
            vec![
                ("A B", None),
                ("C", None),
                ("D", None),
                ("E", Some("e.JPG"))
            ]
        );
        assert_eq!(news.sponsors.organizations[3].contact, "e f");
        assert_eq!(news.sponsors.people, vec!["G", "H"]);
        assert_eq!(news.logo_names(), vec!["e.JPG"]);

        // A user's words go to the author's form at Google, or nowhere.
        let form = |url: &str| {
            parse(format!(r#"{{"form_url": "{url}"}}"#).as_bytes())
                .unwrap()
                .form_url
        };
        let at_google = "https://script.google.com/macros/s/AKfycb_x-1/exec";
        assert_eq!(form(at_google).as_deref(), Some(at_google));
        assert_eq!(form("https://example.com/macros/s/AKfycb/exec"), None);
        assert_eq!(form("https://script.google.com.example.com/x"), None);

        // A price of Pro is one a person would set; anything else is none,
        // and the app keeps its own.
        let price = |price: &str| {
            parse(format!(r#"{{"pro_price": {price}}}"#).as_bytes()).and_then(|news| news.pro_price)
        };
        assert_eq!(price("250000"), Some(250_000));
        assert_eq!(
            (price("0"), price("9999"), price("50000001")),
            (None, None, None)
        );
        assert_eq!(price("\"free\""), None);

        // A long line is cut; a file of another kind, or too long, is none.
        let long = format!(r#"{{"sponsors": {{"people": ["{}"]}}}}"#, "x".repeat(400));
        let cut = &parse(long.as_bytes()).unwrap().sponsors.people[0];
        assert_eq!(cut.chars().count(), MAX_LINE);
        assert_eq!(parse(b"<html>"), None);
        let huge = format!(
            r#"{{"music_url": "https://{}"}}"#,
            "x".repeat(MAX_NEWS_BYTES)
        );
        assert_eq!(parse(huge.as_bytes()), None);
        // An empty file of the right kind is news with nothing in it.
        assert_eq!(parse(b"{}"), Some(News::default()));
    }

    #[test]
    fn the_file_the_repository_holds_is_one_the_app_takes_whole() {
        let text = include_str!("../docs/author/news.json");
        let raw: News = serde_json::from_str(text).expect("docs/author/news.json parses");
        assert_eq!(parse(text.as_bytes()), Some(raw.clone()), "nothing dropped");
        // Every logo it names lies beside it.
        for name in raw.logo_names() {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("docs/author")
                .join(name);
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(bytes.len() <= MAX_LOGO_BYTES, "{name} is too large");
            assert!(decode_logo(&bytes).is_some(), "{name} is no picture");
        }
    }

    #[test]
    fn news_comes_with_its_logos_and_is_kept_for_a_start_without_a_network() {
        let dir = TempFolder::new("news");
        let kept = dir.0.join("author");
        let address = serve(vec![
            ("docs/news.json", PUBLISHED.as_bytes().to_vec()),
            ("docs/hoa_mai.png", png(600, 300)),
        ]);
        // The first feed has nothing: the next one is asked.
        let feeds = [
            format!("{address}gone/news.json"),
            format!("{address}docs/news.json"),
        ];
        let (news, logos) = fetch(&feeds, &kept).unwrap();
        assert_eq!(
            news.music_url.as_deref(),
            Some("https://www.youtube.com/watch?v=abc")
        );
        assert_eq!(news.sponsors.organizations.len(), 2);
        assert_eq!(news.sponsors.people, vec!["Nguyễn Văn A", "Trần Thị B"]);
        assert_eq!(news.sponsors.anonymous, 3);
        // The logo is kept small.
        assert_eq!(logos.len(), 1);
        assert_eq!(logos["hoa_mai.png"].size, [256, 128]);
        assert_eq!(logos["hoa_mai.png"].rgba.len(), 256 * 128 * 4);

        // Without a network the last copy is what the app has.
        assert!(fetch(&[format!("{address}gone/news.json")], &kept).is_err());
        assert_eq!(read_kept(&kept), Some((news, logos)));

        // A sponsor leaves the file: its logo leaves the folder.
        let later = serve(vec![(
            "news.json",
            br#"{"sponsors": {"people": ["C"]}}"#.to_vec(),
        )]);
        let (news, logos) = fetch(&[format!("{later}news.json")], &kept).unwrap();
        assert_eq!(news.sponsors.people, vec!["C"]);
        assert!(logos.is_empty());
        let mut names: Vec<String> = std::fs::read_dir(&kept)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec![NEWS_NAME]);

        // What is not the author's news leaves the kept copy as it was.
        let odd = serve(vec![("news.json", b"<html>sign in</html>".to_vec())]);
        assert!(fetch(&[format!("{odd}news.json")], &kept).is_err());
        assert_eq!(read_kept(&kept).unwrap().0.sponsors.people, vec!["C"]);
    }

    #[test]
    fn a_rehearsal_reads_the_news_from_a_folder_on_this_machine() {
        let dir = TempFolder::new("news_local");
        let feed = dir.file("news.json", PUBLISHED.as_bytes());
        dir.file("hoa_mai.png", &png(64, 64));
        let kept = dir.0.join("kept");
        let (news, logos) = fetch(&[feed.to_string_lossy().into_owned()], &kept).unwrap();
        assert_eq!(news.sponsors.organizations[0].name, "Tiệm ảnh Hoa Mai");
        assert_eq!(logos["hoa_mai.png"].size, [64, 64]);
        // A logo that is missing or no picture costs only itself.
        let dir = TempFolder::new("news_bad_logo");
        let feed = dir.file("news.json", PUBLISHED.as_bytes());
        let (news, logos) =
            fetch(&[feed.to_string_lossy().into_owned()], &dir.0.join("kept")).unwrap();
        assert_eq!(news.sponsors.organizations.len(), 2);
        assert!(logos.is_empty());
    }
}
