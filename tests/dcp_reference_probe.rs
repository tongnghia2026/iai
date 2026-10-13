//! Optional external-profile validation for Phase 1 camera characterization.
//!
//! The profile remains outside the repository. Run explicitly with:
//! `IAI_DCP_REFERENCE_FILE`, and optionally `IAI_DCP_REFERENCE_SHA256` and
//! `IAI_DCP_REFERENCE_CAMERA_MODEL`, set in the environment.

use iai::core::camera_profile::dcp;
use std::path::PathBuf;

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = hmac_sha256::Hash::hash(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
#[ignore = "requires an external DCP via IAI_DCP_REFERENCE_FILE"]
fn external_dcp_profile_parses_with_provenance() {
    let path = PathBuf::from(
        std::env::var_os("IAI_DCP_REFERENCE_FILE")
            .expect("set IAI_DCP_REFERENCE_FILE to a locally licensed .dcp profile"),
    );
    let bytes = std::fs::read(&path).expect("read external DCP reference");
    let actual_sha256 = hex_sha256(&bytes);

    if let Some(expected) = std::env::var_os("IAI_DCP_REFERENCE_SHA256") {
        assert_eq!(
            actual_sha256,
            expected.to_string_lossy().trim().to_ascii_lowercase(),
            "external DCP fingerprint changed"
        );
    }

    let profile = dcp::parse(&bytes).expect("parse external DCP reference");
    assert!((1..=2).contains(&profile.calibrations.len()));

    if let Some(expected) = std::env::var_os("IAI_DCP_REFERENCE_CAMERA_MODEL") {
        let actual = profile
            .unique_camera_model
            .as_deref()
            .expect("external reference must declare UniqueCameraModel");
        assert_eq!(
            actual.trim().to_lowercase(),
            expected.to_string_lossy().trim().to_lowercase(),
            "external DCP camera identity mismatch"
        );
    }

    eprintln!(
        "dcp_reference path={} sha256={} model={:?} profile={:?} calibrations={} illuminants={:?} technical_huesat={} creative_tone={} creative_look={}",
        path.display(),
        actual_sha256,
        profile.unique_camera_model,
        profile.profile_name,
        profile.calibrations.len(),
        profile
            .calibrations
            .iter()
            .map(|calibration| calibration.illuminant)
            .collect::<Vec<_>>(),
        profile
            .calibrations
            .iter()
            .filter(|calibration| calibration.hue_sat_map.is_some())
            .count(),
        profile.creative.tone_curve.is_some(),
        profile.creative.look_table.is_some(),
    );
}
