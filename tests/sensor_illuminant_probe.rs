//! Phase-0 sensor-RAW / known-illuminant probe.
//!
//! Cube++ image `20_2660.CR2` is a real Canon EOS 550D sensor capture with two
//! measured SpyderCube neutral faces. This probe validates sensor decoding and
//! camera-space illuminant recovery; it is not a ColorChecker or camera-to-XYZ
//! accuracy test.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use iai::core::develop_scene::render_default_look;
use iai::formats::raw::RawImporter;
use iai::formats::Importer;
use rawloader::{RawImage, RawImageData};

const REFERENCE_SIZE: [f64; 2] = [2592.0, 1728.0];
const LEFT_TRIANGLE: [[f64; 2]; 3] = [[2128.0, 1366.0], [2327.0, 1219.0], [2332.0, 1411.0]];
const RIGHT_TRIANGLE: [[f64; 2]; 3] = [[2525.0, 1355.0], [2327.0, 1220.0], [2331.0, 1412.0]];
const LEFT_GT: [f64; 3] = [
    0.198_907_375_217_283_36,
    0.469_828_656_568_164_86,
    0.331_263_968_214_551_8,
];
const RIGHT_GT: [f64; 3] = [
    0.204_070_330_887_442_9,
    0.472_103_004_291_845_5,
    0.323_826_664_820_711_6,
];
const MEAN_GT: [f64; 3] = [
    0.201_489_986_414_446,
    0.470_966_329_690_436_3,
    0.327_543_683_895_117_8,
];
const EXPECTED_BYTES: u64 = 24_867_695;
const EXPECTED_SHA256: &str = "0d5de5728cac4855572acc46b47b07c2598cab5df79b19207713f09ba23c2bdd";
const REGISTERED_SIZE: [usize; 2] = [5184, 3456];

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

fn fixture_path() -> Option<PathBuf> {
    std::env::var_os("IAI_CUBEPP_REFERENCE_DIR")
        .map(PathBuf::from)
        .map(|directory| directory.join("20_2660.CR2"))
        .or_else(|| {
            let default = PathBuf::from(
                "target/color-reference-cache/cubepp-raw-fixture/Cube++/auxiliary/source/CR2/20_2660.CR2",
            );
            default.is_file().then_some(default)
        })
}

fn raw_value(data: &RawImageData, index: usize) -> f64 {
    match data {
        RawImageData::Integer(values) => values[index] as f64,
        RawImageData::Float(values) => values[index] as f64,
    }
}

fn active_area(raw: &RawImage) -> (usize, usize, usize, usize) {
    let top = raw.crops[0];
    let left = raw.crops[3];
    let width = raw.width - raw.crops[3] - raw.crops[1];
    let height = raw.height - raw.crops[0] - raw.crops[2];
    (top, left, width, height)
}

fn registered_area(raw: &RawImage) -> (usize, usize, usize, usize) {
    let (top, left, width, height) = active_area(raw);
    assert!(width >= REGISTERED_SIZE[0] && height >= REGISTERED_SIZE[1]);
    // Cube++'s 2592x1728 reference is the default-camera rectangle after the
    // documented few-pixel crop. Register that rectangle within decoder-active
    // coordinates instead of assuming every decoder exposes an exact 2x size.
    (
        top + (height - REGISTERED_SIZE[1]) / 2,
        left + (width - REGISTERED_SIZE[0]) / 2,
        REGISTERED_SIZE[0],
        REGISTERED_SIZE[1],
    )
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

fn sample_camera_triangle(raw: &RawImage, triangle: [[f64; 2]; 3]) -> [f64; 3] {
    assert_eq!(raw.cpp, 1, "fixture must expose a Bayer mosaic");
    assert!(raw.cfa.is_valid(), "fixture must have a valid CFA");
    let (top, left, width, height) = registered_area(raw);
    let min_x = triangle
        .iter()
        .map(|point| point[0])
        .fold(f64::INFINITY, f64::min);
    let max_x = triangle
        .iter()
        .map(|point| point[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = triangle
        .iter()
        .map(|point| point[1])
        .fold(f64::INFINITY, f64::min);
    let max_y = triangle
        .iter()
        .map(|point| point[1])
        .fold(f64::NEG_INFINITY, f64::max);
    let sensor_x0 = left + (min_x * width as f64 / REFERENCE_SIZE[0]).floor() as usize;
    let sensor_x1 = left + (max_x * width as f64 / REFERENCE_SIZE[0]).ceil() as usize;
    let sensor_y0 = top + (min_y * height as f64 / REFERENCE_SIZE[1]).floor() as usize;
    let sensor_y1 = top + (max_y * height as f64 / REFERENCE_SIZE[1]).ceil() as usize;
    let mut samples = [Vec::new(), Vec::new(), Vec::new()];
    for row in sensor_y0..sensor_y1.min(top + height) {
        for column in sensor_x0..sensor_x1.min(left + width) {
            let reference = [
                (column - left) as f64 * REFERENCE_SIZE[0] / width as f64,
                (row - top) as f64 * REFERENCE_SIZE[1] / height as f64,
            ];
            if !point_in_triangle(reference, triangle) {
                continue;
            }
            let mut channel = raw.cfa.color_at(row, column);
            if channel == 3 {
                channel = 1;
            }
            if channel > 2 {
                continue;
            }
            let encoded = raw_value(&raw.data, row * raw.width + column);
            if encoded >= raw.whitelevels[channel] as f64 - 2.0 {
                continue;
            }
            let black_subtracted = encoded - raw.blacklevels[channel] as f64;
            if black_subtracted > 0.0 {
                samples[channel].push(black_subtracted);
            }
        }
    }
    assert!(samples.iter().all(|values| values.len() > 100));
    normalize_sum(samples.each_mut().map(|values| trimmed_mean(values)))
}

fn sample_scene_triangle(
    scene: &iai::core::develop_scene::SceneSource,
    triangle: [[f64; 2]; 3],
) -> [f64; 3] {
    assert!(
        scene.width as usize >= REGISTERED_SIZE[0] && scene.height as usize >= REGISTERED_SIZE[1]
    );
    let registered_left = (scene.width as usize - REGISTERED_SIZE[0]) / 2;
    let registered_top = (scene.height as usize - REGISTERED_SIZE[1]) / 2;
    let min_x = triangle
        .iter()
        .map(|point| point[0])
        .fold(f64::INFINITY, f64::min);
    let max_x = triangle
        .iter()
        .map(|point| point[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = triangle
        .iter()
        .map(|point| point[1])
        .fold(f64::INFINITY, f64::min);
    let max_y = triangle
        .iter()
        .map(|point| point[1])
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

fn srgb_to_linear(value: f32) -> f64 {
    if value <= 0.040_45 {
        (value / 12.92) as f64
    } else {
        (((value + 0.055) / 1.055).powf(2.4)) as f64
    }
}

fn sample_registered_encoded(
    width: usize,
    height: usize,
    triangle: [[f64; 2]; 3],
    mut pixel: impl FnMut(usize, usize) -> [f32; 3],
) -> [f64; 3] {
    assert!(width >= REGISTERED_SIZE[0] && height >= REGISTERED_SIZE[1]);
    let registered_left = (width - REGISTERED_SIZE[0]) / 2;
    let registered_top = (height - REGISTERED_SIZE[1]) / 2;
    let min_x = triangle
        .iter()
        .map(|point| point[0])
        .fold(f64::INFINITY, f64::min);
    let max_x = triangle
        .iter()
        .map(|point| point[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = triangle
        .iter()
        .map(|point| point[1])
        .fold(f64::INFINITY, f64::min);
    let max_y = triangle
        .iter()
        .map(|point| point[1])
        .fold(f64::NEG_INFINITY, f64::max);
    let x0 =
        registered_left + (min_x * REGISTERED_SIZE[0] as f64 / REFERENCE_SIZE[0]).floor() as usize;
    let x1 =
        registered_left + (max_x * REGISTERED_SIZE[0] as f64 / REFERENCE_SIZE[0]).ceil() as usize;
    let y0 =
        registered_top + (min_y * REGISTERED_SIZE[1] as f64 / REFERENCE_SIZE[1]).floor() as usize;
    let y1 =
        registered_top + (max_y * REGISTERED_SIZE[1] as f64 / REFERENCE_SIZE[1]).ceil() as usize;
    let mut samples = [Vec::new(), Vec::new(), Vec::new()];
    for y in y0..y1.min(height) {
        for x in x0..x1.min(width) {
            let reference = [
                (x - registered_left) as f64 * REFERENCE_SIZE[0] / REGISTERED_SIZE[0] as f64,
                (y - registered_top) as f64 * REFERENCE_SIZE[1] / REGISTERED_SIZE[1] as f64,
            ];
            if !point_in_triangle(reference, triangle) {
                continue;
            }
            let rgb = pixel(x, y);
            for channel in 0..3 {
                let value = srgb_to_linear(rgb[channel]);
                if value.is_finite() && value > 0.0 {
                    samples[channel].push(value);
                }
            }
        }
    }
    assert!(samples.iter().all(|values| values.len() > 100));
    normalize_sum(samples.each_mut().map(|values| trimmed_mean(values)))
}

#[test]
#[ignore = "requires external Cube++ 20_2660.CR2 sensor fixture"]
fn cubepp_sensor_illuminant_baseline() {
    let path = fixture_path().expect("set IAI_CUBEPP_REFERENCE_DIR or populate the target cache");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), EXPECTED_BYTES);
    assert_sha256(&path, EXPECTED_SHA256);
    let raw = rawloader::decode_file(&path).expect("decode Cube++ CR2 with rawloader");
    let (top, left, active_width, active_height) = active_area(&raw);
    assert_eq!((active_width, active_height), (5196, 3462));

    let left_illuminant = sample_camera_triangle(&raw, LEFT_TRIANGLE);
    let right_illuminant = sample_camera_triangle(&raw, RIGHT_TRIANGLE);
    let measured_mean = normalize_sum([
        (left_illuminant[0] + right_illuminant[0]) * 0.5,
        (left_illuminant[1] + right_illuminant[1]) * 0.5,
        (left_illuminant[2] + right_illuminant[2]) * 0.5,
    ]);
    let left_error = angular_error_degrees(left_illuminant, LEFT_GT);
    let right_error = angular_error_degrees(right_illuminant, RIGHT_GT);
    let mean_error = angular_error_degrees(measured_mean, MEAN_GT);

    let gains = raw.wb_coeffs;
    let embedded_illuminant = normalize_sum([
        1.0 / gains[0] as f64,
        1.0 / gains[1] as f64,
        1.0 / gains[2] as f64,
    ]);
    let embedded_error = angular_error_degrees(embedded_illuminant, MEAN_GT);
    println!(
        "cubepp,20_2660,raw={}x{},active={}x{}+{},{} cfa={} black={:?} white={:?}",
        raw.width,
        raw.height,
        active_width,
        active_height,
        left,
        top,
        raw.cfa.name,
        raw.blacklevels,
        raw.whitelevels,
    );
    println!(
        "cubepp,camera_linear,left={left_illuminant:?},right={right_illuminant:?},mean={measured_mean:?},left_error_deg={left_error:.6},right_error_deg={right_error:.6},mean_error_deg={mean_error:.6}"
    );
    println!(
        "cubepp,embedded_as_shot,coeffs={gains:?},illuminant={embedded_illuminant:?},error_deg={embedded_error:.6}"
    );
    assert!(left_error <= 1.0, "left illuminant error {left_error}");
    assert!(right_error <= 1.0, "right illuminant error {right_error}");
    assert!(mean_error <= 1.0, "mean illuminant error {mean_error}");
    assert!((left_error - 0.0850).abs() <= 0.02);
    assert!((right_error - 0.0262).abs() <= 0.02);
    assert!((mean_error - 0.0523).abs() <= 0.02);
    assert!((embedded_error - 2.4337).abs() <= 0.05);
    drop(raw);

    std::env::set_var("IAI_RAW_JPEG_MATCH", "none");
    let canvas = RawImporter
        .import(&path)
        .expect("render Cube++ CR2 through iAi");
    std::env::remove_var("IAI_RAW_JPEG_MATCH");
    let scene = canvas
        .develop_source
        .as_ref()
        .expect("RAW must attach scene source");
    assert_eq!((scene.width, scene.height), (5196, 3462));
    let left_scene = sample_scene_triangle(scene, LEFT_TRIANGLE);
    let right_scene = sample_scene_triangle(scene, RIGHT_TRIANGLE);
    let neutral = normalize_sum([1.0, 1.0, 1.0]);
    let left_neutral_error = angular_error_degrees(left_scene, neutral);
    let right_neutral_error = angular_error_degrees(right_scene, neutral);
    println!(
        "cubepp,iai_no_jpeg_match,left={left_scene:?},right={right_scene:?},left_neutral_error_deg={left_neutral_error:.6},right_neutral_error_deg={right_neutral_error:.6}"
    );
    assert!((left_neutral_error - 2.4416).abs() <= 0.05);
    assert!((right_neutral_error - 3.0544).abs() <= 0.05);

    let encoded = render_default_look(scene);
    let iai_left = sample_registered_encoded(
        scene.width as usize,
        scene.height as usize,
        LEFT_TRIANGLE,
        |x, y| {
            let index = (y * scene.width as usize + x) * 4;
            [
                encoded[index] as f32 / 65_535.0,
                encoded[index + 1] as f32 / 65_535.0,
                encoded[index + 2] as f32 / 65_535.0,
            ]
        },
    );
    let iai_right = sample_registered_encoded(
        scene.width as usize,
        scene.height as usize,
        RIGHT_TRIANGLE,
        |x, y| {
            let index = (y * scene.width as usize + x) * 4;
            [
                encoded[index] as f32 / 65_535.0,
                encoded[index + 1] as f32 / 65_535.0,
                encoded[index + 2] as f32 / 65_535.0,
            ]
        },
    );
    let iai_left_error = angular_error_degrees(iai_left, neutral);
    let iai_right_error = angular_error_degrees(iai_right, neutral);
    println!(
        "cubepp,iai_encoded_no_jpeg_match,left={iai_left:?},right={iai_right:?},left_neutral_error_deg={iai_left_error:.6},right_neutral_error_deg={iai_right_error:.6}"
    );
    assert!((iai_left_error - 2.4583).abs() <= 0.05);
    assert!((iai_right_error - 3.0820).abs() <= 0.05);

    let art_path = std::env::var_os("IAI_ART_REFERENCE_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/phase0/art/20_2660.tif"));
    if art_path.is_file() {
        let art = image::ImageReader::open(&art_path)
            .expect("open ART reference TIFF")
            .decode()
            .expect("decode ART reference TIFF")
            .to_rgb32f();
        let (art_width, art_height) = art.dimensions();
        let art_left = sample_registered_encoded(
            art_width as usize,
            art_height as usize,
            LEFT_TRIANGLE,
            |x, y| art.get_pixel(x as u32, y as u32).0,
        );
        let art_right = sample_registered_encoded(
            art_width as usize,
            art_height as usize,
            RIGHT_TRIANGLE,
            |x, y| art.get_pixel(x as u32, y as u32).0,
        );
        let art_left_error = angular_error_degrees(art_left, neutral);
        let art_right_error = angular_error_degrees(art_right, neutral);
        println!(
            "cubepp,art_neutral_tiff,dimensions={art_width}x{art_height},left={art_left:?},right={art_right:?},left_neutral_error_deg={art_left_error:.6},right_neutral_error_deg={art_right_error:.6}"
        );
        assert!((art_left_error - 6.9981).abs() <= 0.05);
        assert!((art_right_error - 5.1385).abs() <= 0.05);
    } else {
        println!("cubepp,art_neutral_tiff,unavailable={}", art_path.display());
    }
}
