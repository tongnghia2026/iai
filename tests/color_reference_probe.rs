//! Phase-0 measured-reference probe.
//!
//! This test consumes Middlebury's registered ColorChecker 24 dataset from an
//! external cache. The `*-raw.png` files are dcraw-rendered linear sRGB, not
//! sensor RAW files; the `*-jpg.png` files are camera-JPEG code values stored in
//! PNG containers. Consequently this probe validates the metric and records
//! D50-reference, current-tone, and camera-JPEG-likeness baselines separately.
//! It does not claim to measure `RawImporter` accuracy.
//!
//! ```text
//! IAI_COLOR_REFERENCE_DIR=target/color-reference-cache/checker24s \
//! IAI_COLOR_REFERENCE_OUT=target/phase0/checker24s \
//! cargo test --release --test color_reference_probe -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use iai::core::color_reference::{
    delta_e_2000, evaluate_colorchecker_linear, linear_srgb_to_lab_d50, ColorCheckerSummary,
    ReferenceError,
};
use iai::core::develop_scene::{render_default_look, SceneSource};
use iai::core::perceptual_color::linear_srgb_to_oklab;
use iai::core::working_color::WorkingColorSpace;

const GRID_COLUMNS: u32 = 6;
const GRID_ROWS: u32 = 4;
const EXPECTED_WIDTH: u32 = 390;
const EXPECTED_HEIGHT: u32 = 260;
const EXPECTED_ARCHIVE_SHA256: &str =
    "420534c8d56cfdd896241b2e96ed34e69df8e6de6a519e56489fe7de7826df74";
const EXPECTED_EXTRACTED_TREE_SHA256: &str =
    "9c9193c91f4cd8a4bec3051d21ce9f4e7b287a0587d17990d3bb883ea0df54b3";

fn assert_sha256(path: &Path, expected: &str) {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("open identity fixture {}: {error}", path.display()));
    let mut digest = hmac_sha256::Hash::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("hash identity fixture {}: {error}", path.display()));
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let actual = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        actual,
        expected,
        "fixture identity mismatch: {}",
        path.display()
    );
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn reference_root() -> Option<PathBuf> {
    std::env::var_os("IAI_COLOR_REFERENCE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            let default = PathBuf::from("target/color-reference-cache/checker24s");
            default.is_dir().then_some(default)
        })
}

fn reference_archive(root: &Path) -> Option<PathBuf> {
    std::env::var_os("IAI_COLOR_REFERENCE_ARCHIVE")
        .map(PathBuf::from)
        .or_else(|| {
            root.parent()
                .map(|parent| parent.join("checker24s-RAW-JPG.zip"))
                .filter(|path| path.is_file())
        })
}

fn assert_extracted_tree_sha256(root: &Path, expected: &str) {
    let mut files = walk_files(root);
    files.sort();
    let mut digest = hmac_sha256::Hash::new();
    digest.update(b"iai-reference-tree-sha256-v1\0");
    for path in files {
        let relative = path
            .strip_prefix(root)
            .expect("walked file must remain under the reference root")
            .to_string_lossy()
            .replace('\\', "/");
        digest.update((relative.len() as u64).to_le_bytes());
        digest.update(relative.as_bytes());
        let mut file = std::fs::File::open(&path)
            .unwrap_or_else(|error| panic!("open tree fixture {}: {error}", path.display()));
        let size = file.metadata().expect("read tree fixture metadata").len();
        digest.update(size.to_le_bytes());
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .unwrap_or_else(|error| panic!("hash tree fixture {}: {error}", path.display()));
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
    }
    let actual = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        actual,
        expected,
        "extracted fixture identity mismatch: {}",
        root.display()
    );
}

struct ExtractedChart {
    means: Vec<[f32; 3]>,
    /// Per-channel fraction of exact 0/1 samples in the 18 chromatic patches.
    chromatic_channel_clip_fraction: [f64; 3],
}

fn trimmed_patch_means(path: &Path, encoded_srgb: bool) -> Result<ExtractedChart, String> {
    let image = image::ImageReader::open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))?
        .decode()
        .map_err(|error| format!("decode {}: {error}", path.display()))?
        .to_rgb32f();
    let (width, height) = image.dimensions();
    if (width, height) != (EXPECTED_WIDTH, EXPECTED_HEIGHT) {
        return Err(format!(
            "{}: expected {}x{}, got {width}x{height}",
            path.display(),
            EXPECTED_WIDTH,
            EXPECTED_HEIGHT
        ));
    }

    let cell_width = width / GRID_COLUMNS;
    let cell_height = height / GRID_ROWS;
    let mut patches = Vec::with_capacity((GRID_COLUMNS * GRID_ROWS) as usize);
    let mut chromatic_clipped = [0usize; 3];
    let mut chromatic_sample_count = 0usize;
    for row in 0..GRID_ROWS {
        for column in 0..GRID_COLUMNS {
            // Registered cells are 65x65. Use the inner 60% and a 10% trimmed
            // vector mean so borders/noise cannot dominate or shift hue.
            let margin_x = cell_width / 5;
            let margin_y = cell_height / 5;
            let x0 = column * cell_width + margin_x;
            let x1 = (column + 1) * cell_width - margin_x;
            let y0 = row * cell_height + margin_y;
            let y1 = (row + 1) * cell_height - margin_y;
            let patch_index = (row * GRID_COLUMNS + column) as usize;
            let mut samples = Vec::new();
            for y in y0..y1 {
                for x in x0..x1 {
                    let pixel = image.get_pixel(x, y).0;
                    if patch_index < 18 {
                        for channel in 0..3 {
                            if pixel[channel] <= 0.0 || pixel[channel] >= 1.0 {
                                chromatic_clipped[channel] += 1;
                            }
                        }
                        chromatic_sample_count += 1;
                    }
                    samples.push(if encoded_srgb {
                        pixel.map(srgb_to_linear)
                    } else {
                        pixel
                    });
                }
            }
            let luma =
                |rgb: &[f32; 3]| 0.212_672_9 * rgb[0] + 0.715_152_2 * rgb[1] + 0.072_175 * rgb[2];
            samples.sort_by(|first, second| luma(first).total_cmp(&luma(second)));
            let trim = samples.len() / 10;
            let kept = &samples[trim..samples.len() - trim];
            let mut mean = [0.0f32; 3];
            for channel in 0..3 {
                mean[channel] =
                    kept.iter().map(|rgb| rgb[channel]).sum::<f32>() / kept.len() as f32;
            }
            patches.push(mean);
        }
    }

    // The bottom row is ordered white-to-black. This catches a rotated/flipped
    // chart before an apparently plausible aggregate can hide the mistake.
    let neutral_y = patches[18..]
        .iter()
        .map(|rgb| 0.212_672_9 * rgb[0] + 0.715_152_2 * rgb[1] + 0.072_175 * rgb[2])
        .collect::<Vec<_>>();
    if neutral_y.windows(2).any(|pair| pair[0] <= pair[1]) {
        return Err(format!(
            "{}: neutral row is not white-to-black: {neutral_y:?}",
            path.display()
        ));
    }
    Ok(ExtractedChart {
        means: patches,
        chromatic_channel_clip_fraction: chromatic_clipped
            .map(|count| count as f64 / chromatic_sample_count.max(1) as f64),
    })
}

fn render_current_tone(
    raw_linear: &[[f32; 3]],
    exposure_scale: f32,
) -> Result<Vec<[f32; 3]>, ReferenceError> {
    if raw_linear.len() != 24 {
        return Err(ReferenceError::PatchCount {
            expected: 24,
            actual: raw_linear.len(),
        });
    }
    let mut scene = SceneSource::new(GRID_COLUMNS, GRID_ROWS);
    for (index, &rgb) in raw_linear.iter().enumerate() {
        let working = WorkingColorSpace::LinearProPhoto
            .from_linear_srgb(rgb.map(|value| value * exposure_scale));
        scene.set_rgb(
            index as u32 % GRID_COLUMNS,
            index as u32 / GRID_COLUMNS,
            working,
        );
    }
    let encoded = render_default_look(&scene);
    Ok(encoded
        .chunks_exact(4)
        .map(|pixel| {
            [
                srgb_to_linear(pixel[0] as f32 / 65_535.0),
                srgb_to_linear(pixel[1] as f32 / 65_535.0),
                srgb_to_linear(pixel[2] as f32 / 65_535.0),
            ]
        })
        .collect())
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let values = values.collect::<Vec<_>>();
    values.iter().sum::<f64>() / values.len() as f64
}

fn global_delta_e_2000(reports: &[ColorCheckerSummary]) -> (f64, f64) {
    let mut values = reports
        .iter()
        .flat_map(|report| report.patches.iter().map(|patch| patch.delta_e_2000))
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let p95_index = ((values.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    (values[p95_index], values[values.len() - 1])
}

#[derive(Clone, Copy, Debug)]
struct PairwiseSummary {
    exposure_scale: f32,
    mean_delta_e_2000: f64,
    p95_delta_e_2000: f64,
    max_delta_e_2000: f64,
    mean_delta_e_ok: f64,
    mean_hue_error_degrees: f64,
    mean_chroma_drift: f64,
    mean_lightness_drift: f64,
    out_of_range_patch_mean_fraction: f64,
}

fn rendered_likeness(
    measured: &[[f32; 3]],
    camera_jpeg: &[[f32; 3]],
) -> Result<PairwiseSummary, ReferenceError> {
    if measured.len() != 24 || camera_jpeg.len() != 24 {
        return Err(ReferenceError::PatchCount {
            expected: 24,
            actual: measured.len().min(camera_jpeg.len()),
        });
    }
    if measured
        .iter()
        .chain(camera_jpeg)
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(ReferenceError::NonFinite);
    }

    let luma = |rgb: &[f32; 3]| {
        0.212_672_9 * rgb[0] as f64 + 0.715_152_2 * rgb[1] as f64 + 0.072_175 * rgb[2] as f64
    };
    // Fit one scalar from the four mid-neutral patches. White and black are
    // excluded because picture-style clipping/toe would corrupt the fit.
    let mut ratios = measured[19..23]
        .iter()
        .zip(&camera_jpeg[19..23])
        .filter_map(|(sample, target)| {
            let measured_y = luma(sample);
            let target_y = luma(target);
            (measured_y > 1.0e-6 && target_y > 1.0e-6).then_some(target_y / measured_y)
        })
        .collect::<Vec<_>>();
    if ratios.is_empty() {
        return Err(ReferenceError::NonFinite);
    }
    ratios.sort_by(f64::total_cmp);
    let middle = ratios.len() / 2;
    let exposure_scale = if ratios.len() % 2 == 0 {
        ((ratios[middle - 1] + ratios[middle]) * 0.5) as f32
    } else {
        ratios[middle] as f32
    };

    let mut delta_e = Vec::with_capacity(24);
    let mut delta_ok = Vec::with_capacity(24);
    let mut hue_errors = Vec::with_capacity(18);
    let mut chroma_drift = 0.0;
    let mut lightness_drift = 0.0;
    let mut out_of_range = 0usize;
    for (index, (sample, target)) in measured.iter().zip(camera_jpeg).enumerate() {
        let sample = sample.map(|value| value * exposure_scale);
        if sample.iter().any(|value| !(0.0..=1.0).contains(value)) {
            out_of_range += 1;
        }
        let sample_lab = linear_srgb_to_lab_d50(sample);
        let target_lab = linear_srgb_to_lab_d50(*target);
        delta_e.push(delta_e_2000(sample_lab, target_lab));
        let sample_ok = linear_srgb_to_oklab(sample);
        let target_ok = linear_srgb_to_oklab(*target);
        delta_ok.push(
            ((sample_ok.l - target_ok.l).powi(2)
                + (sample_ok.a - target_ok.a).powi(2)
                + (sample_ok.b - target_ok.b).powi(2))
            .sqrt() as f64,
        );
        if index < 18 {
            let gap = (sample_lab.hue_degrees() - target_lab.hue_degrees()).abs();
            hue_errors.push(gap.min(360.0 - gap));
        }
        chroma_drift += sample_lab.chroma() - target_lab.chroma();
        lightness_drift += sample_lab.l - target_lab.l;
    }
    delta_e.sort_by(f64::total_cmp);
    delta_ok.sort_by(f64::total_cmp);
    Ok(PairwiseSummary {
        exposure_scale,
        mean_delta_e_2000: mean(delta_e.iter().copied()),
        p95_delta_e_2000: delta_e[22],
        max_delta_e_2000: delta_e[23],
        mean_delta_e_ok: mean(delta_ok.iter().copied()),
        mean_hue_error_degrees: mean(hue_errors.into_iter()),
        mean_chroma_drift: chroma_drift / 24.0,
        mean_lightness_drift: lightness_drift / 24.0,
        out_of_range_patch_mean_fraction: out_of_range as f64 / 24.0,
    })
}

fn summary_row(
    camera: &str,
    kind: &str,
    summary: &ColorCheckerSummary,
    source_clip_max: f64,
    quality_included: bool,
) -> String {
    format!(
        "{camera},{kind},{:.6},{:.4},{:.4},{:.4},{:.6},{:.6},{:.3},{:.3},{:.3},{:.4},{source_clip_max:.6},{quality_included}",
        summary.exposure_scale,
        summary.mean_delta_e_2000,
        summary.p95_delta_e_2000,
        summary.max_delta_e_2000,
        summary.mean_delta_e_ok,
        summary.p95_delta_e_ok,
        summary.mean_hue_error_degrees,
        summary.mean_chroma_drift,
        summary.mean_lightness_drift,
        summary.out_of_range_patch_mean_fraction,
    )
}

fn pairwise_row(camera: &str, summary: PairwiseSummary) -> String {
    format!(
        "{camera},iai_tone_vs_camera_jpeg,{:.6},{:.4},{:.4},{:.4},{:.6},NA,{:.3},{:.3},{:.3},{:.4},NA,NA",
        summary.exposure_scale,
        summary.mean_delta_e_2000,
        summary.p95_delta_e_2000,
        summary.max_delta_e_2000,
        summary.mean_delta_e_ok,
        summary.mean_hue_error_degrees,
        summary.mean_chroma_drift,
        summary.mean_lightness_drift,
        summary.out_of_range_patch_mean_fraction,
    )
}

fn append_patch_rows(
    output: &mut String,
    camera: &str,
    scoreboard: &str,
    summary: &ColorCheckerSummary,
) {
    for (index, patch) in summary.patches.iter().enumerate() {
        writeln!(
            output,
            "{camera},{scoreboard},{},{},{:.4},{:.6},{:.3},{:.3},{:.3}",
            index + 1,
            patch.name,
            patch.delta_e_2000,
            patch.delta_e_ok,
            patch.hue_error_degrees,
            patch.chroma_drift,
            patch.lightness_drift,
        )
        .unwrap();
    }
}

#[test]
#[ignore = "requires the external Middlebury checker24s corpus"]
fn checker24s_measured_baseline() {
    let root = reference_root()
        .expect("set IAI_COLOR_REFERENCE_DIR to the extracted Middlebury checker24s directory");
    assert!(root.is_dir(), "{} is not a directory", root.display());
    let archive = reference_archive(&root).expect(
        "set IAI_COLOR_REFERENCE_ARCHIVE or retain checker24s-RAW-JPG.zip beside the extracted corpus",
    );
    assert_sha256(&archive, EXPECTED_ARCHIVE_SHA256);
    assert_extracted_tree_sha256(&root, EXPECTED_EXTRACTED_TREE_SHA256);
    let mut cameras = std::fs::read_dir(&root)
        .expect("reference root must be readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    cameras.sort();

    let all_pngs = walk_files(&root)
        .into_iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("png"))
        .collect::<Vec<_>>();
    let raw_count = all_pngs
        .iter()
        .filter(|path| {
            path.file_stem()
                .is_some_and(|value| value.to_string_lossy().ends_with("-raw"))
        })
        .count();
    let jpg_count = all_pngs
        .iter()
        .filter(|path| {
            path.file_stem()
                .is_some_and(|value| value.to_string_lossy().ends_with("-jpg"))
        })
        .count();
    assert_eq!((raw_count, jpg_count), (240, 240));
    assert_eq!(cameras.len(), 24);

    let mut csv = String::from(
        "camera,scoreboard,exposure_scale,mean_de00,p95_de00,max_de00,mean_deok,p95_deok,mean_hue_error_deg,mean_chroma_drift,mean_lightness_drift,out_of_range_patch_mean_fraction,source_chromatic_clip_max,quality_included\n",
    );
    let mut patch_csv = String::from(
        "camera,scoreboard,patch_id,patch_name,de00,deok,hue_error_deg,chroma_drift,lightness_drift\n",
    );
    let mut raw_reports = Vec::new();
    let mut jpeg_reports = Vec::new();
    let mut tone_reports = Vec::new();
    let mut quality_raw_reports = Vec::new();
    let mut quality_tone_reports = Vec::new();
    let mut likeness_reports = Vec::new();
    let mut raw_offenders = Vec::new();
    for camera_dir in &cameras {
        // wb1 = fixed tungsten WB, i1 = 3200 K illuminant, e3 = nominal 0 EV.
        let raw_path = camera_dir.join("wb1i1e3-raw.png");
        let jpeg_path = camera_dir.join("wb1i1e3-jpg.png");
        let raw = trimmed_patch_means(&raw_path, false).unwrap();
        let jpeg = trimmed_patch_means(&jpeg_path, true).unwrap();
        let raw_clip_max = raw
            .chromatic_channel_clip_fraction
            .into_iter()
            .fold(0.0, f64::max);
        let jpeg_clip_max = jpeg
            .chromatic_channel_clip_fraction
            .into_iter()
            .fold(0.0, f64::max);
        let quality_included = raw_clip_max <= 0.01;

        let raw_report = evaluate_colorchecker_linear(&raw.means, true).unwrap();
        let jpeg_report = evaluate_colorchecker_linear(&jpeg.means, true).unwrap();
        let tone = render_current_tone(&raw.means, raw_report.exposure_scale).unwrap();
        let tone_report = evaluate_colorchecker_linear(&tone, false).unwrap();
        let likeness = rendered_likeness(&tone, &jpeg.means).unwrap();
        let camera = camera_dir.file_name().unwrap().to_string_lossy();

        writeln!(
            csv,
            "{}",
            summary_row(
                &camera,
                "d50_reference_dcraw_linear",
                &raw_report,
                raw_clip_max,
                quality_included,
            )
        )
        .unwrap();
        writeln!(
            csv,
            "{}",
            summary_row(
                &camera,
                "camera_jpeg_vs_d50_observation",
                &jpeg_report,
                jpeg_clip_max,
                false,
            )
        )
        .unwrap();
        writeln!(
            csv,
            "{}",
            summary_row(
                &camera,
                "iai_current_tone_vs_d50",
                &tone_report,
                0.0,
                quality_included,
            )
        )
        .unwrap();
        writeln!(csv, "{}", pairwise_row(&camera, likeness)).unwrap();

        append_patch_rows(
            &mut patch_csv,
            &camera,
            "d50_reference_dcraw_linear",
            &raw_report,
        );
        append_patch_rows(
            &mut patch_csv,
            &camera,
            "camera_jpeg_vs_d50_observation",
            &jpeg_report,
        );
        append_patch_rows(
            &mut patch_csv,
            &camera,
            "iai_current_tone_vs_d50",
            &tone_report,
        );
        for (index, patch) in raw_report.patches.iter().enumerate() {
            raw_offenders.push((
                patch.delta_e_2000,
                camera.to_string(),
                index + 1,
                patch.name,
            ));
        }
        if quality_included {
            quality_raw_reports.push(raw_report.clone());
            quality_tone_reports.push(tone_report.clone());
        }
        raw_reports.push(raw_report);
        jpeg_reports.push(jpeg_report);
        tone_reports.push(tone_report);
        likeness_reports.push(likeness);
    }

    let raw_mean_de = mean(raw_reports.iter().map(|report| report.mean_delta_e_2000));
    let (raw_global_p95_de, raw_global_max_de) = global_delta_e_2000(&raw_reports);
    let raw_mean_hue = mean(
        raw_reports
            .iter()
            .map(|report| report.mean_hue_error_degrees),
    );
    let jpeg_mean_de = mean(jpeg_reports.iter().map(|report| report.mean_delta_e_2000));
    let (jpeg_global_p95_de, jpeg_global_max_de) = global_delta_e_2000(&jpeg_reports);
    let tone_mean_de = mean(tone_reports.iter().map(|report| report.mean_delta_e_2000));
    let (tone_global_p95_de, tone_global_max_de) = global_delta_e_2000(&tone_reports);
    assert!(
        !quality_raw_reports.is_empty(),
        "all selected RAW observations are clipped"
    );
    let quality_raw_mean_de = mean(
        quality_raw_reports
            .iter()
            .map(|report| report.mean_delta_e_2000),
    );
    let quality_tone_mean_de = mean(
        quality_tone_reports
            .iter()
            .map(|report| report.mean_delta_e_2000),
    );
    let likeness_mean_de = mean(
        likeness_reports
            .iter()
            .map(|report| report.mean_delta_e_2000),
    );
    print!("{csv}");
    println!(
        "aggregate,d50_reference_dcraw_linear,mean_de00={raw_mean_de:.4},p95_de00={raw_global_p95_de:.4},max_de00={raw_global_max_de:.4},mean_hue={raw_mean_hue:.3}"
    );
    println!(
        "aggregate,d50_reference_dcraw_linear_unclipped,images={},mean_de00={quality_raw_mean_de:.4}",
        quality_raw_reports.len()
    );
    println!(
        "aggregate,camera_jpeg_vs_d50_observation,mean_de00={jpeg_mean_de:.4},p95_de00={jpeg_global_p95_de:.4},max_de00={jpeg_global_max_de:.4}"
    );
    println!(
        "aggregate,iai_current_tone_vs_d50,mean_de00={tone_mean_de:.4},p95_de00={tone_global_p95_de:.4},max_de00={tone_global_max_de:.4}"
    );
    println!(
        "aggregate,iai_current_tone_vs_d50_unclipped,images={},mean_de00={quality_tone_mean_de:.4}",
        quality_tone_reports.len()
    );
    println!("aggregate,iai_tone_vs_camera_jpeg,mean_de00={likeness_mean_de:.4}");
    raw_offenders.sort_by(|first, second| second.0.total_cmp(&first.0));
    for (delta, camera, patch_id, patch_name) in raw_offenders.iter().take(8) {
        println!(
            "offender,d50_reference_dcraw_linear,{camera},patch={patch_id}:{patch_name},de00={delta:.4}"
        );
    }

    if let Some(output_dir) = std::env::var_os("IAI_COLOR_REFERENCE_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&output_dir).expect("create reference report directory");
        std::fs::write(output_dir.join("checker24s_summary.csv"), &csv)
            .expect("write checker24s summary CSV");
        std::fs::write(output_dir.join("checker24s_per_patch.csv"), &patch_csv)
            .expect("write checker24s per-patch CSV");
        let aggregate = serde_json::json!({
            "dataset": "Middlebury checker24s-RAW-JPG",
            "selection": "wb1i1e3",
            "camera_count": cameras.len(),
            "d50_reference_dcraw_linear": {
                "mean_de00": raw_mean_de,
                "global_p95_de00": raw_global_p95_de,
                "global_max_de00": raw_global_max_de,
                "mean_hue_error_deg": raw_mean_hue,
            },
            "d50_reference_dcraw_linear_unclipped": {
                "image_count": quality_raw_reports.len(),
                "mean_de00": quality_raw_mean_de,
            },
            "camera_jpeg_vs_d50_observation": {
                "mean_de00": jpeg_mean_de,
                "global_p95_de00": jpeg_global_p95_de,
                "global_max_de00": jpeg_global_max_de,
            },
            "iai_current_tone_vs_d50": {
                "mean_de00": tone_mean_de,
                "global_p95_de00": tone_global_p95_de,
                "global_max_de00": tone_global_max_de,
            },
            "iai_current_tone_vs_d50_unclipped": {
                "image_count": quality_tone_reports.len(),
                "mean_de00": quality_tone_mean_de,
            },
            "iai_tone_vs_camera_jpeg": { "mean_de00": likeness_mean_de },
            "caveat": "dcraw-rendered RGB, not sensor RAW; this is not RawImporter accuracy"
        });
        std::fs::write(
            output_dir.join("checker24s_aggregate.json"),
            serde_json::to_vec_pretty(&aggregate).unwrap(),
        )
        .expect("write checker24s aggregate JSON");
    }

    // Golden aggregate checks catch transfer/grid/metric drift. The documented
    // Archive identity plus aggregate checks catch corpus, transfer, grid, and
    // metric drift. These checks intentionally do not bless current quality.
    assert!(
        (raw_mean_de - 7.4012).abs() <= 0.05,
        "raw mean delta-E00 drifted: {raw_mean_de}"
    );
    assert!(
        (raw_mean_hue - 13.837).abs() <= 0.10,
        "raw hue baseline drifted: {raw_mean_hue}"
    );
    assert!((raw_global_p95_de - 17.3054).abs() <= 0.05);
    assert!((raw_global_max_de - 28.8467).abs() <= 0.05);
    assert!(
        (jpeg_mean_de - 6.6901).abs() <= 0.05,
        "JPEG-vs-D50 observation drifted: {jpeg_mean_de}"
    );
    assert!((jpeg_global_p95_de - 14.4505).abs() <= 0.05);
    assert!(
        (tone_mean_de - 7.5724).abs() <= 0.05,
        "current tone baseline drifted: {tone_mean_de}"
    );
    assert!((tone_global_p95_de - 16.5952).abs() <= 0.05);
    assert_eq!(quality_raw_reports.len(), 6);
    assert!(
        (quality_raw_mean_de - 6.9388).abs() <= 0.05,
        "unclipped raw baseline drifted: {quality_raw_mean_de}"
    );
    assert!(
        (quality_tone_mean_de - 7.0843).abs() <= 0.05,
        "unclipped tone baseline drifted: {quality_tone_mean_de}"
    );
    assert!(
        (likeness_mean_de - 5.8194).abs() <= 0.05,
        "tone/JPEG likeness baseline drifted: {likeness_mean_de}"
    );
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).expect("reference directory must be readable") {
            let path = entry.expect("reference entry must be readable").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}
