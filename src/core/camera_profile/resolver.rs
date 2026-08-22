//! Deterministic, in-memory camera-profile resolution.
//!
//! Discovery and filesystem access deliberately live outside this module. A
//! caller supplies bounded candidate slices and immutable byte buffers; hash,
//! identity, parse, and transform validation all operate on those same bytes.
//! Candidate failures are recorded and resolution proceeds to the next tier,
//! ending at the bit-compatible decoder matrix.

use hmac_sha256::Hash as Sha256;
use unicode_normalization::UnicodeNormalization;

use super::dcp::{self, DcpError, DcpProfile};
use super::dcp_transform::{self, DcpCameraTransform, DcpTransformError};
use super::icc_camera::{
    SceneCameraIcc, SceneCameraIccError, SceneCameraIccMetadata, MAX_SCENE_CAMERA_ICC_BYTES,
};
use super::{resolve_decoder_matrix, RawDecoderBackend, ResolvedCameraProfile};

/// Maximum number of candidates accepted in any non-explicit tier.
pub const MAX_CANDIDATES_PER_TIER: usize = 8;
/// Maximum total external manifest records inspected by one resolution.
pub const MAX_MANIFEST_CANDIDATES: usize = 2048;
/// Hard cap for the transient resolver audit trail.
pub const MAX_RECORDED_ATTEMPTS: usize = 32;
/// Maximum camera identity component retained or compared by the resolver.
pub const MAX_IDENTITY_COMPONENT_BYTES: usize = 256;
/// Maximum manifest entry identifier retained in provenance.
pub const MAX_MANIFEST_ENTRY_ID_BYTES: usize = 256;
/// Maximum locator/profile text retained in provenance.
pub const MAX_PROVENANCE_TEXT_BYTES: usize = 512;
/// Version of the precedence, identity, and provenance contract below.
pub const RESOLVER_VERSION: u16 = 1;

// Mirrors the standalone parser's public-format boundary. Keep the resolver
// check before SHA-256 so an oversized untrusted buffer cannot consume
// unbounded hashing time before `dcp::parse` reports the same limit.
const MAX_STANDALONE_DCP_BYTES: usize = 64 * 1024 * 1024;

/// Fixed-size SHA-256 content identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }
}

/// Hash the exact candidate buffer used for parsing and transform creation.
pub fn sha256(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest(Sha256::hash(bytes))
}

/// Borrowed camera identity supplied by the RAW decoder.
#[derive(Clone, Copy, Debug)]
pub struct CameraIdentityRef<'a> {
    pub make: &'a str,
    pub model: &'a str,
}

/// Bounded display form retained in resolver provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolverCameraIdentity {
    pub make: String,
    pub model: String,
}

/// Immutable candidate content and a diagnostic-only locator.
#[derive(Clone, Copy, Debug)]
pub struct ProfileBlob<'a> {
    pub bytes: &'a [u8],
    pub locator: &'a str,
}

/// A DCP extracted from the current DNG. Index zero is the primary profile;
/// extra profiles use their stable extraction index.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddedDcpCandidate<'a> {
    pub blob: ProfileBlob<'a>,
    pub profile_index: u16,
}

/// Exact manifest binding for an external DCP.
#[derive(Clone, Copy, Debug)]
pub struct ManifestDcpCandidate<'a> {
    pub entry_id: &'a str,
    pub blob: ProfileBlob<'a>,
    pub camera: CameraIdentityRef<'a>,
    pub expected_sha256: Sha256Digest,
    pub expected_unique_camera_model: &'a str,
}

/// Exact manifest binding for a scene-referred camera ICC.
#[derive(Clone, Copy, Debug)]
pub struct TrustedSceneIccCandidate<'a> {
    pub entry_id: &'a str,
    pub blob: ProfileBlob<'a>,
    pub camera: CameraIdentityRef<'a>,
    pub expected_sha256: Sha256Digest,
    /// Permit only an absent `ciis` tag. Malformed or explicitly non-scene
    /// image state remains an ICC validation error.
    pub allow_missing_ciis: bool,
}

/// Compatibility matrix that guarantees resolution has a usable final tier.
#[derive(Clone, Copy, Debug)]
pub struct DecoderFallback<'a> {
    pub backend: RawDecoderBackend,
    pub camera_to_xyz: &'a [[f32; 4]; 3],
}

/// Complete pure resolver input.
pub struct ResolveRequest<'a> {
    pub camera: CameraIdentityRef<'a>,
    pub wb_gains: [f64; 3],
    pub explicit_dcp: Option<ProfileBlob<'a>>,
    pub embedded_dcps: &'a [EmbeddedDcpCandidate<'a>],
    pub manifest_dcps: &'a [ManifestDcpCandidate<'a>],
    pub trusted_scene_iccs: &'a [TrustedSceneIccCandidate<'a>],
    pub decoder_fallback: DecoderFallback<'a>,
}

/// Resolution tier. Declaration order is not used for selection; the resolver
/// visits each tier explicitly so refactors cannot silently reorder policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateTier {
    ExplicitDcp,
    EmbeddedDcp,
    ManifestDcp,
    TrustedSceneIcc,
    DecoderFallback,
}

/// Stable discriminator for a candidate or a tier-wide policy decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileCandidateId {
    Explicit,
    Embedded { profile_index: u16 },
    Manifest { entry_id: String },
    TierAggregate,
    DecoderFallback,
}

/// Why one bounded candidate attempt was rejected.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileRejection {
    CandidateLimit {
        actual: usize,
        maximum: usize,
    },
    AmbiguousTier {
        exact_matches: usize,
    },
    DuplicateEmbeddedIndex {
        profile_index: u16,
    },
    CameraBindingMismatch,
    MissingProfileCameraModel,
    ProfileCameraModelMismatch {
        declared: String,
    },
    HashMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    DcpParse(DcpError),
    DcpTransform(DcpTransformError),
    SceneIcc(SceneCameraIccError),
}

/// Outcome stored for each attempted tier/candidate.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileAttemptOutcome {
    Selected,
    Rejected(ProfileRejection),
}

/// One bounded, deterministic resolver audit record.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileAttempt {
    pub tier: CandidateTier,
    pub candidate_id: ProfileCandidateId,
    pub locator: String,
    pub outcome: ProfileAttemptOutcome,
}

/// Typed reason the resolver reached the decoder compatibility matrix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecoderFallbackReason {
    NoProfileCandidates,
    AllProfileCandidatesRejected {
        highest_attempted_tier: CandidateTier,
        rejected_attempts: usize,
    },
}

/// Stable details about the selected characterization.
#[derive(Clone, Debug, PartialEq)]
pub enum SelectedProfileProvenance {
    Dcp {
        tier: CandidateTier,
        candidate_id: ProfileCandidateId,
        locator: String,
        sha256: Sha256Digest,
        unique_camera_model: Option<String>,
        profile_name: Option<String>,
        illuminants: Vec<u16>,
        selected_cct_kelvin: f64,
        second_calibration_weight: f64,
    },
    SceneIcc {
        tier: CandidateTier,
        candidate_id: ProfileCandidateId,
        locator: String,
        sha256: Sha256Digest,
        metadata: SceneCameraIccMetadata,
    },
    DecoderMatrix {
        backend: RawDecoderBackend,
        reason: DecoderFallbackReason,
    },
}

/// Transient, bounded resolution provenance independent of the existing scene
/// provenance type. Integration can map this into a future persisted schema
/// without changing the current RAW path in this slice.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolverProvenance {
    pub resolver_version: u16,
    pub camera: ResolverCameraIdentity,
    pub selected: SelectedProfileProvenance,
    pub attempts: Vec<ProfileAttempt>,
}

/// Operational characterization selected by the resolver.
#[derive(Debug)]
pub enum ResolvedCameraCharacterization {
    Dcp {
        profile: DcpProfile,
        transform: DcpCameraTransform,
    },
    SceneIcc(SceneCameraIcc),
    DecoderMatrix(ResolvedCameraProfile),
}

/// Always-usable result: malformed/untrusted profiles resolve to the supplied
/// decoder compatibility matrix and remain visible in provenance.
#[derive(Debug)]
pub struct CameraProfileResolution {
    pub characterization: ResolvedCameraCharacterization,
    pub provenance: ResolverProvenance,
}

impl CameraProfileResolution {
    pub fn is_decoder_fallback(&self) -> bool {
        matches!(
            self.characterization,
            ResolvedCameraCharacterization::DecoderMatrix(_)
        )
    }
}

enum DcpModelPolicy<'a> {
    RequiredCameraMatch,
    EmbeddedParentMatch,
    ManifestExpected(&'a str),
}

struct DcpSuccess {
    profile: DcpProfile,
    transform: DcpCameraTransform,
    digest: Sha256Digest,
}

/// Resolve in the locked order explicit DCP, embedded DCP, exact manifest DCP,
/// exact trusted scene ICC, then decoder fallback.
pub fn resolve(request: ResolveRequest<'_>) -> CameraProfileResolution {
    let camera = bounded_camera_identity(request.camera);
    let mut attempts = Vec::with_capacity(MAX_RECORDED_ATTEMPTS.min(16));
    let mut highest_attempted_tier = None;
    let mut rejected_attempts = 0usize;

    if let Some(blob) = request.explicit_dcp {
        highest_attempted_tier = Some(CandidateTier::ExplicitDcp);
        match evaluate_dcp(
            blob,
            None,
            request.camera,
            request.wb_gains,
            DcpModelPolicy::RequiredCameraMatch,
        ) {
            Ok(success) => {
                push_attempt(
                    &mut attempts,
                    CandidateTier::ExplicitDcp,
                    ProfileCandidateId::Explicit,
                    blob.locator,
                    ProfileAttemptOutcome::Selected,
                );
                return dcp_resolution(
                    camera,
                    attempts,
                    CandidateTier::ExplicitDcp,
                    ProfileCandidateId::Explicit,
                    blob.locator,
                    success,
                );
            }
            Err(rejection) => {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::ExplicitDcp,
                    ProfileCandidateId::Explicit,
                    blob.locator,
                    rejection,
                );
            }
        }
    }

    if !request.embedded_dcps.is_empty() {
        highest_attempted_tier.get_or_insert(CandidateTier::EmbeddedDcp);
        if request.embedded_dcps.len() > MAX_CANDIDATES_PER_TIER {
            rejected_attempts += 1;
            push_rejection(
                &mut attempts,
                CandidateTier::EmbeddedDcp,
                ProfileCandidateId::TierAggregate,
                "<embedded DCP tier>",
                ProfileRejection::CandidateLimit {
                    actual: request.embedded_dcps.len(),
                    maximum: MAX_CANDIDATES_PER_TIER,
                },
            );
        } else {
            let mut candidates: Vec<&EmbeddedDcpCandidate<'_>> =
                request.embedded_dcps.iter().collect();
            candidates.sort_by(|left, right| {
                left.profile_index
                    .cmp(&right.profile_index)
                    .then_with(|| left.blob.locator.cmp(right.blob.locator))
            });
            if let Some(duplicate) = candidates
                .windows(2)
                .find(|pair| pair[0].profile_index == pair[1].profile_index)
            {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::EmbeddedDcp,
                    ProfileCandidateId::TierAggregate,
                    "<embedded DCP tier>",
                    ProfileRejection::DuplicateEmbeddedIndex {
                        profile_index: duplicate[0].profile_index,
                    },
                );
            } else {
                for candidate in candidates {
                    match evaluate_dcp(
                        candidate.blob,
                        None,
                        request.camera,
                        request.wb_gains,
                        DcpModelPolicy::EmbeddedParentMatch,
                    ) {
                        Ok(success) => {
                            push_attempt(
                                &mut attempts,
                                CandidateTier::EmbeddedDcp,
                                ProfileCandidateId::Embedded {
                                    profile_index: candidate.profile_index,
                                },
                                candidate.blob.locator,
                                ProfileAttemptOutcome::Selected,
                            );
                            return dcp_resolution(
                                camera,
                                attempts,
                                CandidateTier::EmbeddedDcp,
                                ProfileCandidateId::Embedded {
                                    profile_index: candidate.profile_index,
                                },
                                candidate.blob.locator,
                                success,
                            );
                        }
                        Err(rejection) => {
                            rejected_attempts += 1;
                            push_rejection(
                                &mut attempts,
                                CandidateTier::EmbeddedDcp,
                                ProfileCandidateId::Embedded {
                                    profile_index: candidate.profile_index,
                                },
                                candidate.blob.locator,
                                rejection,
                            );
                        }
                    }
                }
            }
        }
    }

    let manifest_count = request
        .manifest_dcps
        .len()
        .saturating_add(request.trusted_scene_iccs.len());
    if manifest_count > MAX_MANIFEST_CANDIDATES {
        let (tier, locator) = if request.manifest_dcps.is_empty() {
            (CandidateTier::TrustedSceneIcc, "<scene ICC manifest>")
        } else {
            (CandidateTier::ManifestDcp, "<camera profile manifest>")
        };
        highest_attempted_tier.get_or_insert(tier);
        rejected_attempts += 1;
        push_rejection(
            &mut attempts,
            tier,
            ProfileCandidateId::TierAggregate,
            locator,
            ProfileRejection::CandidateLimit {
                actual: manifest_count,
                maximum: MAX_MANIFEST_CANDIDATES,
            },
        );
    } else {
        if !request.manifest_dcps.is_empty() {
            highest_attempted_tier.get_or_insert(CandidateTier::ManifestDcp);
            let mut exact: Vec<&ManifestDcpCandidate<'_>> = request
                .manifest_dcps
                .iter()
                .filter(|candidate| camera_binding_matches(request.camera, candidate.camera))
                .collect();
            exact.sort_by(|left, right| left.entry_id.cmp(right.entry_id));
            if exact.len() > MAX_CANDIDATES_PER_TIER {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::ManifestDcp,
                    ProfileCandidateId::TierAggregate,
                    "<manifest DCP tier>",
                    ProfileRejection::CandidateLimit {
                        actual: exact.len(),
                        maximum: MAX_CANDIDATES_PER_TIER,
                    },
                );
            } else if exact.len() > 1 {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::ManifestDcp,
                    ProfileCandidateId::TierAggregate,
                    "<manifest DCP tier>",
                    ProfileRejection::AmbiguousTier {
                        exact_matches: exact.len(),
                    },
                );
            } else if let Some(candidate) = exact.first() {
                match evaluate_dcp(
                    candidate.blob,
                    Some(candidate.expected_sha256),
                    request.camera,
                    request.wb_gains,
                    DcpModelPolicy::ManifestExpected(candidate.expected_unique_camera_model),
                ) {
                    Ok(success) => {
                        push_attempt(
                            &mut attempts,
                            CandidateTier::ManifestDcp,
                            manifest_candidate_id(candidate.entry_id),
                            candidate.blob.locator,
                            ProfileAttemptOutcome::Selected,
                        );
                        return dcp_resolution(
                            camera,
                            attempts,
                            CandidateTier::ManifestDcp,
                            manifest_candidate_id(candidate.entry_id),
                            candidate.blob.locator,
                            success,
                        );
                    }
                    Err(rejection) => {
                        rejected_attempts += 1;
                        push_rejection(
                            &mut attempts,
                            CandidateTier::ManifestDcp,
                            manifest_candidate_id(candidate.entry_id),
                            candidate.blob.locator,
                            rejection,
                        );
                    }
                }
            } else {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::ManifestDcp,
                    ProfileCandidateId::TierAggregate,
                    "<manifest DCP tier>",
                    ProfileRejection::CameraBindingMismatch,
                );
            }
        }

        if !request.trusted_scene_iccs.is_empty() {
            highest_attempted_tier.get_or_insert(CandidateTier::TrustedSceneIcc);
            let mut exact: Vec<&TrustedSceneIccCandidate<'_>> = request
                .trusted_scene_iccs
                .iter()
                .filter(|candidate| camera_binding_matches(request.camera, candidate.camera))
                .collect();
            exact.sort_by(|left, right| left.entry_id.cmp(right.entry_id));
            if exact.len() > MAX_CANDIDATES_PER_TIER {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::TrustedSceneIcc,
                    ProfileCandidateId::TierAggregate,
                    "<scene ICC tier>",
                    ProfileRejection::CandidateLimit {
                        actual: exact.len(),
                        maximum: MAX_CANDIDATES_PER_TIER,
                    },
                );
            } else if exact.len() > 1 {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::TrustedSceneIcc,
                    ProfileCandidateId::TierAggregate,
                    "<scene ICC tier>",
                    ProfileRejection::AmbiguousTier {
                        exact_matches: exact.len(),
                    },
                );
            } else if let Some(candidate) = exact.first() {
                let result = if candidate.blob.bytes.len() > MAX_SCENE_CAMERA_ICC_BYTES {
                    Err(ProfileRejection::SceneIcc(SceneCameraIccError::TooLarge {
                        actual: candidate.blob.bytes.len(),
                        maximum: MAX_SCENE_CAMERA_ICC_BYTES,
                    }))
                } else {
                    let digest = sha256(candidate.blob.bytes);
                    if digest != candidate.expected_sha256 {
                        Err(ProfileRejection::HashMismatch {
                            expected: candidate.expected_sha256,
                            actual: digest,
                        })
                    } else {
                        SceneCameraIcc::new(candidate.blob.bytes, candidate.allow_missing_ciis)
                            .map(|profile| (digest, profile))
                            .map_err(ProfileRejection::SceneIcc)
                    }
                };
                match result {
                    Ok((digest, profile)) => {
                        let metadata = profile.metadata().clone();
                        push_attempt(
                            &mut attempts,
                            CandidateTier::TrustedSceneIcc,
                            manifest_candidate_id(candidate.entry_id),
                            candidate.blob.locator,
                            ProfileAttemptOutcome::Selected,
                        );
                        let provenance = ResolverProvenance {
                            resolver_version: RESOLVER_VERSION,
                            camera,
                            selected: SelectedProfileProvenance::SceneIcc {
                                tier: CandidateTier::TrustedSceneIcc,
                                candidate_id: manifest_candidate_id(candidate.entry_id),
                                locator: bounded_text(candidate.blob.locator),
                                sha256: digest,
                                metadata,
                            },
                            attempts,
                        };
                        return CameraProfileResolution {
                            characterization: ResolvedCameraCharacterization::SceneIcc(profile),
                            provenance,
                        };
                    }
                    Err(rejection) => {
                        rejected_attempts += 1;
                        push_rejection(
                            &mut attempts,
                            CandidateTier::TrustedSceneIcc,
                            manifest_candidate_id(candidate.entry_id),
                            candidate.blob.locator,
                            rejection,
                        );
                    }
                }
            } else {
                rejected_attempts += 1;
                push_rejection(
                    &mut attempts,
                    CandidateTier::TrustedSceneIcc,
                    ProfileCandidateId::TierAggregate,
                    "<scene ICC tier>",
                    ProfileRejection::CameraBindingMismatch,
                );
            }
        }
    }

    let fallback_reason = match highest_attempted_tier {
        Some(highest_attempted_tier) => DecoderFallbackReason::AllProfileCandidatesRejected {
            highest_attempted_tier,
            rejected_attempts,
        },
        None => DecoderFallbackReason::NoProfileCandidates,
    };
    push_attempt(
        &mut attempts,
        CandidateTier::DecoderFallback,
        ProfileCandidateId::DecoderFallback,
        "<decoder matrix>",
        ProfileAttemptOutcome::Selected,
    );
    let fallback = resolve_decoder_matrix(
        request.decoder_fallback.backend,
        &camera.make,
        &camera.model,
        request.decoder_fallback.camera_to_xyz,
    );
    CameraProfileResolution {
        characterization: ResolvedCameraCharacterization::DecoderMatrix(fallback),
        provenance: ResolverProvenance {
            resolver_version: RESOLVER_VERSION,
            camera,
            selected: SelectedProfileProvenance::DecoderMatrix {
                backend: request.decoder_fallback.backend,
                reason: fallback_reason,
            },
            attempts,
        },
    }
}

fn evaluate_dcp(
    blob: ProfileBlob<'_>,
    expected_sha256: Option<Sha256Digest>,
    camera: CameraIdentityRef<'_>,
    wb_gains: [f64; 3],
    model_policy: DcpModelPolicy<'_>,
) -> Result<DcpSuccess, ProfileRejection> {
    if blob.bytes.len() > MAX_STANDALONE_DCP_BYTES {
        return Err(ProfileRejection::DcpParse(DcpError::FileTooLarge {
            actual: blob.bytes.len(),
            maximum: MAX_STANDALONE_DCP_BYTES,
        }));
    }
    let digest = sha256(blob.bytes);
    if let Some(expected) = expected_sha256 {
        if digest != expected {
            return Err(ProfileRejection::HashMismatch {
                expected,
                actual: digest,
            });
        }
    }
    let profile = dcp::parse(blob.bytes).map_err(ProfileRejection::DcpParse)?;
    validate_dcp_model(&profile, camera, model_policy)?;
    let transform = dcp_transform::build_camera_transform(&profile, wb_gains)
        .map_err(ProfileRejection::DcpTransform)?;
    Ok(DcpSuccess {
        profile,
        transform,
        digest,
    })
}

fn validate_dcp_model(
    profile: &DcpProfile,
    camera: CameraIdentityRef<'_>,
    policy: DcpModelPolicy<'_>,
) -> Result<(), ProfileRejection> {
    match policy {
        DcpModelPolicy::RequiredCameraMatch => match profile.unique_camera_model.as_deref() {
            Some(declared) if declared_model_matches_camera(declared, camera) => Ok(()),
            Some(declared) => Err(ProfileRejection::ProfileCameraModelMismatch {
                declared: bounded_text(declared),
            }),
            None => Err(ProfileRejection::MissingProfileCameraModel),
        },
        DcpModelPolicy::EmbeddedParentMatch => match profile.unique_camera_model.as_deref() {
            Some(declared) if !declared_model_matches_camera(declared, camera) => {
                Err(ProfileRejection::ProfileCameraModelMismatch {
                    declared: bounded_text(declared),
                })
            }
            _ => Ok(()),
        },
        DcpModelPolicy::ManifestExpected(expected) => {
            if !declared_model_matches_camera(expected, camera) {
                return Err(ProfileRejection::CameraBindingMismatch);
            }
            match profile.unique_camera_model.as_deref() {
                Some(declared) if canonical_equal(declared, expected) => Ok(()),
                Some(declared) => Err(ProfileRejection::ProfileCameraModelMismatch {
                    declared: bounded_text(declared),
                }),
                None => Err(ProfileRejection::MissingProfileCameraModel),
            }
        }
    }
}

fn dcp_resolution(
    camera: ResolverCameraIdentity,
    attempts: Vec<ProfileAttempt>,
    tier: CandidateTier,
    candidate_id: ProfileCandidateId,
    locator: &str,
    success: DcpSuccess,
) -> CameraProfileResolution {
    let provenance = SelectedProfileProvenance::Dcp {
        tier,
        candidate_id,
        locator: bounded_text(locator),
        sha256: success.digest,
        unique_camera_model: success
            .profile
            .unique_camera_model
            .as_deref()
            .map(bounded_text),
        profile_name: success.profile.profile_name.as_deref().map(bounded_text),
        illuminants: success
            .profile
            .calibrations
            .iter()
            .map(|calibration| calibration.illuminant)
            .collect(),
        selected_cct_kelvin: success.transform.selection.cct_kelvin,
        second_calibration_weight: success.transform.selection.second_calibration_weight,
    };
    CameraProfileResolution {
        characterization: ResolvedCameraCharacterization::Dcp {
            profile: success.profile,
            transform: success.transform,
        },
        provenance: ResolverProvenance {
            resolver_version: RESOLVER_VERSION,
            camera,
            selected: provenance,
            attempts,
        },
    }
}

fn push_rejection(
    attempts: &mut Vec<ProfileAttempt>,
    tier: CandidateTier,
    candidate_id: ProfileCandidateId,
    locator: &str,
    rejection: ProfileRejection,
) {
    push_attempt(
        attempts,
        tier,
        candidate_id,
        locator,
        ProfileAttemptOutcome::Rejected(rejection),
    );
}

fn push_attempt(
    attempts: &mut Vec<ProfileAttempt>,
    tier: CandidateTier,
    candidate_id: ProfileCandidateId,
    locator: &str,
    outcome: ProfileAttemptOutcome,
) {
    if attempts.len() < MAX_RECORDED_ATTEMPTS {
        attempts.push(ProfileAttempt {
            tier,
            candidate_id,
            locator: bounded_text(locator),
            outcome,
        });
    }
}

fn manifest_candidate_id(entry_id: &str) -> ProfileCandidateId {
    ProfileCandidateId::Manifest {
        entry_id: bounded_text_to(entry_id, MAX_MANIFEST_ENTRY_ID_BYTES),
    }
}

fn bounded_camera_identity(camera: CameraIdentityRef<'_>) -> ResolverCameraIdentity {
    ResolverCameraIdentity {
        make: bounded_text_to(camera.make.trim(), MAX_IDENTITY_COMPONENT_BYTES),
        model: bounded_text_to(camera.model.trim(), MAX_IDENTITY_COMPONENT_BYTES),
    }
}

fn bounded_text(value: &str) -> String {
    bounded_text_to(value, MAX_PROVENANCE_TEXT_BYTES)
}

fn bounded_text_to(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Conservative exact-key normalization: Unicode compatibility composition,
/// case folding through lowercase, and whitespace collapse. Punctuation,
/// vendor prefixes, digits, and token order are never removed or rearranged.
fn canonical_component(value: &str) -> Option<String> {
    if value.len() > MAX_IDENTITY_COMPONENT_BYTES {
        return None;
    }
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.nfkc().flat_map(char::to_lowercase) {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
        if output.len() > MAX_IDENTITY_COMPONENT_BYTES {
            return None;
        }
    }
    (!output.is_empty()).then_some(output)
}

fn canonical_equal(left: &str, right: &str) -> bool {
    match (canonical_component(left), canonical_component(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Exact, normalized camera-identity equality shared with discovery so both use
/// one identity contract. Whitespace/case/Unicode composition only; no fuzzy or
/// substring matching.
pub(crate) fn camera_binding_matches(
    left: CameraIdentityRef<'_>,
    right: CameraIdentityRef<'_>,
) -> bool {
    canonical_equal(left.make, right.make) && canonical_equal(left.model, right.model)
}

fn declared_model_matches_camera(declared: &str, camera: CameraIdentityRef<'_>) -> bool {
    let Some(declared) = canonical_component(declared) else {
        return false;
    };
    let Some(make) = canonical_component(camera.make) else {
        return false;
    };
    let Some(model) = canonical_component(camera.model) else {
        return false;
    };
    if declared == model {
        return true;
    }
    let Some(full_len) = make
        .len()
        .checked_add(1)
        .and_then(|len| len.checked_add(model.len()))
    else {
        return false;
    };
    if full_len > MAX_IDENTITY_COMPONENT_BYTES {
        return false;
    }
    let mut full = make;
    full.push(' ');
    full.push_str(&model);
    declared == full
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::camera_profile::icc_camera::SceneCameraIccImageState;
    use lcms2::{CIExyY, CIExyYTRIPLE, Profile, ProfileClassSignature, ToneCurve};

    const CAMERA: CameraIdentityRef<'static> = CameraIdentityRef {
        make: "Canon",
        model: "EOS R5",
    };
    const OTHER_CAMERA: CameraIdentityRef<'static> = CameraIdentityRef {
        make: "Nikon",
        model: "Z 8",
    };
    const WB_GAINS: [f64; 3] = [2.0, 1.0, 1.5];
    const CAMERA_TO_XYZ: [[f32; 4]; 3] = [
        [0.62, 0.21, 0.17, 0.0],
        [0.12, 0.81, 0.07, 0.0],
        [-0.02, 0.13, 0.89, 0.0],
    ];

    #[derive(Clone)]
    struct FixtureEntry {
        tag: u16,
        field_type: u16,
        count: u32,
        bytes: Vec<u8>,
    }

    fn put_u16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn ascii(tag: u16, field_type: u16, text: &str) -> FixtureEntry {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        FixtureEntry {
            tag,
            field_type,
            count: bytes.len() as u32,
            bytes,
        }
    }

    fn identity_matrix() -> FixtureEntry {
        let values = [1i32, 0, 0, 0, 1, 0, 0, 0, 1];
        let mut bytes = Vec::with_capacity(72);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
            bytes.extend_from_slice(&1i32.to_le_bytes());
        }
        FixtureEntry {
            tag: 50721,
            field_type: 10,
            count: 9,
            bytes,
        }
    }

    fn dcp_fixture(unique_model: Option<&str>, profile_name: &str) -> Vec<u8> {
        let mut entries = Vec::new();
        if let Some(model) = unique_model {
            entries.push(ascii(50708, 2, model));
        }
        entries.push(identity_matrix());
        entries.push(FixtureEntry {
            tag: 50778,
            field_type: 3,
            count: 1,
            bytes: 21u16.to_le_bytes().to_vec(),
        });
        entries.push(ascii(50936, 1, profile_name));

        let ifd_at = 8usize;
        let data_at = ifd_at + 2 + entries.len() * 12 + 4;
        let mut result = vec![0u8; data_at];
        result[0..2].copy_from_slice(b"II");
        put_u16(&mut result, 2, 0x4352);
        put_u32(&mut result, 4, ifd_at as u32);
        put_u16(&mut result, ifd_at, entries.len() as u16);

        let mut payload = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let at = ifd_at + 2 + index * 12;
            put_u16(&mut result, at, entry.tag);
            put_u16(&mut result, at + 2, entry.field_type);
            put_u32(&mut result, at + 4, entry.count);
            if entry.bytes.len() <= 4 {
                result[at + 8..at + 8 + entry.bytes.len()].copy_from_slice(&entry.bytes);
            } else {
                put_u32(&mut result, at + 8, (data_at + payload.len()) as u32);
                payload.extend_from_slice(&entry.bytes);
                if payload.len() % 2 != 0 {
                    payload.push(0);
                }
            }
        }
        result.extend_from_slice(&payload);
        result
    }

    fn scene_icc_without_ciis() -> Vec<u8> {
        let white = CIExyY {
            x: 0.3457,
            y: 0.3585,
            Y: 1.0,
        };
        let primaries = CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.7347,
                y: 0.2653,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.1596,
                y: 0.8404,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.0366,
                y: 0.0001,
                Y: 1.0,
            },
        };
        let curve = ToneCurve::new(1.0);
        let mut profile = Profile::new_rgb(&white, &primaries, &[&curve, &curve, &curve])
            .expect("self-authored RGB ICC");
        profile.set_version(4.3);
        profile.set_device_class(ProfileClassSignature::InputClass);
        profile.icc().expect("serialize self-authored ICC")
    }

    fn decoder() -> DecoderFallback<'static> {
        DecoderFallback {
            backend: RawDecoderBackend::Rawloader,
            camera_to_xyz: &CAMERA_TO_XYZ,
        }
    }

    fn selected_tier(resolution: &CameraProfileResolution) -> CandidateTier {
        match &resolution.provenance.selected {
            SelectedProfileProvenance::Dcp { tier, .. }
            | SelectedProfileProvenance::SceneIcc { tier, .. } => *tier,
            SelectedProfileProvenance::DecoderMatrix { .. } => CandidateTier::DecoderFallback,
        }
    }

    #[test]
    fn precedence_is_explicit_then_embedded_then_manifest_then_icc() {
        let dcp = dcp_fixture(Some("Canon EOS R5"), "fixture");
        let icc = scene_icc_without_ciis();
        let explicit = ProfileBlob {
            bytes: &dcp,
            locator: "explicit.dcp",
        };
        let embedded = [
            EmbeddedDcpCandidate {
                blob: ProfileBlob {
                    bytes: &dcp,
                    locator: "embedded-extra.dcp",
                },
                profile_index: 2,
            },
            EmbeddedDcpCandidate {
                blob: ProfileBlob {
                    bytes: &dcp,
                    locator: "embedded-primary.dcp",
                },
                profile_index: 0,
            },
        ];
        let manifest = [ManifestDcpCandidate {
            entry_id: "canon-r5",
            blob: ProfileBlob {
                bytes: &dcp,
                locator: "manifest.dcp",
            },
            camera: CAMERA,
            expected_sha256: sha256(&dcp),
            expected_unique_camera_model: "Canon EOS R5",
        }];
        let iccs = [TrustedSceneIccCandidate {
            entry_id: "canon-r5-icc",
            blob: ProfileBlob {
                bytes: &icc,
                locator: "manifest.icc",
            },
            camera: CAMERA,
            expected_sha256: sha256(&icc),
            allow_missing_ciis: true,
        }];

        let all = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: Some(explicit),
            embedded_dcps: &embedded,
            manifest_dcps: &manifest,
            trusted_scene_iccs: &iccs,
            decoder_fallback: decoder(),
        });
        assert_eq!(selected_tier(&all), CandidateTier::ExplicitDcp);
        assert_eq!(all.provenance.attempts.len(), 1);
        assert_eq!(
            all.provenance.attempts[0].candidate_id,
            ProfileCandidateId::Explicit
        );
        assert!(matches!(
            &all.provenance.selected,
            SelectedProfileProvenance::Dcp {
                candidate_id: ProfileCandidateId::Explicit,
                ..
            }
        ));

        let without_explicit = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &embedded,
            manifest_dcps: &manifest,
            trusted_scene_iccs: &iccs,
            decoder_fallback: decoder(),
        });
        assert_eq!(selected_tier(&without_explicit), CandidateTier::EmbeddedDcp);
        assert!(matches!(
            &without_explicit.provenance.selected,
            SelectedProfileProvenance::Dcp {
                candidate_id: ProfileCandidateId::Embedded { profile_index: 0 },
                locator,
                ..
            } if locator == "embedded-primary.dcp"
        ));

        let manifest_before_icc = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &manifest,
            trusted_scene_iccs: &iccs,
            decoder_fallback: decoder(),
        });
        assert_eq!(
            selected_tier(&manifest_before_icc),
            CandidateTier::ManifestDcp
        );
        assert!(matches!(
            &manifest_before_icc.provenance.selected,
            SelectedProfileProvenance::Dcp {
                candidate_id: ProfileCandidateId::Manifest { entry_id },
                ..
            } if entry_id == "canon-r5"
        ));

        let icc_only = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &[],
            trusted_scene_iccs: &iccs,
            decoder_fallback: decoder(),
        });
        assert_eq!(selected_tier(&icc_only), CandidateTier::TrustedSceneIcc);
        assert!(matches!(
            &icc_only.provenance.selected,
            SelectedProfileProvenance::SceneIcc {
                candidate_id: ProfileCandidateId::Manifest { entry_id },
                ..
            } if entry_id == "canon-r5-icc"
        ));
    }

    #[test]
    fn invalid_high_priority_candidates_fall_through_with_typed_attempts() {
        let wrong_model = dcp_fixture(Some("Canon EOS R6"), "wrong camera");
        let valid = dcp_fixture(Some("Canon EOS R5"), "manifest fallback");
        let embedded = [EmbeddedDcpCandidate {
            blob: ProfileBlob {
                bytes: &wrong_model,
                locator: "embedded-wrong.dcp",
            },
            profile_index: 0,
        }];
        let manifest = [ManifestDcpCandidate {
            entry_id: "r5-valid",
            blob: ProfileBlob {
                bytes: &valid,
                locator: "manifest-valid.dcp",
            },
            camera: CAMERA,
            expected_sha256: sha256(&valid),
            expected_unique_camera_model: "Canon EOS R5",
        }];
        let resolution = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: Some(ProfileBlob {
                bytes: b"bad",
                locator: "broken-explicit.dcp",
            }),
            embedded_dcps: &embedded,
            manifest_dcps: &manifest,
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });

        assert_eq!(selected_tier(&resolution), CandidateTier::ManifestDcp);
        assert_eq!(resolution.provenance.attempts.len(), 3);
        assert_eq!(
            resolution.provenance.attempts[0].candidate_id,
            ProfileCandidateId::Explicit
        );
        assert!(matches!(
            resolution.provenance.attempts[0].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::DcpParse(_))
        ));
        assert_eq!(
            resolution.provenance.attempts[1].candidate_id,
            ProfileCandidateId::Embedded { profile_index: 0 }
        );
        assert!(matches!(
            resolution.provenance.attempts[1].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::ProfileCameraModelMismatch { .. })
        ));
        assert_eq!(
            resolution.provenance.attempts[2].candidate_id,
            ProfileCandidateId::Manifest {
                entry_id: "r5-valid".to_owned(),
            }
        );
        assert_eq!(
            resolution.provenance.attempts[2].outcome,
            ProfileAttemptOutcome::Selected
        );
    }

    #[test]
    fn identity_matching_is_normalized_exact_and_never_fuzzy() {
        let exact = dcp_fixture(Some("  CANON   EOS R5  "), "exact");
        let accepted = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: Some(ProfileBlob {
                bytes: &exact,
                locator: "normalized-exact.dcp",
            }),
            embedded_dcps: &[],
            manifest_dcps: &[],
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        assert_eq!(selected_tier(&accepted), CandidateTier::ExplicitDcp);

        let prefix = dcp_fixture(Some("Canon EOS R5C"), "prefix mismatch");
        let rejected = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: Some(ProfileBlob {
                bytes: &prefix,
                locator: "prefix.dcp",
            }),
            embedded_dcps: &[],
            manifest_dcps: &[],
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        assert!(rejected.is_decoder_fallback());
        assert!(matches!(
            rejected.provenance.attempts[0].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::ProfileCameraModelMismatch { .. })
        ));
    }

    #[test]
    fn manifest_hash_mismatch_never_parses_or_selects_candidate() {
        let dcp = dcp_fixture(Some("Canon EOS R5"), "hash fixture");
        let manifest = [ManifestDcpCandidate {
            entry_id: "tampered",
            blob: ProfileBlob {
                bytes: &dcp,
                locator: "tampered.dcp",
            },
            camera: CAMERA,
            expected_sha256: Sha256Digest::new([0x5a; 32]),
            expected_unique_camera_model: "Canon EOS R5",
        }];
        let resolution = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &manifest,
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        assert!(resolution.is_decoder_fallback());
        assert!(matches!(
            resolution.provenance.attempts[0].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::HashMismatch { .. })
        ));
        assert!(matches!(
            resolution.provenance.selected,
            SelectedProfileProvenance::DecoderMatrix {
                reason: DecoderFallbackReason::AllProfileCandidatesRejected {
                    highest_attempted_tier: CandidateTier::ManifestDcp,
                    rejected_attempts: 1,
                },
                ..
            }
        ));
        assert_eq!(
            resolution.provenance.attempts[0].candidate_id,
            ProfileCandidateId::Manifest {
                entry_id: "tampered".to_owned(),
            }
        );
    }

    #[test]
    fn missing_icc_scene_state_requires_manifest_trust_override() {
        let icc = scene_icc_without_ciis();
        let digest = sha256(&icc);
        let untrusted = [TrustedSceneIccCandidate {
            entry_id: "icc-without-ciis",
            blob: ProfileBlob {
                bytes: &icc,
                locator: "camera.icc",
            },
            camera: CAMERA,
            expected_sha256: digest,
            allow_missing_ciis: false,
        }];
        let rejected = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &[],
            trusted_scene_iccs: &untrusted,
            decoder_fallback: decoder(),
        });
        assert!(rejected.is_decoder_fallback());
        assert!(matches!(
            rejected.provenance.attempts[0].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::SceneIcc(
                SceneCameraIccError::MissingImageState
            ))
        ));

        let trusted = [TrustedSceneIccCandidate {
            allow_missing_ciis: true,
            ..untrusted[0]
        }];
        let accepted = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &[],
            trusted_scene_iccs: &trusted,
            decoder_fallback: decoder(),
        });
        assert_eq!(selected_tier(&accepted), CandidateTier::TrustedSceneIcc);
        assert!(matches!(
            accepted.provenance.selected,
            SelectedProfileProvenance::SceneIcc {
                metadata: SceneCameraIccMetadata {
                    image_state: SceneCameraIccImageState::TrustedSceneOverride,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn decoder_fallback_matrix_remains_bit_exact() {
        let resolution = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &[],
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        let expected = resolve_decoder_matrix(
            RawDecoderBackend::Rawloader,
            CAMERA.make,
            CAMERA.model,
            &CAMERA_TO_XYZ,
        );
        let ResolvedCameraCharacterization::DecoderMatrix(actual) = &resolution.characterization
        else {
            panic!("expected decoder fallback");
        };
        for row in 0..3 {
            for column in 0..3 {
                assert_eq!(
                    actual.camera_to_linear_srgb[row][column].to_bits(),
                    expected.camera_to_linear_srgb[row][column].to_bits()
                );
            }
        }
        assert!(matches!(
            resolution.provenance.selected,
            SelectedProfileProvenance::DecoderMatrix {
                reason: DecoderFallbackReason::NoProfileCandidates,
                ..
            }
        ));
        assert_eq!(
            resolution.provenance.attempts[0].candidate_id,
            ProfileCandidateId::DecoderFallback
        );
    }

    #[test]
    fn exact_manifest_filtering_and_limits_are_bounded() {
        let dcp = dcp_fixture(Some("Canon EOS R5"), "ambiguous");
        let digest = sha256(&dcp);
        let candidate = ManifestDcpCandidate {
            entry_id: "one",
            blob: ProfileBlob {
                bytes: &dcp,
                locator: "one.dcp",
            },
            camera: CAMERA,
            expected_sha256: digest,
            expected_unique_camera_model: "Canon EOS R5",
        };
        let ambiguous = [
            candidate,
            ManifestDcpCandidate {
                entry_id: "two",
                blob: ProfileBlob {
                    bytes: &dcp,
                    locator: "two.dcp",
                },
                ..candidate
            },
        ];
        let collision = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &ambiguous,
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        assert!(collision.is_decoder_fallback());
        assert!(matches!(
            collision.provenance.attempts[0].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::AmbiguousTier { exact_matches: 2 })
        ));
        assert_eq!(
            collision.provenance.attempts[0].candidate_id,
            ProfileCandidateId::TierAggregate
        );

        let exact_flood = [candidate; MAX_CANDIDATES_PER_TIER + 1];
        let exact_bounded = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &exact_flood,
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        assert!(exact_bounded.provenance.attempts.len() <= MAX_RECORDED_ATTEMPTS);
        assert!(matches!(
            exact_bounded.provenance.attempts[0].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::CandidateLimit {
                actual,
                maximum: MAX_CANDIDATES_PER_TIER,
            }) if actual == MAX_CANDIDATES_PER_TIER + 1
        ));

        let nonmatching = ManifestDcpCandidate {
            camera: OTHER_CAMERA,
            ..candidate
        };
        let long_entry_id = "x".repeat(MAX_MANIFEST_ENTRY_ID_BYTES + 17);
        let exact_candidate = ManifestDcpCandidate {
            entry_id: &long_entry_id,
            ..candidate
        };
        let mut noisy_catalog = vec![nonmatching; MAX_CANDIDATES_PER_TIER + 1];
        noisy_catalog.push(exact_candidate);
        let selected = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &noisy_catalog,
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        assert_eq!(selected_tier(&selected), CandidateTier::ManifestDcp);
        assert!(matches!(
            &selected.provenance.selected,
            SelectedProfileProvenance::Dcp {
                candidate_id: ProfileCandidateId::Manifest { entry_id },
                ..
            } if entry_id.len() == MAX_MANIFEST_ENTRY_ID_BYTES
                && long_entry_id.starts_with(entry_id)
        ));

        let global_flood = vec![nonmatching; MAX_MANIFEST_CANDIDATES + 1];
        let globally_bounded = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &global_flood,
            trusted_scene_iccs: &[],
            decoder_fallback: decoder(),
        });
        assert!(globally_bounded.is_decoder_fallback());
        assert_eq!(
            globally_bounded.provenance.attempts[0].candidate_id,
            ProfileCandidateId::TierAggregate
        );
        assert!(matches!(
            globally_bounded.provenance.attempts[0].outcome,
            ProfileAttemptOutcome::Rejected(ProfileRejection::CandidateLimit {
                actual,
                maximum: MAX_MANIFEST_CANDIDATES,
            }) if actual == MAX_MANIFEST_CANDIDATES + 1
        ));
    }

    #[test]
    fn scene_icc_filters_nonmatching_manifest_entries_before_tier_cap() {
        let icc = scene_icc_without_ciis();
        let candidate = TrustedSceneIccCandidate {
            entry_id: "canon-r5-icc",
            blob: ProfileBlob {
                bytes: &icc,
                locator: "camera.icc",
            },
            camera: CAMERA,
            expected_sha256: sha256(&icc),
            allow_missing_ciis: true,
        };
        let nonmatching = TrustedSceneIccCandidate {
            camera: OTHER_CAMERA,
            ..candidate
        };
        let mut catalog = vec![nonmatching; MAX_CANDIDATES_PER_TIER + 1];
        catalog.push(candidate);
        let resolution = resolve(ResolveRequest {
            camera: CAMERA,
            wb_gains: WB_GAINS,
            explicit_dcp: None,
            embedded_dcps: &[],
            manifest_dcps: &[],
            trusted_scene_iccs: &catalog,
            decoder_fallback: decoder(),
        });
        assert_eq!(selected_tier(&resolution), CandidateTier::TrustedSceneIcc);
        assert!(matches!(
            &resolution.provenance.selected,
            SelectedProfileProvenance::SceneIcc {
                candidate_id: ProfileCandidateId::Manifest { entry_id },
                ..
            } if entry_id == "canon-r5-icc"
        ));
    }

    #[test]
    fn sha256_uses_stable_lowercase_content_identity() {
        assert_eq!(
            sha256(b"abc").to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
