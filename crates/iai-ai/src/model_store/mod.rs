//! Every model of the crate in one place, ComfyUI-like: unpack a model pack
//! or drop a model into a model folder and it runs.
//!
//! - `catalog`: the one table of known models;
//! - `roots`: the model folders, and where a model fetched or installed goes;
//! - `onnx`, `recognize`, `scan`: what each file in them is, by its hash,
//!   else by the shape of its inputs and outputs, else why not;
//! - `list`: the author's list of where to fetch each model
//!   (`docs/author/models.json`), read from the public repository, with the
//!   copy built into the app when it cannot be read;
//! - `fetch`: fetching a model from the first of its sources that gives the
//!   right file, resumed where it stopped;
//! - `install`: installing a model from an `.onnx` or `.zip` file;
//! - `status`: what the "Model AI" window shows.
//!
//! [`locate`] is cheap enough to be asked every frame; what it does not find
//! where its slot keeps it, it takes from the last scan, run in the
//! background (once at the first ask, again on [`rescan`] and when a model
//! is missing).

pub mod catalog;
pub mod fetch;
pub mod install;
pub mod list;
pub mod onnx;
pub mod recognize;
pub mod roots;
pub mod scan;
pub mod status;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub use catalog::{Family, Feature, KnownModel, Pack, Slot};
use recognize::{HashCache, Reason, Recognition};
pub use roots::Places;
use scan::Inventory;

/// How long an answer of [`locate`] stands without asking the disk again.
const LOCATE_KEPT: Duration = Duration::from_secs(5);
/// A model asked for and not found starts a new scan at most this often.
const RESCAN_AFTER_MISS: Duration = Duration::from_secs(60);
/// A scan this recent is not run again for a model about to be run.
const FRESH_SCAN: Duration = Duration::from_secs(10);

#[derive(Default)]
struct State {
    inventory: Option<Arc<Inventory>>,
    scanned_at: Option<Instant>,
    scanning: bool,
    /// Counts the scans done, so a window knows when to look again.
    generation: u64,
}

fn state() -> &'static (Mutex<State>, Condvar) {
    static STATE: OnceLock<(Mutex<State>, Condvar)> = OnceLock::new();
    STATE.get_or_init(|| (Mutex::new(State::default()), Condvar::new()))
}

fn lock_state() -> std::sync::MutexGuard<'static, State> {
    state().0.lock().unwrap_or_else(|e| e.into_inner())
}

/// `%APPDATA%\IAI\model-checks.json`: the hashes already read.
fn cache_file() -> PathBuf {
    iai_platform::app_data::dir().join("model-checks.json")
}

fn hash_cache() -> &'static Mutex<HashCache> {
    static CACHE: OnceLock<Mutex<HashCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        // A test writes nothing into the machine's own folders.
        Mutex::new(if cfg!(test) {
            HashCache::in_memory()
        } else {
            HashCache::load(cache_file())
        })
    })
}

/// Work on a copy of the hash cache, so a long hash does not hold anyone
/// else up, and put what was learnt back.
fn with_cache<T>(work: impl FnOnce(&mut HashCache) -> T) -> T {
    let mut copy = hash_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let out = work(&mut copy);
    let mut shared = hash_cache().lock().unwrap_or_else(|e| e.into_inner());
    shared.merge(copy);
    shared.save();
    out
}

/// The model folders of this machine, most authoritative first.
pub fn roots() -> Vec<PathBuf> {
    Places::of_this_machine().roots()
}

/// The folder a model fetched or installed goes to (made if missing).
pub fn main_folder() -> PathBuf {
    Places::of_this_machine().main_folder()
}

/// Where `slot`'s model goes when it is fetched or installed.
pub fn install_path(slot: Slot) -> PathBuf {
    let model = slot.model();
    main_folder().join(model.dir).join(model.file)
}

/// The files known by hash: the table's and those the author's list adds.
pub fn known() -> recognize::Known {
    list::current().known()
}

fn kept_locations() -> &'static Mutex<HashMap<Slot, (Instant, Option<PathBuf>)>> {
    static KEPT: OnceLock<Mutex<HashMap<Slot, (Instant, Option<PathBuf>)>>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ask the disk again at the next [`locate`]: a model came or went.
pub fn forget_found() {
    kept_locations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// The file `slot` runs on this machine, if there is one: its model where
/// its slot keeps it in the first model folder that has it, else what the
/// last scan found for it (checked first, then the user's own of its
/// family). Cheap: asked every frame, the disk is asked every few seconds.
/// Whether the file is the right one is said by [`check`].
pub fn locate(slot: Slot) -> Option<PathBuf> {
    let now = Instant::now();
    if let Some((at, found)) = kept_locations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&slot)
        && now.duration_since(*at) < LOCATE_KEPT
    {
        return found.clone();
    }
    let inventory = inventory();
    let found = locate_in(&roots(), inventory.as_deref(), slot);
    if found.is_none() {
        request_scan(Some(RESCAN_AFTER_MISS));
    }
    kept_locations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(slot, (now, found.clone()));
    found
}

/// Like [`locate`], for the moment a model is about to be run (never on the
/// UI thread): when it is not where its slot keeps it, a scan is waited for,
/// so a model dropped anywhere in a model folder is found the first time.
pub fn locate_or_wait(slot: Slot) -> Option<PathBuf> {
    if let Some(found) = locate(slot) {
        return Some(found);
    }
    // A scan of a moment ago will do (the upscaler asks for three models in
    // a row).
    let fresh = {
        let state = lock_state();
        state
            .inventory
            .clone()
            .filter(|_| state.scanned_at.is_some_and(|at| at.elapsed() < FRESH_SCAN))
            .filter(|_| !state.scanning)
    };
    let inventory = match fresh {
        Some(inventory) => inventory,
        None if cfg!(test) => return None,
        None => scan_now(),
    };
    let found = locate_in(&roots(), Some(&inventory), slot);
    kept_locations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(slot, (Instant::now(), found.clone()));
    found
}

/// [`locate`] over the given folders and scan.
pub fn locate_in(roots: &[PathBuf], inventory: Option<&Inventory>, slot: Slot) -> Option<PathBuf> {
    let model = slot.model();
    roots
        .iter()
        .map(|root| root.join(model.dir).join(model.file))
        .find(|path| {
            path.is_file() && {
                let dir = path.parent().unwrap_or(Path::new("."));
                model.companions.iter().all(|c| dir.join(c.file).is_file())
            }
        })
        .or_else(|| inventory?.best_for(slot).map(|f| f.path.clone()))
}

/// How a file stands for a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// One of the slot's known models, checked by its hash.
    Verified(recognize::Accepted),
    /// The user's own model of the slot's family.
    Custom(Family),
    /// Not one the slot can run, and why.
    Unusable(String),
}

impl Check {
    pub fn usable(&self) -> bool {
        !matches!(self, Check::Unusable(_))
    }
}

/// What the file at `path` is to `slot`. Hashes the file the first time it is
/// asked about it (remembered afterwards), so never on the UI thread.
pub fn check(path: &Path, slot: Slot) -> Check {
    let known = known();
    let recognition = with_cache(|cache| recognize::recognize(path, &known, cache));
    check_of(recognition, slot)
}

fn check_of(recognition: Recognition, slot: Slot) -> Check {
    match recognition {
        Recognition::Verified(accepted) if accepted.slot == slot => Check::Verified(accepted),
        Recognition::Verified(accepted) => Check::Unusable(format!(
            "đây là model {}, của tính năng «{}»",
            accepted.name,
            accepted.slot.model().feature.label()
        )),
        Recognition::Custom(family) if recognize::fits(family, slot) => Check::Custom(family),
        Recognition::Custom(family) => Check::Unusable(format!(
            "đây là model họ {}, không dùng cho {}",
            family.label(),
            slot.model().feature.label()
        )),
        Recognition::Unusable(reason) => Check::Unusable(reason.text()),
        Recognition::Companion | Recognition::NotAModel => {
            Check::Unusable(Reason::UnknownShape("không phải file .onnx".to_string()).text())
        }
    }
}

/// The last scan, if one has been done.
pub fn inventory() -> Option<Arc<Inventory>> {
    lock_state().inventory.clone()
}

/// A scan is running.
pub fn is_scanning() -> bool {
    lock_state().scanning
}

/// How many scans have finished: a window shows them again when this moves.
pub fn generation() -> u64 {
    lock_state().generation
}

/// Look through the model folders again, in the background (the "Model AI"
/// window asks on opening and when the app comes back to the front).
pub fn rescan() {
    request_scan(None);
}

/// Start a scan in the background unless one is running, or one finished
/// less than `unless_within` ago.
fn request_scan(unless_within: Option<Duration>) {
    // A test reads no model folder of the machine it runs on.
    if cfg!(test) {
        return;
    }
    {
        let mut state = lock_state();
        let recent = unless_within
            .zip(state.scanned_at)
            .is_some_and(|(within, at)| at.elapsed() < within);
        if state.scanning || recent {
            return;
        }
        state.scanning = true;
    }
    let started = std::thread::Builder::new()
        .name("model-scan".into())
        .spawn(|| {
            run_scan();
        });
    if started.is_err() {
        lock_state().scanning = false;
    }
}

/// Look through the model folders now and wait for it (or for the scan
/// already running).
pub fn scan_now() -> Arc<Inventory> {
    {
        let (lock, done) = state();
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.scanning {
            let generation = guard.generation;
            while guard.scanning && guard.generation == generation {
                guard = done.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
            if let Some(inventory) = guard.inventory.clone() {
                return inventory;
            }
        }
        guard.scanning = true;
    }
    run_scan()
}

/// The scan itself; the caller has set `scanning`.
fn run_scan() -> Arc<Inventory> {
    let roots = roots();
    let known = known();
    let inventory = Arc::new(with_cache(|cache| scan::scan(&roots, &known, cache)));
    {
        let mut state = lock_state();
        state.inventory = Some(Arc::clone(&inventory));
        state.scanned_at = Some(Instant::now());
        state.scanning = false;
        state.generation += 1;
    }
    state().1.notify_all();
    forget_found();
    inventory
}

#[cfg(test)]
mod tests {
    use super::recognize::tests::{fake, temp};
    use super::*;

    #[test]
    fn a_model_where_its_slot_keeps_it_comes_first_then_what_the_scan_found() {
        let base = temp("locate");
        let (first, second) = (base.join("exe"), base.join("appdata"));
        std::fs::create_dir_all(second.join("face-mesh")).unwrap();
        let in_place = second
            .join("face-mesh")
            .join("face_landmarks_detector.onnx");
        std::fs::write(&in_place, b"model").unwrap();
        let roots = [first.clone(), second.clone()];
        assert_eq!(
            locate_in(&roots, None, Slot::FaceMesh),
            Some(in_place.clone())
        );
        // The same file in the first folder wins.
        std::fs::create_dir_all(first.join("face-mesh")).unwrap();
        let beside_exe = first.join("face-mesh").join("face_landmarks_detector.onnx");
        std::fs::write(&beside_exe, b"model").unwrap();
        assert_eq!(locate_in(&roots, None, Slot::FaceMesh), Some(beside_exe));

        // Sapiens2 without its weights beside it is not in place.
        std::fs::create_dir_all(first.join("sapiens2-seg")).unwrap();
        std::fs::write(
            first
                .join("sapiens2-seg")
                .join("sapiens2_seg_0.4b_512x384.onnx"),
            b"m",
        )
        .unwrap();
        assert_eq!(locate_in(&roots, None, Slot::BodyParts), None);
        std::fs::write(
            first
                .join("sapiens2-seg")
                .join("sapiens2_seg_0.4b_512x384.onnx.data"),
            b"w",
        )
        .unwrap();
        assert!(locate_in(&roots, None, Slot::BodyParts).is_some());

        // A renamed upscaler of the user's, anywhere: from the scan.
        std::fs::create_dir_all(second.join("mine")).unwrap();
        std::fs::write(
            second.join("mine").join("x4.onnx"),
            fake(Family::RealEsrgan),
        )
        .unwrap();
        let inventory = scan::scan(
            &roots,
            &recognize::Known::table(),
            &mut HashCache::in_memory(),
        );
        assert_eq!(locate_in(&roots, None, Slot::RealesrganGeneral), None);
        assert_eq!(
            locate_in(&roots, Some(&inventory), Slot::RealesrganGeneral),
            Some(second.join("mine").join("x4.onnx"))
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_file_is_said_to_fit_a_slot_or_why_not() {
        let face = recognize::Accepted::of(Slot::FaceMesh.model());
        assert!(matches!(
            check_of(Recognition::Verified(face.clone()), Slot::FaceMesh),
            Check::Verified(_)
        ));
        match check_of(Recognition::Verified(face), Slot::Gfpgan) {
            Check::Unusable(why) => assert!(why.contains("Điểm mốc khuôn mặt"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            check_of(
                Recognition::Custom(Family::RealEsrgan),
                Slot::RealesrganRrdb
            ),
            Check::Custom(Family::RealEsrgan)
        );
        assert!(!check_of(Recognition::Custom(Family::RealEsrgan), Slot::Gfpgan).usable());
    }
}
