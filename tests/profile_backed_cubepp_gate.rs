//! Phase-1 profile-backed characterization gate.
//!
//! Renders the real Cube++ Canon EOS 550D capture `20_2660.CR2` through the
//! iAi RAW pipeline twice: once with the installed Adobe Standard DCP forced via
//! `IAI_CAMERA_PROFILE`, and once with the decoder-matrix fallback. It asserts
//! the resolver actually selected that DCP (exact model + content hash, A/D65
//! dual calibration), that a profile-backed render defaults to no embedded-JPEG
//! match, and that the scene master keeps finite signed/HDR values with no
//! hidden clamp. The SpyderCube neutral residual is recorded for both paths as
//! an angular error; this is a camera-space neutrality check, NOT a DeltaE or
//! colorimetric-accuracy claim.
//!
//! Ignored and env-driven: it needs the external Cube++ fixture and the locally
//! installed Adobe profile, neither of which lives in the repository.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use iai::core::camera_profile::resolver::{
    CandidateTier, ProfileCandidateId, SelectedProfileProvenance,
};
use iai::core::camera_profile::JpegMatchMode;
use iai::core::develop_scene::{f16_bits_to_f32, SceneSource};
use iai::formats::raw::RawImporter;
use iai::formats::Importer;

const CR2_BYTES: u64 = 24_867_695;
const CR2_SHA256: &str = "0d5de5728cac4855572acc46b47b07c2598cab5df79b19207713f09ba23c2bdd";
const DCP_BYTES: u64 = 55_844;
const DCP_SHA256: &str = "4401efb46ed414d6153fa37adf93bf9e038f9314d42896826ab6cb07b4e09103";
const EXPECTED_MODEL: &str = "Canon EOS 550D";
const STANDARD_A: u16 = 17;
const D65: u16 = 21;

// SpyderCube neutral faces in the Cube++ 2592x1728 reference frame.
const REFERENCE_SIZE: [f64; 2] = [2592.0, 1728.0];
const REGISTERED_SIZE: [usize; 2] = [5184, 3456];
const LEFT_TRIANGLE: [[f64; 2]; 3] = [[2128.0, 1366.0], [2327.0, 1219.0], [2332.0, 1411.0]];
const RIGHT_TRIANGLE: [[f64; 2]; 3] = [[2525.0, 1355.0], [2327.0, 1220.0], [2331.0, 1412.0]];

fn cr2_path() -> PathBuf {
    let path = std::env::var_os("IAI_CUBEPP_REFERENCE_DIR")
        .map(PathBuf::from)
        .map(|directory| directory.join("20_2660.CR2"))
        .unwrap_or_else(|| {
            PathBuf::from(
                "target/color-reference-cache/cubepp-raw-fixture/Cube++/auxiliary/source/CR2/20_2660.CR2",
            )
        });
    assert!(
        path.is_file(),
        "set IAI_CUBEPP_REFERENCE_DIR or populate the target cache: {}",
        path.display()
    );
    path
}

fn dcp_path() -> PathBuf {
    let path = std::env::var_os("IAI_CANON_550D_DCP")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                "C:/ProgramData/Adobe/CameraRaw/CameraProfiles/Adobe Standard/Canon EOS 550D Adobe Standard.dcp",
            )
        });
    assert!(
        path.is_file(),
        "set IAI_CANON_550D_DCP to the installed Adobe Standard profile: {}",
        path.display()
    );
    path
}

fn sha256_hex(path: &Path) -> String {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("open fixture {}: {error}", path.display()));
    let mut digest = hmac_sha256::Hash::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("hash fixture {}: {error}", path.display()));
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn render(path: &Path) -> iai::core::canvas::Canvas {
    RawImporter
        .import(path)
        .expect("render Cube++ CR2 through iAi")
}

fn point_in_triangle(point: [f64; 2], triangle: [[f64; 2]; 3]) -> bool {
    let sign = |p: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (p[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (p[1] - b[1])
    };
    let d1 = sign(point, triangle[0], triangle[1]);
    let d2 = sign(point, triangle[1], triangle[2]);
    let d3 = sign(point, triangle[2], triangle[0]);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}

fn normalize_sum(mut rgb: [f64; 3]) -> [f64; 3] {
    let sum = rgb.iter().sum::<f64>();
    for value in &mut rgb {
        *value /= sum;
    }
    rgb
}

fn angular_error_degrees(first: [f64; 3], second: [f64; 3]) -> f64 {
    let dot = first
        .iter()
        .zip(second)
        .map(|(first, second)| first * second)
        .sum::<f64>();
    let norm = |value: [f64; 3]| {
        value
            .iter()
            .map(|channel| channel * channel)
            .sum::<f64>()
            .sqrt()
    };
    (dot / (norm(first) * norm(second)))
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

fn trimmed_mean(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let trim = values.len() / 10;
    let kept = &values[trim..values.len() - trim];
    kept.iter().sum::<f64>() / kept.len() as f64
}

/// Mean scene-linear RGB (sum-normalized) inside one SpyderCube neutral face.
fn sample_scene_triangle(scene: &SceneSource, triangle: [[f64; 2]; 3]) -> [f64; 3] {
    assert!(
        scene.width as usize >= REGISTERED_SIZE[0] && scene.height as usize >= REGISTERED_SIZE[1]
    );
    let registered_left = (scene.width as usize - REGISTERED_SIZE[0]) / 2;
    let registered_top = (scene.height as usize - REGISTERED_SIZE[1]) / 2;
    let min_x = triangle.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let max_x = triangle
        .iter()
        .map(|p| p[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = triangle.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
    let max_y = triangle
        .iter()
        .map(|p| p[1])
        .fold(f64::NEG_INFINITY, f64::max);
    let x0 = registered_left as u32
        + (min_x * REGISTERED_SIZE[0] as f64 / REFERENCE_SIZE[0]).floor() as u32;
    let x1 = registered_left as u32
        + (max_x * REGISTERED_SIZE[0] as f64 / REFERENCE_SIZE[0]).ceil() as u32;
    let y0 = registered_top as u32
        + (min_y * REGISTERED_SIZE[1] as f64 / REFERENCE_SIZE[1]).floor() as u32;
    let y1 = registered_top as u32
        + (max_y * REGISTERED_SIZE[1] as f64 / REFERENCE_SIZE[1]).ceil() as u32;
    let mut samples = [Vec::new(), Vec::new(), Vec::new()];
    for y in y0..y1.min(scene.height) {
        for x in x0..x1.min(scene.width) {
            let reference = [
                (x as usize - registered_left) as f64 * REFERENCE_SIZE[0]
                    / REGISTERED_SIZE[0] as f64,
                (y as usize - registered_top) as f64 * REFERENCE_SIZE[1]
                    / REGISTERED_SIZE[1] as f64,
            ];
            if !point_in_triangle(reference, triangle) {
                continue;
            }
            let rgb = scene
                .color_pipeline
                .working
                .to_linear_srgb(scene.get_rgb(x, y));
            for channel in 0..3 {
                if rgb[channel].is_finite() && rgb[channel] > 0.0 {
                    samples[channel].push(rgb[channel] as f64);
                }
            }
        }
    }
    assert!(samples.iter().all(|values| values.len() > 100));
    normalize_sum(samples.each_mut().map(|values| trimmed_mean(values)))
}

#[test]
#[ignore = "requires external Cube++ 20_2660.CR2 and installed Canon EOS 550D DCP"]
fn cubepp_dcp_profile_backed_gate() {
    let cr2 = cr2_path();
    assert_eq!(std::fs::metadata(&cr2).unwrap().len(), CR2_BYTES);
    assert_eq!(sha256_hex(&cr2), CR2_SHA256, "Cube++ CR2 identity");

    let dcp = dcp_path();
    assert_eq!(std::fs::metadata(&dcp).unwrap().len(), DCP_BYTES);
    assert_eq!(sha256_hex(&dcp), DCP_SHA256, "Adobe Standard DCP identity");

    // --- Decoder-matrix fallback render (no profile) ---
    std::env::remove_var("IAI_CAMERA_PROFILE");
    std::env::set_var("IAI_RAW_JPEG_MATCH", "none");
    let fallback = render(&cr2);
    let fallback_scene = fallback
        .develop_source
        .as_ref()
        .expect("fallback RAW attaches a scene");
    let fallback_char = fallback_scene
        .camera_profile
        .as_ref()
        .expect("fallback records characterization");
    assert!(
        fallback_char.is_decoder_fallback(),
        "no profile => decoder-matrix fallback, got {:?}",
        fallback_char.resolution.selected
    );
    let fallback_left = sample_scene_triangle(fallback_scene, LEFT_TRIANGLE);
    let fallback_right = sample_scene_triangle(fallback_scene, RIGHT_TRIANGLE);

    // --- Profile-backed DCP render ---
    std::env::set_var("IAI_CAMERA_PROFILE", &dcp);
    let dcp_canvas = render(&cr2);
    std::env::remove_var("IAI_CAMERA_PROFILE");
    std::env::remove_var("IAI_RAW_JPEG_MATCH");

    let scene = dcp_canvas
        .develop_source
        .as_ref()
        .expect("DCP RAW attaches a scene");
    let characterization = scene
        .camera_profile
        .as_ref()
        .expect("DCP render records characterization");

    // Provenance: an explicit DCP was actually selected, with the exact model,
    // content hash, and A/D65 dual calibration.
    let SelectedProfileProvenance::Dcp {
        sha256,
        unique_camera_model,
        illuminants,
        selected_cct_kelvin,
        second_calibration_weight,
        ..
    } = &characterization.resolution.selected
    else {
        panic!(
            "expected DCP selection, got {:?}",
            characterization.resolution.selected
        );
    };
    assert_eq!(sha256.to_hex(), DCP_SHA256, "selected DCP content hash");
    assert_eq!(
        unique_camera_model.as_deref(),
        Some(EXPECTED_MODEL),
        "selected DCP model"
    );
    assert_eq!(
        illuminants.as_slice(),
        [STANDARD_A, D65],
        "Adobe Standard is a Standard-A/D65 dual-illuminant profile"
    );
    assert!(
        selected_cct_kelvin.is_finite() && (1000.0..25_000.0).contains(selected_cct_kelvin),
        "selected CCT {selected_cct_kelvin} K is implausible"
    );
    assert!(
        (0.0..=1.0).contains(second_calibration_weight),
        "second calibration weight {second_calibration_weight} out of [0,1]"
    );

    // Default embedded-JPEG match for a profile-backed render is None, and no
    // camera picture-style curve is fitted.
    assert_eq!(
        characterization.jpeg_match,
        JpegMatchMode::None,
        "profile-backed default must be no JPEG match"
    );
    assert!(
        scene.camera_rgb_curve.is_none(),
        "profile-backed render fits no camera RGB curve"
    );

    // The scene master keeps finite signed/HDR values with no hidden clamp.
    let mut non_finite = 0usize;
    let mut above_one = 0usize;
    let mut below_zero = 0usize;
    let mut max_value = f32::NEG_INFINITY;
    let mut min_value = f32::INFINITY;
    for pixel in scene.half.chunks_exact(4) {
        for &bits in &pixel[..3] {
            let value = f16_bits_to_f32(bits);
            if !value.is_finite() {
                non_finite += 1;
                continue;
            }
            max_value = max_value.max(value);
            min_value = min_value.min(value);
            if value > 1.0 {
                above_one += 1;
            }
            if value < 0.0 {
                below_zero += 1;
            }
        }
    }
    assert_eq!(non_finite, 0, "scene master must be finite");
    assert!(
        above_one > 0,
        "highlight headroom above 1.0 must survive (max={max_value})"
    );

    // SpyderCube neutral residual, recorded for both paths. Camera-space
    // neutrality only; this is NOT a DeltaE / colorimetric-accuracy claim.
    let neutral = normalize_sum([1.0, 1.0, 1.0]);
    let dcp_left = sample_scene_triangle(scene, LEFT_TRIANGLE);
    let dcp_right = sample_scene_triangle(scene, RIGHT_TRIANGLE);
    let dcp_left_error = angular_error_degrees(dcp_left, neutral);
    let dcp_right_error = angular_error_degrees(dcp_right, neutral);
    let fallback_left_error = angular_error_degrees(fallback_left, neutral);
    let fallback_right_error = angular_error_degrees(fallback_right, neutral);

    println!(
        "cubepp,dcp_profile_backed,model={EXPECTED_MODEL},cct_k={selected_cct_kelvin:.1},weight={second_calibration_weight:.4},jpeg_match=none"
    );
    println!(
        "cubepp,dcp_scene,left={dcp_left:?},right={dcp_right:?},left_neutral_deg={dcp_left_error:.4},right_neutral_deg={dcp_right_error:.4}"
    );
    println!(
        "cubepp,fallback_scene,left={fallback_left:?},right={fallback_right:?},left_neutral_deg={fallback_left_error:.4},right_neutral_deg={fallback_right_error:.4}"
    );
    println!(
        "cubepp,scene_master,min={min_value},max={max_value},above_one={above_one},below_zero={below_zero}"
    );

    // Both characterizations should land the neutral faces close to neutral in
    // camera space. Generous bound; the exact residuals are recorded above.
    for error in [
        dcp_left_error,
        dcp_right_error,
        fallback_left_error,
        fallback_right_error,
    ] {
        assert!(
            error.is_finite() && error <= 8.0,
            "neutral residual {error}"
        );
    }
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "iai-manifest-gate-{tag}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// The manifest tier resolves the same DCP as the explicit override: build a
/// throwaway manifest that binds this camera to a local copy of the installed
/// profile, point `IAI_CAMERA_PROFILE_MANIFEST` at it, and confirm the resolver
/// selected the DCP through the ManifestDcp tier.
#[test]
#[ignore = "requires external Cube++ 20_2660.CR2 and installed Canon EOS 550D DCP"]
fn cubepp_dcp_via_manifest_tier() {
    let cr2 = cr2_path();
    let dcp = dcp_path();

    // Read the exact make/model the decoder reports so the manifest camera
    // binding is an exact, normalized match regardless of decoder naming.
    std::env::remove_var("IAI_CAMERA_PROFILE");
    std::env::remove_var("IAI_CAMERA_PROFILE_MANIFEST");
    std::env::set_var("IAI_RAW_JPEG_MATCH", "none");
    let probe = render(&cr2);
    let identity = probe
        .develop_source
        .as_ref()
        .and_then(|scene| scene.camera_profile.as_ref())
        .map(|characterization| characterization.resolution.camera.clone())
        .expect("probe records camera identity");
    drop(probe);

    // Stage a profile root containing a local copy of the DCP and a manifest.
    let root = unique_temp_dir("dcp");
    std::fs::copy(&dcp, root.join("canon_550d.dcp")).expect("copy DCP into profile root");
    let manifest = format!(
        r#"{{
            "schema_version": 1,
            "profiles": [
                {{
                    "id": "canon-550d-adobe",
                    "kind": "dcp",
                    "path": "canon_550d.dcp",
                    "sha256": "{DCP_SHA256}",
                    "cameras": [ {{ "make": "{}", "model": "{}" }} ],
                    "unique_camera_model": "{EXPECTED_MODEL}"
                }}
            ]
        }}"#,
        identity.make, identity.model
    );
    let manifest_path = root.join("manifest.json");
    std::fs::write(&manifest_path, manifest).expect("write manifest");

    std::env::set_var("IAI_CAMERA_PROFILE_MANIFEST", &manifest_path);
    let canvas = render(&cr2);
    std::env::remove_var("IAI_CAMERA_PROFILE_MANIFEST");
    std::env::remove_var("IAI_RAW_JPEG_MATCH");

    let characterization = canvas
        .develop_source
        .as_ref()
        .and_then(|scene| scene.camera_profile.as_ref())
        .expect("manifest render records characterization");

    let SelectedProfileProvenance::Dcp {
        tier,
        candidate_id,
        sha256,
        unique_camera_model,
        illuminants,
        ..
    } = &characterization.resolution.selected
    else {
        panic!(
            "expected DCP selection via manifest, got {:?}",
            characterization.resolution.selected
        );
    };
    assert_eq!(
        *tier,
        CandidateTier::ManifestDcp,
        "selected via manifest tier"
    );
    assert_eq!(
        *candidate_id,
        ProfileCandidateId::Manifest {
            entry_id: "canon-550d-adobe".to_owned(),
        }
    );
    assert_eq!(sha256.to_hex(), DCP_SHA256, "manifest DCP content hash");
    assert_eq!(unique_camera_model.as_deref(), Some(EXPECTED_MODEL));
    assert_eq!(illuminants.as_slice(), [STANDARD_A, D65]);
    assert_eq!(
        characterization.jpeg_match,
        JpegMatchMode::None,
        "profile-backed default is no JPEG match"
    );

    std::fs::remove_dir_all(&root).ok();
    println!("cubepp,manifest_tier,entry=canon-550d-adobe,tier=ManifestDcp,jpeg_match=none");
}
