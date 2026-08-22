//! Bounded discovery and manifest I/O for external camera profiles.
//!
//! This is the only camera-profile component that reads the filesystem or the
//! process environment. It parses a strict, versioned JSON manifest, performs
//! exact camera-identity matching using the resolver's identity contract, and
//! loads each selected profile's bytes exactly once so the pure resolver hashes
//! and parses the same buffer.
//!
//! There is deliberately no filesystem scan, fuzzy matching, automatic download,
//! or copied vendor asset here. Relative profile paths are validated to normal
//! components only, and the canonical loaded file must stay under the canonical
//! profile root. The manifest schema is version 1 with `deny_unknown_fields`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use super::icc_camera::MAX_SCENE_CAMERA_ICC_BYTES;
use super::resolver::{
    camera_binding_matches, CameraIdentityRef, ManifestDcpCandidate, ProfileBlob, Sha256Digest,
    TrustedSceneIccCandidate, MAX_IDENTITY_COMPONENT_BYTES, MAX_MANIFEST_ENTRY_ID_BYTES,
    MAX_PROVENANCE_TEXT_BYTES,
};

/// The only manifest schema version this build understands.
pub const MANIFEST_SCHEMA_VERSION: u16 = 1;
/// Maximum manifest file size accepted before JSON parsing.
pub const MAX_MANIFEST_FILE_BYTES: usize = 1 << 20;
/// Maximum number of profile records in one manifest.
pub const MAX_MANIFEST_PROFILES: usize = 2048;
/// Maximum camera identities bound to one profile record.
pub const MAX_CAMERAS_PER_ENTRY: usize = 16;
/// Maximum relative-path length accepted in a manifest record.
pub const MAX_PROFILE_PATH_BYTES: usize = 1024;
/// Maximum standalone DCP payload discovery will read from disk. Mirrors the
/// resolver's standalone DCP limit; keep the two values equal.
pub const MAX_DCP_PROFILE_BYTES: usize = 64 * 1024 * 1024;

/// Environment variable naming an explicit DCP that overrides all discovery.
pub const EXPLICIT_PROFILE_ENV: &str = "IAI_CAMERA_PROFILE";
/// Environment variable naming the profile manifest file. Its parent directory
/// is the profile root that all relative record paths resolve against.
pub const MANIFEST_PATH_ENV: &str = "IAI_CAMERA_PROFILE_MANIFEST";

/// Kind of external profile bound by a manifest record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    Dcp,
    SceneIcc,
}

/// Declared input domain of a scene camera ICC. Only bounded `[0,1]` camera
/// input is representable today; the enum keeps the manifest self-documenting
/// for the still-gated ICC path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IccInputDomain {
    /// Finite camera samples already normalized into `[0,1]`.
    BoundedUnit,
}

/// A validated camera identity bound to a manifest record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestCameraId {
    make: String,
    model: String,
}

impl ManifestCameraId {
    pub fn make(&self) -> &str {
        &self.make
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn as_ref(&self) -> CameraIdentityRef<'_> {
        CameraIdentityRef {
            make: &self.make,
            model: &self.model,
        }
    }
}

/// Kind-specific, validated portion of a manifest record.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ProfileSpec {
    Dcp {
        unique_camera_model: String,
    },
    SceneIcc {
        allow_missing_ciis: bool,
        input_domain: IccInputDomain,
    },
}

/// One validated manifest record with owned, bounded fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    id: String,
    kind: ProfileKind,
    relative_path: PathBuf,
    sha256: Sha256Digest,
    cameras: Vec<ManifestCameraId>,
    spec: ProfileSpec,
}

impl ManifestEntry {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn kind(&self) -> ProfileKind {
        self.kind
    }

    pub fn cameras(&self) -> &[ManifestCameraId] {
        &self.cameras
    }
}

/// A parsed, validated profile manifest. Holds no file handles; matching and
/// loading are separate, bounded steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileManifest {
    entries: Vec<ManifestEntry>,
}

/// A manifest record whose camera list contains an exact match for a query
/// camera. Borrows the owning manifest; load it to obtain profile bytes.
#[derive(Clone, Copy, Debug)]
pub struct MatchedProfile<'m> {
    entry: &'m ManifestEntry,
    matched_camera: usize,
}

impl<'m> MatchedProfile<'m> {
    pub fn entry(&self) -> &'m ManifestEntry {
        self.entry
    }

    pub fn kind(&self) -> ProfileKind {
        self.entry.kind
    }

    pub fn id(&self) -> &'m str {
        &self.entry.id
    }

    fn matched(&self) -> &'m ManifestCameraId {
        &self.entry.cameras[self.matched_camera]
    }
}

/// A manifest profile read once from disk. Owns its bytes so resolver candidate
/// borrows all reference the same buffer that will be hashed and parsed.
#[derive(Clone, Debug)]
pub struct LoadedManifestProfile {
    id: String,
    kind: ProfileKind,
    matched_make: String,
    matched_model: String,
    sha256: Sha256Digest,
    spec: ProfileSpec,
    locator: String,
    bytes: Vec<u8>,
}

impl LoadedManifestProfile {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn kind(&self) -> ProfileKind {
        self.kind
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn sha256(&self) -> Sha256Digest {
        self.sha256
    }

    fn camera(&self) -> CameraIdentityRef<'_> {
        CameraIdentityRef {
            make: &self.matched_make,
            model: &self.matched_model,
        }
    }

    fn blob(&self) -> ProfileBlob<'_> {
        ProfileBlob {
            bytes: &self.bytes,
            locator: &self.locator,
        }
    }

    /// Resolver candidate for a DCP record, or `None` for a scene ICC record.
    pub fn as_dcp_candidate(&self) -> Option<ManifestDcpCandidate<'_>> {
        match &self.spec {
            ProfileSpec::Dcp {
                unique_camera_model,
            } => Some(ManifestDcpCandidate {
                entry_id: &self.id,
                blob: self.blob(),
                camera: self.camera(),
                expected_sha256: self.sha256,
                expected_unique_camera_model: unique_camera_model,
            }),
            ProfileSpec::SceneIcc { .. } => None,
        }
    }

    /// Resolver candidate for a scene ICC record, or `None` for a DCP record.
    ///
    /// The RAW pipeline does not yet apply scene ICCs; this exists for resolver
    /// tests and the future gated ICC path.
    pub fn as_scene_icc_candidate(&self) -> Option<TrustedSceneIccCandidate<'_>> {
        match &self.spec {
            ProfileSpec::SceneIcc {
                allow_missing_ciis, ..
            } => Some(TrustedSceneIccCandidate {
                entry_id: &self.id,
                blob: self.blob(),
                camera: self.camera(),
                expected_sha256: self.sha256,
                allow_missing_ciis: *allow_missing_ciis,
            }),
            ProfileSpec::Dcp { .. } => None,
        }
    }
}

/// An explicit override DCP named by the environment. Owns its bytes.
#[derive(Clone, Debug)]
pub struct ExplicitProfile {
    locator: String,
    bytes: Vec<u8>,
}

impl ExplicitProfile {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn blob(&self) -> ProfileBlob<'_> {
        ProfileBlob {
            bytes: &self.bytes,
            locator: &self.locator,
        }
    }
}

/// A structural or semantic manifest failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    TooLarge {
        actual: usize,
        maximum: usize,
    },
    Json {
        message: String,
    },
    UnsupportedSchemaVersion {
        actual: u16,
    },
    TooManyProfiles {
        actual: usize,
        maximum: usize,
    },
    EmptyId {
        index: usize,
    },
    IdTooLong {
        index: usize,
        actual: usize,
        maximum: usize,
    },
    DuplicateId {
        id: String,
    },
    NoCameras {
        id: String,
    },
    TooManyCameras {
        id: String,
        actual: usize,
        maximum: usize,
    },
    EmptyCameraComponent {
        id: String,
    },
    CameraComponentTooLong {
        id: String,
        actual: usize,
        maximum: usize,
    },
    DuplicateCameraAlias {
        id: String,
    },
    InvalidPath {
        id: String,
        reason: PathRejection,
    },
    InvalidSha256 {
        id: String,
    },
    MissingDcpModel {
        id: String,
    },
    DcpModelTooLong {
        id: String,
        actual: usize,
        maximum: usize,
    },
    UnexpectedField {
        id: String,
        field: &'static str,
        kind: ProfileKind,
    },
    MissingIccField {
        id: String,
        field: &'static str,
    },
}

/// Why a manifest relative path was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathRejection {
    Empty,
    TooLong,
    Absolute,
    NonNormalComponent,
    BackslashSeparator,
}

/// A filesystem failure while loading a selected profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileLoadError {
    Io {
        locator: String,
        kind: std::io::ErrorKind,
    },
    RootCanonicalize {
        root: String,
        kind: std::io::ErrorKind,
    },
    OutsideRoot {
        locator: String,
    },
    TooLarge {
        locator: String,
        actual: u64,
        maximum: usize,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, maximum } => {
                write!(f, "manifest is {actual} bytes; maximum is {maximum}")
            }
            Self::Json { message } => write!(f, "manifest JSON is invalid: {message}"),
            Self::UnsupportedSchemaVersion { actual } => {
                write!(f, "unsupported manifest schema_version {actual}")
            }
            Self::TooManyProfiles { actual, maximum } => {
                write!(f, "manifest lists {actual} profiles; maximum is {maximum}")
            }
            Self::EmptyId { index } => write!(f, "profile {index} has an empty id"),
            Self::IdTooLong {
                index,
                actual,
                maximum,
            } => write!(
                f,
                "profile {index} id is {actual} bytes; maximum is {maximum}"
            ),
            Self::DuplicateId { id } => write!(f, "duplicate profile id {id:?}"),
            Self::NoCameras { id } => write!(f, "profile {id:?} lists no cameras"),
            Self::TooManyCameras {
                id,
                actual,
                maximum,
            } => write!(
                f,
                "profile {id:?} lists {actual} cameras; maximum is {maximum}"
            ),
            Self::EmptyCameraComponent { id } => {
                write!(f, "profile {id:?} has an empty camera make or model")
            }
            Self::CameraComponentTooLong {
                id,
                actual,
                maximum,
            } => write!(
                f,
                "profile {id:?} camera identity is {actual} bytes; maximum is {maximum}"
            ),
            Self::DuplicateCameraAlias { id } => {
                write!(f, "profile {id:?} lists the same camera twice")
            }
            Self::InvalidPath { id, reason } => {
                write!(f, "profile {id:?} has an invalid path: {reason:?}")
            }
            Self::InvalidSha256 { id } => {
                write!(f, "profile {id:?} sha256 is not 64 lowercase hex digits")
            }
            Self::MissingDcpModel { id } => {
                write!(f, "dcp profile {id:?} is missing unique_camera_model")
            }
            Self::DcpModelTooLong {
                id,
                actual,
                maximum,
            } => write!(
                f,
                "dcp profile {id:?} unique_camera_model is {actual} bytes; maximum is {maximum}"
            ),
            Self::UnexpectedField { id, field, kind } => {
                write!(f, "profile {id:?} of kind {kind:?} must not set {field:?}")
            }
            Self::MissingIccField { id, field } => {
                write!(f, "scene_icc profile {id:?} is missing {field:?}")
            }
        }
    }
}

impl fmt::Display for ProfileLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { locator, kind } => write!(f, "cannot read {locator}: {kind:?}"),
            Self::RootCanonicalize { root, kind } => {
                write!(f, "cannot canonicalize profile root {root}: {kind:?}")
            }
            Self::OutsideRoot { locator } => {
                write!(f, "profile {locator} resolves outside the profile root")
            }
            Self::TooLarge {
                locator,
                actual,
                maximum,
            } => write!(
                f,
                "profile {locator} is {actual} bytes; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}
impl std::error::Error for ProfileLoadError {}

// Raw serde shapes. `deny_unknown_fields` rejects any key not modelled here; the
// kind-specific optional fields are validated against `kind` after parsing.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema_version: u16,
    #[serde(default)]
    profiles: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    id: String,
    kind: ProfileKind,
    path: String,
    sha256: String,
    cameras: Vec<RawCamera>,
    #[serde(default)]
    unique_camera_model: Option<String>,
    #[serde(default)]
    allow_missing_ciis: Option<bool>,
    #[serde(default)]
    input_domain: Option<IccInputDomain>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCamera {
    make: String,
    model: String,
}

impl ProfileManifest {
    /// Parse and fully validate a manifest byte buffer.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        if bytes.len() > MAX_MANIFEST_FILE_BYTES {
            return Err(ManifestError::TooLarge {
                actual: bytes.len(),
                maximum: MAX_MANIFEST_FILE_BYTES,
            });
        }
        let raw: RawManifest =
            serde_json::from_slice(bytes).map_err(|error| ManifestError::Json {
                message: bounded(&error.to_string(), MAX_PROVENANCE_TEXT_BYTES),
            })?;
        if raw.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchemaVersion {
                actual: raw.schema_version,
            });
        }
        if raw.profiles.len() > MAX_MANIFEST_PROFILES {
            return Err(ManifestError::TooManyProfiles {
                actual: raw.profiles.len(),
                maximum: MAX_MANIFEST_PROFILES,
            });
        }

        let mut entries = Vec::with_capacity(raw.profiles.len());
        let mut seen_ids: Vec<String> = Vec::with_capacity(raw.profiles.len());
        for (index, raw_entry) in raw.profiles.into_iter().enumerate() {
            let entry = validate_entry(index, raw_entry)?;
            if seen_ids.iter().any(|existing| existing == &entry.id) {
                return Err(ManifestError::DuplicateId {
                    id: entry.id.clone(),
                });
            }
            seen_ids.push(entry.id.clone());
            entries.push(entry);
        }

        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    /// Records whose camera list contains an exact, normalized match for
    /// `camera`. The resolver re-checks this binding before selecting.
    pub fn matching_profiles(&self, camera: CameraIdentityRef<'_>) -> Vec<MatchedProfile<'_>> {
        let mut matched = Vec::new();
        for entry in &self.entries {
            if let Some(position) = entry
                .cameras
                .iter()
                .position(|bound| camera_binding_matches(camera, bound.as_ref()))
            {
                matched.push(MatchedProfile {
                    entry,
                    matched_camera: position,
                });
            }
        }
        matched
    }
}

fn validate_entry(index: usize, raw: RawEntry) -> Result<ManifestEntry, ManifestError> {
    let id = raw.id;
    if id.is_empty() {
        return Err(ManifestError::EmptyId { index });
    }
    if id.len() > MAX_MANIFEST_ENTRY_ID_BYTES {
        return Err(ManifestError::IdTooLong {
            index,
            actual: id.len(),
            maximum: MAX_MANIFEST_ENTRY_ID_BYTES,
        });
    }

    if raw.cameras.is_empty() {
        return Err(ManifestError::NoCameras { id });
    }
    if raw.cameras.len() > MAX_CAMERAS_PER_ENTRY {
        return Err(ManifestError::TooManyCameras {
            actual: raw.cameras.len(),
            maximum: MAX_CAMERAS_PER_ENTRY,
            id,
        });
    }

    let mut cameras: Vec<ManifestCameraId> = Vec::with_capacity(raw.cameras.len());
    for raw_camera in raw.cameras {
        let make = raw_camera.make;
        let model = raw_camera.model;
        if make.trim().is_empty() || model.trim().is_empty() {
            return Err(ManifestError::EmptyCameraComponent { id });
        }
        for component in [&make, &model] {
            if component.len() > MAX_IDENTITY_COMPONENT_BYTES {
                return Err(ManifestError::CameraComponentTooLong {
                    actual: component.len(),
                    maximum: MAX_IDENTITY_COMPONENT_BYTES,
                    id,
                });
            }
        }
        let candidate = ManifestCameraId { make, model };
        if cameras
            .iter()
            .any(|existing| camera_binding_matches(existing.as_ref(), candidate.as_ref()))
        {
            return Err(ManifestError::DuplicateCameraAlias { id });
        }
        cameras.push(candidate);
    }

    let relative_path = validate_relative_path(&id, &raw.path)?;
    let sha256 = parse_sha256_lowercase_hex(&raw.sha256)
        .ok_or_else(|| ManifestError::InvalidSha256 { id: id.clone() })?;

    let spec = match raw.kind {
        ProfileKind::Dcp => {
            if raw.allow_missing_ciis.is_some() {
                return Err(ManifestError::UnexpectedField {
                    field: "allow_missing_ciis",
                    kind: ProfileKind::Dcp,
                    id,
                });
            }
            if raw.input_domain.is_some() {
                return Err(ManifestError::UnexpectedField {
                    field: "input_domain",
                    kind: ProfileKind::Dcp,
                    id,
                });
            }
            let unique_camera_model = raw
                .unique_camera_model
                .ok_or_else(|| ManifestError::MissingDcpModel { id: id.clone() })?;
            if unique_camera_model.trim().is_empty() {
                return Err(ManifestError::MissingDcpModel { id });
            }
            if unique_camera_model.len() > MAX_IDENTITY_COMPONENT_BYTES {
                return Err(ManifestError::DcpModelTooLong {
                    actual: unique_camera_model.len(),
                    maximum: MAX_IDENTITY_COMPONENT_BYTES,
                    id,
                });
            }
            ProfileSpec::Dcp {
                unique_camera_model,
            }
        }
        ProfileKind::SceneIcc => {
            if raw.unique_camera_model.is_some() {
                return Err(ManifestError::UnexpectedField {
                    field: "unique_camera_model",
                    kind: ProfileKind::SceneIcc,
                    id,
                });
            }
            let allow_missing_ciis =
                raw.allow_missing_ciis
                    .ok_or(ManifestError::MissingIccField {
                        field: "allow_missing_ciis",
                        id: id.clone(),
                    })?;
            let input_domain = raw.input_domain.ok_or(ManifestError::MissingIccField {
                field: "input_domain",
                id: id.clone(),
            })?;
            ProfileSpec::SceneIcc {
                allow_missing_ciis,
                input_domain,
            }
        }
    };

    Ok(ManifestEntry {
        id,
        kind: raw.kind,
        relative_path,
        sha256,
        cameras,
        spec,
    })
}

fn validate_relative_path(id: &str, raw: &str) -> Result<PathBuf, ManifestError> {
    let reject = |reason| ManifestError::InvalidPath {
        id: id.to_owned(),
        reason,
    };
    if raw.is_empty() {
        return Err(reject(PathRejection::Empty));
    }
    if raw.len() > MAX_PROFILE_PATH_BYTES {
        return Err(reject(PathRejection::TooLong));
    }
    // Force portable forward-slash separators so a Windows-style path cannot be
    // smuggled through as a single "normal" component on a Unix host.
    if raw.contains('\\') {
        return Err(reject(PathRejection::BackslashSeparator));
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(reject(PathRejection::Absolute));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(reject(PathRejection::NonNormalComponent));
        }
    }
    Ok(path.to_path_buf())
}

fn parse_sha256_lowercase_hex(value: &str) -> Option<Sha256Digest> {
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    let bytes = value.as_bytes();
    for (index, slot) in digest.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2])?;
        let low = hex_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Some(Sha256Digest::new(digest))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Read and parse a manifest file with the same bounds as [`ProfileManifest::parse`].
pub fn load_manifest_file(path: &Path) -> Result<ProfileManifest, ProfileLoadError> {
    let locator = bounded_path(path);
    let bytes = read_bounded(path, &locator, MAX_MANIFEST_FILE_BYTES)?;
    ProfileManifest::parse(&bytes).map_err(|error| ProfileLoadError::Io {
        // Surface manifest validation failures through the same load boundary,
        // keeping the manifest message in the locator for provenance.
        locator: bounded(&format!("{locator}: {error}"), MAX_PROVENANCE_TEXT_BYTES),
        kind: std::io::ErrorKind::InvalidData,
    })
}

/// Read one matched profile's bytes exactly once, enforcing the profile-root
/// containment and per-kind size caps.
pub fn load_matched_profile(
    root: &Path,
    matched: &MatchedProfile<'_>,
) -> Result<LoadedManifestProfile, ProfileLoadError> {
    let entry = matched.entry;
    let canonical_root =
        root.canonicalize()
            .map_err(|error| ProfileLoadError::RootCanonicalize {
                root: bounded_path(root),
                kind: error.kind(),
            })?;
    let joined = canonical_root.join(&entry.relative_path);
    let locator_hint = bounded_path(&joined);
    let canonical_file = joined
        .canonicalize()
        .map_err(|error| ProfileLoadError::Io {
            locator: locator_hint.clone(),
            kind: error.kind(),
        })?;
    if !canonical_file.starts_with(&canonical_root) {
        return Err(ProfileLoadError::OutsideRoot {
            locator: bounded_path(&canonical_file),
        });
    }

    let maximum = match entry.kind {
        ProfileKind::Dcp => MAX_DCP_PROFILE_BYTES,
        ProfileKind::SceneIcc => MAX_SCENE_CAMERA_ICC_BYTES,
    };
    let locator = bounded_path(&canonical_file);
    let bytes = read_bounded(&canonical_file, &locator, maximum)?;

    let matched_camera = matched.matched();
    Ok(LoadedManifestProfile {
        id: entry.id.clone(),
        kind: entry.kind,
        matched_make: matched_camera.make.clone(),
        matched_model: matched_camera.model.clone(),
        sha256: entry.sha256,
        spec: entry.spec.clone(),
        locator,
        bytes,
    })
}

/// The explicit-override profile path from [`EXPLICIT_PROFILE_ENV`], if set and
/// non-empty. Env access lives here so the pure resolver/manifest logic stays
/// deterministic and testable without process state.
pub fn explicit_profile_override_path() -> Option<PathBuf> {
    let value = std::env::var_os(EXPLICIT_PROFILE_ENV)?;
    non_empty_path(value)
}

/// The profile manifest path from [`MANIFEST_PATH_ENV`], if set and non-empty.
/// Env access lives here so the pure manifest/resolver logic stays testable
/// without process state.
pub fn manifest_override_path() -> Option<PathBuf> {
    let value = std::env::var_os(MANIFEST_PATH_ENV)?;
    non_empty_path(value)
}

fn non_empty_path(value: OsString) -> Option<PathBuf> {
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

/// Path to a per-camera DCP in the default profile directory beside the running
/// executable: `camera_profiles/<make>__<model>.dcp`, with the identity lowered
/// and path-sanitised. Returned only when the file exists.
///
/// This is the zero-configuration profile source: a build (or a user, or a
/// future clean-room profile pack) drops DCPs into that folder under a neutral
/// naming convention and they apply automatically — no environment variable and
/// no vendor name baked into the code. The loaded DCP still flows through the
/// explicit tier's required camera-match check, so a mis-named file is rejected
/// rather than applied to the wrong camera.
pub fn default_camera_dcp_path(make: &str, model: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = format!(
        "{}__{}.dcp",
        sanitize_identity(make),
        sanitize_identity(model)
    );
    let path = exe.parent()?.join("camera_profiles").join(name);
    path.is_file().then_some(path)
}

/// Lowercase and replace filesystem-reserved characters so a camera identity
/// maps to one stable, portable filename component.
fn sanitize_identity(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

/// Read an explicit-override DCP once, bounded to the standalone DCP cap.
pub fn load_explicit_dcp(path: &Path) -> Result<ExplicitProfile, ProfileLoadError> {
    let locator = bounded_path(path);
    let bytes = read_bounded(path, &locator, MAX_DCP_PROFILE_BYTES)?;
    Ok(ExplicitProfile { locator, bytes })
}

fn read_bounded(path: &Path, locator: &str, maximum: usize) -> Result<Vec<u8>, ProfileLoadError> {
    let metadata = std::fs::metadata(path).map_err(|error| ProfileLoadError::Io {
        locator: locator.to_owned(),
        kind: error.kind(),
    })?;
    if metadata.len() > maximum as u64 {
        return Err(ProfileLoadError::TooLarge {
            locator: locator.to_owned(),
            actual: metadata.len(),
            maximum,
        });
    }
    let bytes = std::fs::read(path).map_err(|error| ProfileLoadError::Io {
        locator: locator.to_owned(),
        kind: error.kind(),
    })?;
    if bytes.len() > maximum {
        return Err(ProfileLoadError::TooLarge {
            locator: locator.to_owned(),
            actual: bytes.len() as u64,
            maximum,
        });
    }
    Ok(bytes)
}

fn bounded_path(path: &Path) -> String {
    bounded(&path.display().to_string(), MAX_PROVENANCE_TEXT_BYTES)
}

fn bounded(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const CANON: CameraIdentityRef<'static> = CameraIdentityRef {
        make: "Canon",
        model: "EOS R5",
    };

    fn sha_hex(byte: u8) -> String {
        let mut out = String::with_capacity(64);
        for _ in 0..32 {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    fn dcp_entry_json(id: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "kind": "dcp",
                "path": "dcp/canon_r5.dcp",
                "sha256": "{}",
                "cameras": [{{ "make": "Canon", "model": "EOS R5" }}],
                "unique_camera_model": "Canon EOS R5"
            }}"#,
            sha_hex(0xab)
        )
    }

    fn manifest_json(entries: &str) -> String {
        format!(r#"{{ "schema_version": 1, "profiles": [{entries}] }}"#)
    }

    #[test]
    fn parses_and_matches_dcp_and_scene_icc_records() {
        let icc = format!(
            r#"{{
                "id": "canon-r5-scene-icc",
                "kind": "scene_icc",
                "path": "icc/canon_r5.icc",
                "sha256": "{}",
                "cameras": [{{ "make": "Canon", "model": "EOS R5" }}],
                "allow_missing_ciis": false,
                "input_domain": "bounded_unit"
            }}"#,
            sha_hex(0xcd)
        );
        let json = manifest_json(&format!("{},{}", dcp_entry_json("canon-r5-dcp"), icc));
        let manifest = ProfileManifest::parse(json.as_bytes()).expect("valid manifest");
        assert_eq!(manifest.entries().len(), 2);

        let matched = manifest.matching_profiles(CANON);
        assert_eq!(matched.len(), 2);
        assert!(matched.iter().any(|m| m.kind() == ProfileKind::Dcp));
        assert!(matched.iter().any(|m| m.kind() == ProfileKind::SceneIcc));

        // Normalized identity match: extra whitespace and case still match.
        let noisy = CameraIdentityRef {
            make: "  canon ",
            model: "eos   r5",
        };
        assert_eq!(manifest.matching_profiles(noisy).len(), 2);

        // Different camera: no matches, no fuzzy prefixing.
        let other = CameraIdentityRef {
            make: "Canon",
            model: "EOS R5C",
        };
        assert!(manifest.matching_profiles(other).is_empty());
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let json = r#"{ "schema_version": 2, "profiles": [] }"#;
        assert_eq!(
            ProfileManifest::parse(json.as_bytes()),
            Err(ManifestError::UnsupportedSchemaVersion { actual: 2 })
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let json = r#"{ "schema_version": 1, "profiles": [], "extra": true }"#;
        assert!(matches!(
            ProfileManifest::parse(json.as_bytes()),
            Err(ManifestError::Json { .. })
        ));

        let entry = format!(
            r#"{{
                "id": "x",
                "kind": "dcp",
                "path": "a.dcp",
                "sha256": "{}",
                "cameras": [{{ "make": "Canon", "model": "EOS R5" }}],
                "unique_camera_model": "Canon EOS R5",
                "surprise": 1
            }}"#,
            sha_hex(0x11)
        );
        assert!(matches!(
            ProfileManifest::parse(manifest_json(&entry).as_bytes()),
            Err(ManifestError::Json { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let json = manifest_json(&format!(
            "{},{}",
            dcp_entry_json("same"),
            dcp_entry_json("same")
        ));
        assert_eq!(
            ProfileManifest::parse(json.as_bytes()),
            Err(ManifestError::DuplicateId {
                id: "same".to_owned()
            })
        );
    }

    #[test]
    fn rejects_too_many_cameras_and_duplicate_alias() {
        let mut cameras = String::new();
        for index in 0..(MAX_CAMERAS_PER_ENTRY + 1) {
            if index > 0 {
                cameras.push(',');
            }
            cameras.push_str(&format!(r#"{{ "make": "Canon", "model": "EOS {index}" }}"#));
        }
        let entry = format!(
            r#"{{
                "id": "many",
                "kind": "dcp",
                "path": "a.dcp",
                "sha256": "{}",
                "cameras": [{cameras}],
                "unique_camera_model": "Canon EOS R5"
            }}"#,
            sha_hex(0x22)
        );
        assert!(matches!(
            ProfileManifest::parse(manifest_json(&entry).as_bytes()),
            Err(ManifestError::TooManyCameras { .. })
        ));

        let dup = format!(
            r#"{{
                "id": "dup",
                "kind": "dcp",
                "path": "a.dcp",
                "sha256": "{}",
                "cameras": [
                    {{ "make": "Canon", "model": "EOS R5" }},
                    {{ "make": " canon ", "model": "eos  r5" }}
                ],
                "unique_camera_model": "Canon EOS R5"
            }}"#,
            sha_hex(0x22)
        );
        assert_eq!(
            ProfileManifest::parse(manifest_json(&dup).as_bytes()),
            Err(ManifestError::DuplicateCameraAlias {
                id: "dup".to_owned()
            })
        );
    }

    #[test]
    fn rejects_unsafe_paths() {
        // Exercise the validator directly with literal strings so JSON string
        // escaping cannot swallow a backslash or slash before it is checked.
        // A leading-slash path is absolute on Unix but only root-relative on
        // Windows; both platforms must still reject it, so allow either reason.
        let cases: [(&str, &[PathRejection]); 6] = [
            ("../secret.dcp", &[PathRejection::NonNormalComponent]),
            ("dcp/../../x.dcp", &[PathRejection::NonNormalComponent]),
            ("./local.dcp", &[PathRejection::NonNormalComponent]),
            (
                "/etc/passwd",
                &[PathRejection::Absolute, PathRejection::NonNormalComponent],
            ),
            ("dir\\file.dcp", &[PathRejection::BackslashSeparator]),
            ("", &[PathRejection::Empty]),
        ];
        for (path, allowed) in cases {
            match validate_relative_path("p", path) {
                Err(ManifestError::InvalidPath { id, reason }) => {
                    assert_eq!(id, "p");
                    assert!(
                        allowed.contains(&reason),
                        "path {path:?} rejected as {reason:?}, expected one of {allowed:?}"
                    );
                }
                other => panic!("path {path:?} should be an InvalidPath, got {other:?}"),
            }
        }

        // A plain relative path with normal components is accepted.
        assert_eq!(
            validate_relative_path("p", "dcp/canon_r5.dcp"),
            Ok(PathBuf::from("dcp/canon_r5.dcp"))
        );
    }

    #[test]
    fn rejects_bad_sha256() {
        for bad in [
            "abc".to_owned(),
            "A".repeat(64),                 // uppercase not allowed
            format!("{}g", "a".repeat(63)), // non-hex char
        ] {
            let entry = format!(
                r#"{{
                    "id": "s",
                    "kind": "dcp",
                    "path": "a.dcp",
                    "sha256": "{bad}",
                    "cameras": [{{ "make": "Canon", "model": "EOS R5" }}],
                    "unique_camera_model": "Canon EOS R5"
                }}"#
            );
            assert_eq!(
                ProfileManifest::parse(manifest_json(&entry).as_bytes()),
                Err(ManifestError::InvalidSha256 { id: "s".to_owned() }),
                "sha {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_kind_field_mismatches() {
        let dcp_with_icc_field = format!(
            r#"{{
                "id": "m",
                "kind": "dcp",
                "path": "a.dcp",
                "sha256": "{}",
                "cameras": [{{ "make": "Canon", "model": "EOS R5" }}],
                "unique_camera_model": "Canon EOS R5",
                "allow_missing_ciis": true
            }}"#,
            sha_hex(0x44)
        );
        assert_eq!(
            ProfileManifest::parse(manifest_json(&dcp_with_icc_field).as_bytes()),
            Err(ManifestError::UnexpectedField {
                id: "m".to_owned(),
                field: "allow_missing_ciis",
                kind: ProfileKind::Dcp,
            })
        );

        let dcp_without_model = format!(
            r#"{{
                "id": "m",
                "kind": "dcp",
                "path": "a.dcp",
                "sha256": "{}",
                "cameras": [{{ "make": "Canon", "model": "EOS R5" }}]
            }}"#,
            sha_hex(0x44)
        );
        assert_eq!(
            ProfileManifest::parse(manifest_json(&dcp_without_model).as_bytes()),
            Err(ManifestError::MissingDcpModel { id: "m".to_owned() })
        );

        let icc_with_model = format!(
            r#"{{
                "id": "m",
                "kind": "scene_icc",
                "path": "a.icc",
                "sha256": "{}",
                "cameras": [{{ "make": "Canon", "model": "EOS R5" }}],
                "allow_missing_ciis": false,
                "input_domain": "bounded_unit",
                "unique_camera_model": "Canon EOS R5"
            }}"#,
            sha_hex(0x44)
        );
        assert_eq!(
            ProfileManifest::parse(manifest_json(&icc_with_model).as_bytes()),
            Err(ManifestError::UnexpectedField {
                id: "m".to_owned(),
                field: "unique_camera_model",
                kind: ProfileKind::SceneIcc,
            })
        );

        let icc_without_trust = format!(
            r#"{{
                "id": "m",
                "kind": "scene_icc",
                "path": "a.icc",
                "sha256": "{}",
                "cameras": [{{ "make": "Canon", "model": "EOS R5" }}],
                "input_domain": "bounded_unit"
            }}"#,
            sha_hex(0x44)
        );
        assert_eq!(
            ProfileManifest::parse(manifest_json(&icc_without_trust).as_bytes()),
            Err(ManifestError::MissingIccField {
                id: "m".to_owned(),
                field: "allow_missing_ciis",
            })
        );
    }

    #[test]
    fn rejects_oversized_manifest() {
        let oversized = vec![b' '; MAX_MANIFEST_FILE_BYTES + 1];
        assert_eq!(
            ProfileManifest::parse(&oversized),
            Err(ManifestError::TooLarge {
                actual: MAX_MANIFEST_FILE_BYTES + 1,
                maximum: MAX_MANIFEST_FILE_BYTES,
            })
        );
    }

    #[test]
    fn sanitizes_camera_identity_into_a_stable_filename_component() {
        assert_eq!(sanitize_identity("Canon"), "canon");
        assert_eq!(sanitize_identity("  EOS 6D "), "eos 6d");
        assert_eq!(sanitize_identity("SONY"), "sony");
        assert_eq!(sanitize_identity("A/B:C*?"), "a_b_c__");
    }

    #[test]
    fn parses_sha256_only_lowercase_hex() {
        let digest = parse_sha256_lowercase_hex(&sha_hex(0x0f)).expect("valid hex");
        assert_eq!(digest.as_bytes(), &[0x0f; 32]);
        assert!(parse_sha256_lowercase_hex(&"F".repeat(64)).is_none());
        assert!(parse_sha256_lowercase_hex("deadbeef").is_none());
    }

    fn unique_temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "iai-discovery-{tag}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn loads_matched_profile_bytes_once_and_builds_candidate() {
        let root = unique_temp_dir("load");
        std::fs::create_dir_all(root.join("dcp")).expect("nested dir");
        let payload = b"deterministic dcp bytes";
        std::fs::write(root.join("dcp").join("canon_r5.dcp"), payload).expect("write profile");

        let manifest =
            ProfileManifest::parse(manifest_json(&dcp_entry_json("canon-r5-dcp")).as_bytes())
                .expect("valid manifest");
        let matched = manifest.matching_profiles(CANON);
        assert_eq!(matched.len(), 1);

        let loaded = load_matched_profile(&root, &matched[0]).expect("load profile");
        assert_eq!(loaded.bytes(), payload);
        assert_eq!(loaded.kind(), ProfileKind::Dcp);

        let candidate = loaded.as_dcp_candidate().expect("dcp candidate");
        assert_eq!(candidate.entry_id, "canon-r5-dcp");
        assert_eq!(candidate.expected_unique_camera_model, "Canon EOS R5");
        assert_eq!(candidate.blob.bytes, payload);
        assert_eq!(candidate.camera.make, "Canon");
        assert!(loaded.as_scene_icc_candidate().is_none());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_profile_file_is_an_io_error() {
        let root = unique_temp_dir("missing");
        let manifest =
            ProfileManifest::parse(manifest_json(&dcp_entry_json("canon-r5-dcp")).as_bytes())
                .expect("valid manifest");
        let matched = manifest.matching_profiles(CANON);
        let error = load_matched_profile(&root, &matched[0]).expect_err("missing file");
        assert!(matches!(error, ProfileLoadError::Io { .. }));
        std::fs::remove_dir_all(&root).ok();
    }
}
