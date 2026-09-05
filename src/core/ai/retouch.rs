//! Offline AI retouch pipeline.
//!
//! The pipeline deliberately owns no editor state.  It accepts a flattened
//! straight-alpha RGBA8 image and returns another RGBA8 image, which makes it
//! safe to run off the UI thread and easy to test.  Model runners are lazy and
//! stage-local: a model is loaded, used, and dropped before the next stage.
//! When a model is missing or its adapter is unavailable, the stage uses the
//! documented CPU fallback and records a warning instead of aborting the job.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use image::{imageops, GrayImage, Rgb32FImage, RgbaImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

const DEFAULT_TILE: u32 = 512;
const DEFAULT_OVERLAP: u32 = 64;
const FACE_ALIGNMENT_SIDE: u32 = 512;
pub const MAX_UPSCALE_PIXELS: u64 = 150_000_000;
const MAX_MASK_CACHE_BYTES: usize = 384 * 1024 * 1024;
const FACE_ALIGNMENT_TARGET: [[f32; 2]; 5] = [
    [192.98138, 239.94708],
    [318.90277, 240.19360],
    [256.63416, 314.01935],
    [201.26117, 371.41043],
    [313.08905, 371.15118],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelId {
    FaceDetector,
    Bisenet,
    BodyParsing,
    Nafnet,
    Iat,
    Gfpgan,
    RealesrganGeneral,
    RealesrganRrdbX2,
    RealesrganRrdb,
}

impl ModelId {
    pub const ALL: [Self; 9] = [
        Self::FaceDetector,
        Self::Bisenet,
        Self::BodyParsing,
        Self::Nafnet,
        Self::Iat,
        Self::Gfpgan,
        Self::RealesrganGeneral,
        Self::RealesrganRrdbX2,
        Self::RealesrganRrdb,
    ];

    pub const fn directory(self) -> &'static str {
        match self {
            Self::FaceDetector => "face-detector",
            Self::Bisenet => "bisenet",
            Self::BodyParsing => "body-parsing",
            Self::Nafnet => "nafnet",
            Self::Iat => "iat",
            Self::Gfpgan => "gfpgan",
            Self::RealesrganGeneral | Self::RealesrganRrdbX2 | Self::RealesrganRrdb => "realesrgan",
        }
    }

    pub const fn default_file(self) -> &'static str {
        match self {
            Self::FaceDetector => "face_detection_yunet_2026may.onnx",
            Self::Bisenet => "bisenet_face_parsing.onnx",
            Self::BodyParsing => "selfie_multiclass_256x256.onnx",
            Self::Nafnet => "NAFNet-width32.onnx",
            Self::Iat => "IAT.onnx",
            Self::Gfpgan => "GFPGANv1.4.onnx",
            Self::RealesrganGeneral => "RealESRGANv3-general-x4v3.onnx",
            Self::RealesrganRrdbX2 => "RealESRGAN_x2plus.onnx",
            Self::RealesrganRrdb => "RealESRGAN_x4plus.onnx",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::FaceDetector => "YuNet Face Detector + 5 Landmarks",
            Self::Bisenet => "BiSeNet Face Parsing",
            Self::BodyParsing => "MediaPipe Selfie Multiclass",
            Self::Nafnet => "NAFNet width32",
            Self::Iat => "IAT Color / Exposure",
            Self::Gfpgan => "GFPGAN v1.4",
            Self::RealesrganGeneral => "Real-ESRGAN General x4v3",
            Self::RealesrganRrdbX2 => "Real-ESRGAN RRDB x2",
            Self::RealesrganRrdb => "Real-ESRGAN RRDB x4",
        }
    }

    pub const fn manifest_id(self) -> &'static str {
        match self {
            Self::FaceDetector => "face-detector",
            Self::Bisenet => "bisenet",
            Self::BodyParsing => "body-parsing",
            Self::Nafnet => "nafnet",
            Self::Iat => "iat",
            Self::Gfpgan => "gfpgan",
            Self::RealesrganGeneral => "realesrgan-general",
            Self::RealesrganRrdbX2 => "realesrgan-rrdb-x2",
            Self::RealesrganRrdb => "realesrgan-rrdb",
        }
    }

    pub const fn expected_sha256(self) -> &'static str {
        match self {
            Self::FaceDetector => {
                "ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0"
            }
            Self::Bisenet => "71b20280c2aeac6f646e85e392977869bef3c472555cb53a5573320d15ebd5e0",
            Self::BodyParsing => "d6757008a8f46b54da751d2b8bf4277d5dd5d011573f3f9c5caa29aa8bfcbffa",
            Self::Nafnet => "6b1943601008e5432d0770553e43956ca41f769e99da4f124514ffdadb0acbf3",
            Self::Iat => "90e1f7850cb363c8296418d28ee111ba3d74d2cc35335aab944c5d521302fbe2",
            Self::Gfpgan => "0dac008dbd9c025b9a0acff35e769792a7b8b7c52464d17d3981369834803087",
            Self::RealesrganGeneral => {
                "ac0920068b2c43da1944e788b1728e88b5e2b005a225e29c4c4c4e029bdbe926"
            }
            Self::RealesrganRrdbX2 => {
                "517dcc8f74067504bee3be68fdae3ca399b52870a0dec6e8de7f8a2654dfdfed"
            }
            Self::RealesrganRrdb => {
                "b644aab83761a02489ef1cfd581a4ac677ba43380adb6a8c3c0a3ad801c9f17e"
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelMetadata {
    pub id: ModelId,
    pub display_name: String,
    pub model_path: String,
    pub input_size: u32,
    pub provider: String,
    pub supports_cpu: bool,
    pub tile_size: u32,
    pub overlap: u32,
    pub version: String,
    pub source: String,
    pub license: String,
    pub required: bool,
}

impl ModelMetadata {
    pub fn for_id(id: ModelId) -> Self {
        Self {
            id,
            display_name: id.display_name().to_string(),
            model_path: format!("{}/{}", id.directory(), id.default_file()),
            input_size: match id {
                ModelId::FaceDetector => 640,
                ModelId::BodyParsing => 256,
                ModelId::Iat => 0, // dynamic H/W
                ModelId::RealesrganGeneral
                | ModelId::RealesrganRrdbX2
                | ModelId::RealesrganRrdb => 256,
                _ => 512,
            },
            provider: "ONNX Runtime Auto (DirectML GPU -> CPU)".to_string(),
            supports_cpu: true,
            tile_size: DEFAULT_TILE,
            overlap: DEFAULT_OVERLAP,
            version: "Phase2-ColorFix".to_string(),
            source: "See docs/AI_MODELS.md".to_string(),
            license: "See upstream model license".to_string(),
            required: !matches!(id, ModelId::RealesrganRrdbX2 | ModelId::RealesrganRrdb),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelManifest {
    pub id: String,
    pub file: String,
    pub sha256: String,
    pub source: String,
    pub license: String,
    pub required: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpscaleMode {
    #[default]
    Off,
    X2,
    X4,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorLook {
    /// Bright, clean and youthful while retaining believable skin.
    #[default]
    Fresh,
    Natural,
    Warm,
    Cool,
}

impl UpscaleMode {
    pub const fn factor(self) -> u32 {
        match self {
            Self::Off => 1,
            Self::X2 => 2,
            Self::X4 => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetouchConfig {
    pub overall_amount: u8,
    #[serde(default = "default_true")]
    pub enable_denoise: bool,
    pub denoise_amount: u8,
    #[serde(default = "default_true")]
    pub auto_denoise: bool,
    #[serde(default = "default_true")]
    pub enable_color: bool,
    pub color_amount: u8,
    #[serde(default)]
    pub color_look: ColorLook,
    #[serde(default = "default_true")]
    pub enable_face_restore: bool,
    pub face_restore_amount: u8,
    #[serde(default = "default_true")]
    pub enable_hair: bool,
    pub hair_detail_amount: u8,
    #[serde(default = "default_true")]
    pub enable_skin: bool,
    pub skin_amount: u8,
    #[serde(default = "default_true")]
    pub enable_eyes: bool,
    pub eyes_amount: u8,
    #[serde(default = "default_true")]
    pub enable_lips: bool,
    pub lips_amount: u8,
    #[serde(default = "default_true")]
    pub enable_clothes: bool,
    pub clothes_amount: u8,
    #[serde(default = "default_true")]
    pub prefer_gpu: bool,
    pub protect_identity: bool,
    pub upscale: UpscaleMode,
    pub preview_masks: bool,
}

impl Default for RetouchConfig {
    fn default() -> Self {
        Self {
            // Defaults are deliberately Max as requested. Independent safety
            // ceilings below still prevent wholesale model replacement.
            overall_amount: 100,
            enable_denoise: true,
            denoise_amount: 100,
            auto_denoise: true,
            enable_color: true,
            color_amount: 100,
            color_look: ColorLook::Fresh,
            enable_face_restore: true,
            face_restore_amount: 100,
            enable_hair: true,
            hair_detail_amount: 100,
            enable_skin: true,
            skin_amount: 100,
            enable_eyes: true,
            eyes_amount: 100,
            enable_lips: true,
            lips_amount: 100,
            enable_clothes: true,
            clothes_amount: 100,
            prefer_gpu: true,
            protect_identity: true,
            upscale: UpscaleMode::Off,
            preview_masks: false,
        }
    }
}

impl RetouchConfig {
    pub fn any_effect_enabled(&self) -> bool {
        self.enable_denoise
            || self.enable_color
            || self.enable_face_restore
            || self.enable_hair
            || self.enable_skin
            || self.enable_eyes
            || self.enable_lips
            || self.enable_clothes
            || self.upscale != UpscaleMode::Off
    }

    fn needs_face_masks(&self) -> bool {
        self.enable_face_restore || self.enable_skin || self.enable_eyes || self.enable_lips
    }

    fn needs_body_masks(&self) -> bool {
        self.enable_hair || self.enable_skin || self.enable_clothes || self.preview_masks
    }
}

const fn default_true() -> bool {
    true
}

pub fn estimated_cpu_seconds(width: u32, height: u32) -> u64 {
    let pixels = width as u64 * height as u64;
    // About 35 seconds of model/session overhead plus roughly 20 seconds per
    // megapixel on the CPU benchmark machine used for the release audit.
    35u64.saturating_add(pixels.saturating_mul(20) / 1_000_000)
}

pub fn upscale_within_budget(width: u32, height: u32, factor: u32) -> bool {
    (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(factor as u64)
        .saturating_mul(factor as u64)
        <= MAX_UPSCALE_PIXELS
}

#[derive(Clone, Debug, PartialEq)]
pub enum RetouchStage {
    Mask,
    Denoise,
    ColorExposure,
    FaceRestore,
    SelectiveDetail,
    Composite,
    Upscale,
    Done,
}

impl RetouchStage {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Mask => "Face Parsing / Mask",
            Self::Denoise => "Denoise",
            Self::ColorExposure => "Color / Exposure",
            Self::FaceRestore => "Face Restore",
            Self::SelectiveDetail => "Selective Detail",
            Self::Composite => "Composite",
            Self::Upscale => "Optional Upscale",
            Self::Done => "Done",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetouchProgress {
    pub stage: RetouchStage,
    pub fraction: f32,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct StageTiming {
    pub stage: String,
    pub millis: u128,
}

#[derive(Clone, Debug, Default)]
pub struct RetouchBenchmark {
    pub timings: Vec<StageTiming>,
    pub total_millis: u128,
    pub output_width: u32,
    pub output_height: u32,
    pub provider: String,
    pub peak_memory_bytes: usize,
    pub changed_pixels: usize,
    pub mean_absolute_delta: f32,
    pub face_changed_pixels: usize,
    pub face_mean_absolute_delta: f32,
    pub estimated_noise_sigma: f32,
    pub effective_denoise_amount: f32,
    pub detected_color_cast: String,
    pub white_balance_gains: [f32; 3],
    pub color_cast_confidence: f32,
}

#[derive(Clone, Debug)]
pub struct RetouchResult {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub face_mask: Vec<f32>,
    /// Optional colour overlay at the source resolution. It is returned only
    /// when Preview Masks was enabled and is placed as a separate toggleable
    /// layer by the editor.
    pub mask_preview_rgba: Option<Vec<u8>>,
    pub benchmark: RetouchBenchmark,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RetouchStatus {
    pub running: bool,
    pub progress: RetouchProgress,
    pub error: Option<String>,
    pub last_benchmark: Option<RetouchBenchmark>,
}

impl Default for RetouchStatus {
    fn default() -> Self {
        Self {
            running: false,
            progress: RetouchProgress {
                stage: RetouchStage::Done,
                fraction: 0.0,
                message: "Sẵn sàng".to_string(),
            },
            error: None,
            last_benchmark: None,
        }
    }
}

pub trait IModelRunner: Send {
    fn metadata(&self) -> &ModelMetadata;
    fn available(&self) -> bool;
    fn run_image(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String>;
}

#[derive(Clone, Debug)]
struct FaceDetection {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    landmarks: [[f32; 2]; 5],
    score: f32,
}

#[derive(Clone, Debug)]
struct AlignedFaceMasks {
    transform: SimilarityTransform,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    face: Vec<u8>,
    skin: Vec<u8>,
    hair: Vec<u8>,
    eyes_and_brows: Vec<u8>,
    lips: Vec<u8>,
    clothes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct SemanticMasks {
    face: Vec<f32>,
    skin: Vec<f32>,
    hair: Vec<f32>,
    eyes_and_brows: Vec<f32>,
    lips: Vec<f32>,
    clothes: Vec<f32>,
    background: Vec<f32>,
    accessories: Vec<f32>,
    aligned_faces: Vec<AlignedFaceMasks>,
    face_count: usize,
    ai_generated: bool,
}

impl SemanticMasks {
    fn empty(width: u32, height: u32) -> Self {
        let count = width as usize * height as usize;
        Self {
            face: vec![0.0; count],
            skin: vec![0.0; count],
            hair: vec![0.0; count],
            eyes_and_brows: vec![0.0; count],
            lips: vec![0.0; count],
            clothes: vec![0.0; count],
            background: vec![0.0; count],
            accessories: vec![0.0; count],
            aligned_faces: Vec::new(),
            face_count: 0,
            ai_generated: false,
        }
    }

    fn merge_max(&mut self, other: Self) {
        for (target, source) in [
            (&mut self.face, other.face),
            (&mut self.skin, other.skin),
            (&mut self.hair, other.hair),
            (&mut self.eyes_and_brows, other.eyes_and_brows),
            (&mut self.lips, other.lips),
            (&mut self.clothes, other.clothes),
            (&mut self.background, other.background),
            (&mut self.accessories, other.accessories),
        ] {
            for (target, source) in target.iter_mut().zip(source) {
                *target = target.max(source);
            }
        }
        self.face_count = self.face_count.max(other.face_count);
        self.aligned_faces.extend(other.aligned_faces);
        self.ai_generated |= other.ai_generated;
    }

    fn cpu_fallback(rgba: &[u8], width: u32, height: u32) -> Self {
        let face = cpu_face_mask(rgba, width, height);
        let skin = skin_mask(rgba, &face);
        let (eyes_and_brows, lips) = feature_masks(&face, width, height);
        let count = width as usize * height as usize;
        Self {
            face,
            skin,
            hair: vec![0.0; count],
            eyes_and_brows,
            lips,
            clothes: vec![0.0; count],
            background: vec![0.0; count],
            accessories: vec![0.0; count],
            aligned_faces: Vec::new(),
            face_count: 0,
            ai_generated: false,
        }
    }

    fn approximate_bytes(&self) -> usize {
        let float_planes = self.face.len()
            + self.skin.len()
            + self.hair.len()
            + self.eyes_and_brows.len()
            + self.lips.len()
            + self.clothes.len()
            + self.background.len()
            + self.accessories.len();
        let aligned_planes = self
            .aligned_faces
            .iter()
            .map(|face| {
                face.face.len()
                    + face.skin.len()
                    + face.hair.len()
                    + face.eyes_and_brows.len()
                    + face.lips.len()
                    + face.clothes.len()
            })
            .sum::<usize>();
        float_planes
            .saturating_mul(std::mem::size_of::<f32>())
            .saturating_add(aligned_planes)
    }
}

impl AlignedFaceMasks {
    fn union_into(&self, target: &mut SemanticMasks, canvas_width: u32) {
        for local_y in 0..self.height {
            for local_x in 0..self.width {
                let local = (local_y * self.width + local_x) as usize;
                let global = ((self.y + local_y) * canvas_width + self.x + local_x) as usize;
                let decode = |values: &[u8]| values[local] as f32 / 255.0;
                target.face[global] = target.face[global].max(decode(&self.face));
                target.skin[global] = target.skin[global].max(decode(&self.skin));
                target.hair[global] = target.hair[global].max(decode(&self.hair));
                target.eyes_and_brows[global] =
                    target.eyes_and_brows[global].max(decode(&self.eyes_and_brows));
                target.lips[global] = target.lips[global].max(decode(&self.lips));
                target.clothes[global] = target.clothes[global].max(decode(&self.clothes));
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SimilarityTransform {
    a: f32,
    b: f32,
    tx: f32,
    ty: f32,
}

impl SimilarityTransform {
    fn fit(source: &[[f32; 2]; 5], target: &[[f32; 2]; 5]) -> Result<Self, String> {
        let source_mean = source.iter().fold([0.0f32; 2], |mut sum, point| {
            sum[0] += point[0] / 5.0;
            sum[1] += point[1] / 5.0;
            sum
        });
        let target_mean = target.iter().fold([0.0f32; 2], |mut sum, point| {
            sum[0] += point[0] / 5.0;
            sum[1] += point[1] / 5.0;
            sum
        });
        let mut denominator = 0.0f32;
        let mut real = 0.0f32;
        let mut imaginary = 0.0f32;
        for (source, target) in source.iter().zip(target.iter()) {
            let sx = source[0] - source_mean[0];
            let sy = source[1] - source_mean[1];
            let tx = target[0] - target_mean[0];
            let ty = target[1] - target_mean[1];
            denominator += sx * sx + sy * sy;
            real += sx * tx + sy * ty;
            imaginary += sx * ty - sy * tx;
        }
        if denominator <= f32::EPSILON {
            return Err("Face alignment landmarks are degenerate".to_string());
        }
        let a = real / denominator;
        let b = imaginary / denominator;
        if a * a + b * b <= f32::EPSILON {
            return Err("Face alignment transform is singular".to_string());
        }
        Ok(Self {
            a,
            b,
            tx: target_mean[0] - a * source_mean[0] + b * source_mean[1],
            ty: target_mean[1] - b * source_mean[0] - a * source_mean[1],
        })
    }

    fn source_to_target(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x - self.b * y + self.tx,
            self.b * x + self.a * y + self.ty,
        )
    }

    fn target_to_source(self, x: f32, y: f32) -> (f32, f32) {
        let determinant = self.a * self.a + self.b * self.b;
        let x = x - self.tx;
        let y = y - self.ty;
        (
            (self.a * x + self.b * y) / determinant,
            (-self.b * x + self.a * y) / determinant,
        )
    }
}

fn bilinear_rgba(rgba: &[u8], width: u32, height: u32, x: f32, y: f32) -> [u8; 4] {
    if x < 0.0 || y < 0.0 || x > width as f32 - 1.0 || y > height as f32 - 1.0 {
        return [0, 0, 0, 255];
    }
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let mut result = [0u8; 4];
    for channel in 0..4 {
        let sample = |sample_x: u32, sample_y: u32| {
            rgba[(sample_y as usize * width as usize + sample_x as usize) * 4 + channel] as f32
        };
        let top = sample(x0, y0) * (1.0 - fx) + sample(x1, y0) * fx;
        let bottom = sample(x0, y1) * (1.0 - fx) + sample(x1, y1) * fx;
        result[channel] = (top * (1.0 - fy) + bottom * fy).clamp(0.0, 255.0).round() as u8;
    }
    result
}

fn aligned_face_crop(
    rgba: &[u8],
    width: u32,
    height: u32,
    transform: SimilarityTransform,
) -> Vec<u8> {
    let mut crop = vec![0u8; (FACE_ALIGNMENT_SIDE * FACE_ALIGNMENT_SIDE * 4) as usize];
    for target_y in 0..FACE_ALIGNMENT_SIDE {
        for target_x in 0..FACE_ALIGNMENT_SIDE {
            let (source_x, source_y) = transform.target_to_source(target_x as f32, target_y as f32);
            let pixel = bilinear_rgba(rgba, width, height, source_x, source_y);
            let offset = (target_y as usize * FACE_ALIGNMENT_SIDE as usize + target_x as usize) * 4;
            crop[offset..offset + 4].copy_from_slice(&pixel);
        }
    }
    crop
}

fn class_membership(
    classes: &[u8],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    accepts: impl Fn(u8) -> bool,
) -> f32 {
    if x < 0.0 || y < 0.0 || x > width as f32 - 1.0 || y > height as f32 - 1.0 {
        return 0.0;
    }
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let sample = |sample_x: u32, sample_y: u32| {
        let class = classes[sample_y as usize * width as usize + sample_x as usize];
        if accepts(class) {
            1.0
        } else {
            0.0
        }
    };
    let top = sample(x0, y0) * (1.0 - fx) + sample(x1, y0) * fx;
    let bottom = sample(x0, y1) * (1.0 - fx) + sample(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

fn face_iou(first: &FaceDetection, second: &FaceDetection) -> f32 {
    let left = first.x.max(second.x);
    let top = first.y.max(second.y);
    let right = (first.x + first.width).min(second.x + second.width);
    let bottom = (first.y + first.height).min(second.y + second.height);
    let intersection = (right - left).max(0.0) * (bottom - top).max(0.0);
    let union = first.width * first.height + second.width * second.height - intersection;
    if union <= f32::EPSILON {
        0.0
    } else {
        intersection / union
    }
}

type ModelValidationCache = HashMap<PathBuf, (u64, u128, bool)>;
static MODEL_VALIDATION_CACHE: OnceLock<Mutex<ModelValidationCache>> = OnceLock::new();

fn model_artifact_is_valid(id: ModelId, path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let cache = MODEL_VALIDATION_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock() {
        if let Some((length, cached_modified, valid)) = cache.get(path) {
            if *length == metadata.len() && *cached_modified == modified {
                return *valid;
            }
        }
    }

    let valid = (|| -> Result<bool, std::io::Error> {
        let mut file = File::open(path)?;
        let mut hasher = hmac_sha256::Hash::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let digest = hasher.finalize();
        let actual = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(actual == id.expected_sha256())
    })()
    .unwrap_or(false);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(path.to_path_buf(), (metadata.len(), modified, valid));
    }
    valid
}

/// Lazy local ONNX runner. Each model has an explicit tensor adapter below;
/// incompatible or missing artifacts fall back safely instead of blocking the
/// editor.
pub struct LocalOnnxRunner {
    metadata: ModelMetadata,
    path: PathBuf,
    prefer_gpu: bool,
    provider: AtomicU8,
}

impl LocalOnnxRunner {
    pub fn new(id: ModelId) -> Self {
        Self::with_gpu_preference(id, false)
    }

    fn with_gpu_preference(id: ModelId, prefer_gpu: bool) -> Self {
        let metadata = ModelMetadata::for_id(id);
        let path = model_path(id);
        Self {
            metadata,
            path,
            prefer_gpu,
            provider: AtomicU8::new(0),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn used_directml(&self) -> bool {
        self.provider.load(Ordering::Relaxed) == 1
    }

    fn build_session(&self, label: &str) -> Result<ort::session::Session, String> {
        let cpu_session = || {
            ort::session::Session::builder()
                .map_err(|error| format!("{label} ORT CPU builder: {error}"))?
                .commit_from_file(&self.path)
                .map_err(|error| format!("{label} load model CPU: {error}"))
        };
        if !self.prefer_gpu {
            self.provider.store(0, Ordering::Relaxed);
            return cpu_session();
        }

        // ONNX Runtime's DirectML EP requires sequential graph execution and
        // memory patterns disabled. If registration or model compilation is
        // unsupported on this adapter, rebuild a clean CPU session.
        let gpu_attempt = (|| {
            let provider = ort::ep::DirectML::default()
                .with_performance_preference(
                    ort::ep::directml::PerformancePreference::HighPerformance,
                )
                .with_device_filter(ort::ep::directml::DeviceFilter::Gpu)
                .build()
                .error_on_failure();
            ort::session::Session::builder()
                .map_err(|error| format!("{label} ORT DirectML builder: {error}"))?
                .with_parallel_execution(false)
                .map_err(|error| format!("{label} DirectML sequential mode: {error}"))?
                .with_memory_pattern(false)
                .map_err(|error| format!("{label} DirectML memory pattern: {error}"))?
                .with_execution_providers([provider])
                .map_err(|error| format!("{label} register DirectML: {error}"))?
                .commit_from_file(&self.path)
                .map_err(|error| format!("{label} load model DirectML: {error}"))
        })();
        match gpu_attempt {
            Ok(session) => {
                self.provider.store(1, Ordering::Relaxed);
                Ok(session)
            }
            Err(_gpu_error) => {
                self.provider.store(0, Ordering::Relaxed);
                cpu_session()
            }
        }
    }

    fn detect_faces(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<FaceDetection>, String> {
        const DETECTOR_LONG_EDGE: f32 = 640.0;
        const SCORE_THRESHOLD: f32 = 0.65;
        const NMS_THRESHOLD: f32 = 0.30;
        const STRIDES: [u32; 3] = [8, 16, 32];
        let pixels = width as usize * height as usize;
        if self.metadata.id != ModelId::FaceDetector || pixels == 0 || rgba.len() != pixels * 4 {
            return Err("YuNet: invalid detector or RGBA input".to_string());
        }

        let scale = DETECTOR_LONG_EDGE / width.max(height) as f32;
        let scaled_width = ((width as f32 * scale).round() as u32).max(1);
        let scaled_height = ((height as f32 * scale).round() as u32).max(1);
        let padded_width = ((scaled_width + 31) / 32) * 32;
        let padded_height = ((scaled_height + 31) / 32) * 32;
        let source = RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| "YuNet: invalid RGBA buffer".to_string())?;
        let resized = imageops::resize(
            &source,
            scaled_width,
            scaled_height,
            imageops::FilterType::Triangle,
        );
        let padded_pixels = padded_width as usize * padded_height as usize;
        let mut bgr = vec![0.0f32; padded_pixels * 3];
        for y in 0..scaled_height {
            for x in 0..scaled_width {
                let pixel = resized.get_pixel(x, y);
                let index = y as usize * padded_width as usize + x as usize;
                bgr[index] = pixel[2] as f32;
                bgr[padded_pixels + index] = pixel[1] as f32;
                bgr[padded_pixels * 2 + index] = pixel[0] as f32;
            }
        }
        let tensor = ort::value::Tensor::<f32>::from_array((
            [1i64, 3, padded_height as i64, padded_width as i64],
            bgr,
        ))
        .map_err(|e| format!("YuNet input tensor: {e}"))?;
        let mut session = self.build_session("YuNet")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input".to_string());
        let outputs = session
            .run(ort::inputs![input_name.as_str() => tensor])
            .map_err(|e| format!("YuNet inference: {e}"))?;
        let mut candidates = Vec::new();
        for (level, stride) in STRIDES.into_iter().enumerate() {
            let cols = padded_width / stride;
            let rows = padded_height / stride;
            let anchors = (cols * rows) as usize;
            let cls_name = format!("cls_{stride}");
            let obj_name = format!("obj_{stride}");
            let bbox_name = format!("bbox_{stride}");
            let kps_name = format!("kps_{stride}");
            let (_, cls) = outputs[cls_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("YuNet cls output {level}: {e}"))?;
            let (_, obj) = outputs[obj_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("YuNet obj output {level}: {e}"))?;
            let (_, bbox) = outputs[bbox_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("YuNet bbox output {level}: {e}"))?;
            let (_, kps) = outputs[kps_name.as_str()]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("YuNet kps output {level}: {e}"))?;
            if cls.len() < anchors
                || obj.len() < anchors
                || bbox.len() < anchors * 4
                || kps.len() < anchors * 10
            {
                return Err(format!("YuNet output contract mismatch at stride {stride}"));
            }
            for row in 0..rows {
                for col in 0..cols {
                    let index = (row * cols + col) as usize;
                    let score = (cls[index].clamp(0.0, 1.0) * obj[index].clamp(0.0, 1.0)).sqrt();
                    if score < SCORE_THRESHOLD {
                        continue;
                    }
                    let center_x = (col as f32 + bbox[index * 4]) * stride as f32;
                    let center_y = (row as f32 + bbox[index * 4 + 1]) * stride as f32;
                    let box_width = bbox[index * 4 + 2].exp() * stride as f32;
                    let box_height = bbox[index * 4 + 3].exp() * stride as f32;
                    let raw_x0 = center_x - box_width * 0.5;
                    let raw_y0 = center_y - box_height * 0.5;
                    let raw_x1 = center_x + box_width * 0.5;
                    let raw_y1 = center_y + box_height * 0.5;
                    let clipped_x0 = raw_x0.clamp(0.0, scaled_width as f32 - 1.0);
                    let clipped_y0 = raw_y0.clamp(0.0, scaled_height as f32 - 1.0);
                    let clipped_x1 = raw_x1.clamp(0.0, scaled_width as f32);
                    let clipped_y1 = raw_y1.clamp(0.0, scaled_height as f32);
                    if clipped_x1 - clipped_x0 < 4.0 || clipped_y1 - clipped_y0 < 4.0 {
                        continue;
                    }
                    let mut landmarks = [[0.0f32; 2]; 5];
                    for landmark in 0..5 {
                        landmarks[landmark][0] =
                            ((kps[index * 10 + landmark * 2] + col as f32) * stride as f32 / scale)
                                .clamp(0.0, width as f32 - 1.0);
                        landmarks[landmark][1] =
                            ((kps[index * 10 + landmark * 2 + 1] + row as f32) * stride as f32
                                / scale)
                                .clamp(0.0, height as f32 - 1.0);
                    }
                    candidates.push(FaceDetection {
                        x: clipped_x0 / scale,
                        y: clipped_y0 / scale,
                        width: (clipped_x1 - clipped_x0) / scale,
                        height: (clipped_y1 - clipped_y0) / scale,
                        landmarks,
                        score,
                    });
                }
            }
        }
        candidates.sort_by(|first, second| second.score.total_cmp(&first.score));
        let mut kept = Vec::new();
        for candidate in candidates {
            if kept
                .iter()
                .all(|existing| face_iou(&candidate, existing) < NMS_THRESHOLD)
            {
                kept.push(candidate);
                if kept.len() >= 32 {
                    break;
                }
            }
        }
        Ok(kept)
    }

    fn run_iat_direct(&self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        let pixels = width as usize * height as usize;
        if pixels == 0 || rgba.len() != pixels * 4 {
            return Err("IAT: invalid RGBA input".to_string());
        }

        // Contract produced by scripts/export_retouch_onnx.py:
        // input/output = float32 NCHW RGB in [0, 1], batch=1, dynamic H/W.
        let mut chw = vec![0.0f32; pixels * 3];
        for (i, pixel) in rgba.chunks_exact(4).enumerate() {
            chw[i] = pixel[0] as f32 / 255.0;
            chw[pixels + i] = pixel[1] as f32 / 255.0;
            chw[pixels * 2 + i] = pixel[2] as f32 / 255.0;
        }
        let tensor =
            ort::value::Tensor::<f32>::from_array(([1i64, 3, height as i64, width as i64], chw))
                .map_err(|e| format!("IAT input tensor: {e}"))?;
        let mut session = self.build_session("IAT")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input".to_string());
        let outputs = session
            .run(ort::inputs![input_name.as_str() => tensor])
            .map_err(|e| format!("IAT inference: {e}"))?;
        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("IAT output tensor: {e}"))?;
        if data.len() != pixels * 3 {
            return Err(format!(
                "IAT output contract mismatch: expected {} RGB values, got {}",
                pixels * 3,
                data.len()
            ));
        }

        let mut out = Vec::with_capacity(rgba.len());
        for i in 0..pixels {
            out.push((data[i].clamp(0.0, 1.0) * 255.0).round() as u8);
            out.push((data[pixels + i].clamp(0.0, 1.0) * 255.0).round() as u8);
            out.push((data[pixels * 2 + i].clamp(0.0, 1.0) * 255.0).round() as u8);
            out.push(rgba[i * 4 + 3]);
        }
        Ok(out)
    }

    fn run_iat(&self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        const MAX_IAT_LONG_EDGE: u32 = 1536;
        if width.max(height) <= MAX_IAT_LONG_EDGE {
            return self.run_iat_direct(rgba, width, height);
        }
        let pixels = width as usize * height as usize;
        if pixels == 0 || rgba.len() != pixels * 4 {
            return Err("IAT: invalid RGBA input".to_string());
        }
        // IAT predicts illumination/colour, not fine texture. Running its
        // dynamic network on a bounded preview avoids multi-gigabyte activation
        // memory at 12/24 MP. The smooth learned correction field is then
        // resized to source resolution and blended by the pipeline.
        let scale = MAX_IAT_LONG_EDGE as f32 / width.max(height) as f32;
        let scaled_width = ((width as f32 * scale).round() as u32).max(1);
        let scaled_height = ((height as f32 * scale).round() as u32).max(1);
        let source = RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| "IAT: invalid RGBA buffer".to_string())?;
        let preview = imageops::resize(
            &source,
            scaled_width,
            scaled_height,
            imageops::FilterType::Triangle,
        );
        let corrected = self.run_iat_direct(preview.as_raw(), preview.width(), preview.height())?;
        let corrected = RgbaImage::from_raw(scaled_width, scaled_height, corrected)
            .ok_or_else(|| "IAT: invalid corrected preview".to_string())?;
        let mut upscaled =
            imageops::resize(&corrected, width, height, imageops::FilterType::Triangle).into_raw();
        for (index, pixel) in upscaled.chunks_exact_mut(4).enumerate() {
            pixel[3] = rgba[index * 4 + 3];
        }
        Ok(upscaled)
    }

    fn run_nafnet(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        cancel: Option<&AtomicBool>,
        progress: Option<&dyn Fn(usize, usize)>,
    ) -> Result<Vec<u8>, String> {
        cancel_if_requested_optional(cancel)?;
        let pixels = width as usize * height as usize;
        if pixels == 0 || rgba.len() != pixels * 4 {
            return Err("NAFNet: invalid RGBA input".to_string());
        }

        // The exported model accepts dynamic H/W as long as both are multiples
        // of 16. Large images are processed with 512px tiles and 64px overlap.
        // Edge tiles replicate their last source pixel into the padded area.
        let tile_width = (((width.min(self.metadata.tile_size) + 15) / 16) * 16).max(16);
        let tile_height = (((height.min(self.metadata.tile_size) + 15) / 16) * 16).max(16);
        let overlap = self
            .metadata
            .overlap
            .min(tile_width.saturating_sub(16))
            .min(tile_height.saturating_sub(16));
        let stride_x = tile_width.saturating_sub(overlap).max(16);
        let stride_y = tile_height.saturating_sub(overlap).max(16);
        let origins = |length: u32, tile: u32, stride: u32| {
            let mut values = Vec::new();
            let mut origin: u32 = 0;
            loop {
                values.push(origin);
                if origin.saturating_add(tile) >= length {
                    break;
                }
                origin = origin.saturating_add(stride);
            }
            values
        };
        let xs = origins(width, tile_width, stride_x);
        let ys = origins(height, tile_height, stride_y);
        let tile_pixels = tile_width as usize * tile_height as usize;
        let mut accumulated = vec![0.0f32; pixels * 3];
        let mut weights = vec![0.0f32; pixels];

        let mut session = self.build_session("NAFNet")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input".to_string());

        let total_tiles = xs.len() * ys.len();
        let mut completed_tiles = 0usize;
        for &origin_y in &ys {
            for &origin_x in &xs {
                cancel_if_requested_optional(cancel)?;
                let valid_width = tile_width.min(width - origin_x);
                let valid_height = tile_height.min(height - origin_y);
                let mut chw = vec![0.0f32; tile_pixels * 3];
                for tile_y in 0..tile_height {
                    let source_y = (origin_y + tile_y).min(height - 1);
                    for tile_x in 0..tile_width {
                        let source_x = (origin_x + tile_x).min(width - 1);
                        let source = (source_y as usize * width as usize + source_x as usize) * 4;
                        let tile_index = tile_y as usize * tile_width as usize + tile_x as usize;
                        chw[tile_index] = rgba[source] as f32 / 255.0;
                        chw[tile_pixels + tile_index] = rgba[source + 1] as f32 / 255.0;
                        chw[tile_pixels * 2 + tile_index] = rgba[source + 2] as f32 / 255.0;
                    }
                }
                let tensor = ort::value::Tensor::<f32>::from_array((
                    [1i64, 3, tile_height as i64, tile_width as i64],
                    chw,
                ))
                .map_err(|e| format!("NAFNet input tensor: {e}"))?;
                let outputs = session
                    .run(ort::inputs![input_name.as_str() => tensor])
                    .map_err(|e| format!("NAFNet inference: {e}"))?;
                let (_, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| format!("NAFNet output tensor: {e}"))?;
                if data.len() != tile_pixels * 3 {
                    return Err(format!(
                        "NAFNet output contract mismatch: expected {} RGB values, got {}",
                        tile_pixels * 3,
                        data.len()
                    ));
                }

                for tile_y in 0..valid_height {
                    for tile_x in 0..valid_width {
                        let mut weight = 1.0f32;
                        if origin_x > 0 && tile_x < overlap {
                            weight *= (tile_x + 1) as f32 / (overlap + 1) as f32;
                        }
                        if origin_x + valid_width < width && tile_x + overlap >= valid_width {
                            weight *= (valid_width - tile_x) as f32 / (overlap + 1) as f32;
                        }
                        if origin_y > 0 && tile_y < overlap {
                            weight *= (tile_y + 1) as f32 / (overlap + 1) as f32;
                        }
                        if origin_y + valid_height < height && tile_y + overlap >= valid_height {
                            weight *= (valid_height - tile_y) as f32 / (overlap + 1) as f32;
                        }
                        let target = (origin_y + tile_y) as usize * width as usize
                            + (origin_x + tile_x) as usize;
                        let tile_index = tile_y as usize * tile_width as usize + tile_x as usize;
                        weights[target] += weight;
                        for channel in 0..3 {
                            accumulated[channel * pixels + target] +=
                                data[channel * tile_pixels + tile_index].clamp(0.0, 1.0) * weight;
                        }
                    }
                }
                completed_tiles += 1;
                emit_work_progress(progress, completed_tiles, total_tiles);
            }
        }

        let mut out = Vec::with_capacity(rgba.len());
        for i in 0..pixels {
            let weight = weights[i].max(f32::EPSILON);
            for channel in 0..3 {
                out.push(
                    (accumulated[channel * pixels + i] / weight * 255.0)
                        .clamp(0.0, 255.0)
                        .round() as u8,
                );
            }
            out.push(rgba[i * 4 + 3]);
        }
        Ok(out)
    }

    fn run_realesrgan_detail(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        detail_mask: &[f32],
        cancel: Option<&AtomicBool>,
        progress: Option<&dyn Fn(usize, usize)>,
    ) -> Result<Vec<u8>, String> {
        cancel_if_requested_optional(cancel)?;
        const TILE: u32 = 256;
        const OVERLAP: u32 = 32;
        const SCALE: u32 = 4;
        let pixels = width as usize * height as usize;
        if pixels == 0 || rgba.len() != pixels * 4 || detail_mask.len() != pixels {
            return Err("Real-ESRGAN: invalid image or detail mask".to_string());
        }

        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for (index, &value) in detail_mask.iter().enumerate() {
            if value <= 0.015 {
                continue;
            }
            let x = index as u32 % width;
            let y = index as u32 / width;
            bounds = Some(match bounds {
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                None => (x, y, x, y),
            });
        }
        let Some((mut x0, mut y0, mut x1, mut y1)) = bounds else {
            return Ok(rgba.to_vec());
        };
        // Context outside the selected mask prevents the network from treating
        // the mask boundary as an image edge.
        x0 = x0.saturating_sub(OVERLAP);
        y0 = y0.saturating_sub(OVERLAP);
        x1 = x1.saturating_add(OVERLAP).min(width - 1);
        y1 = y1.saturating_add(OVERLAP).min(height - 1);
        let roi_width = x1 - x0 + 1;
        let roi_height = y1 - y0 + 1;
        let tile_width = TILE.min(roi_width).max(1);
        let tile_height = TILE.min(roi_height).max(1);
        let stride_x = tile_width
            .saturating_sub(OVERLAP.min(tile_width / 2))
            .max(1);
        let stride_y = tile_height
            .saturating_sub(OVERLAP.min(tile_height / 2))
            .max(1);
        let origins = |start: u32, length: u32, tile: u32, stride: u32| {
            let mut values = Vec::new();
            let mut local = 0u32;
            loop {
                values.push(start + local);
                if local.saturating_add(tile) >= length {
                    break;
                }
                local = local.saturating_add(stride);
            }
            values
        };
        let xs = origins(x0, roi_width, tile_width, stride_x);
        let ys = origins(y0, roi_height, tile_height, stride_y);
        let tile_pixels = tile_width as usize * tile_height as usize;
        let mut accumulated = vec![0.0f32; pixels * 3];
        let mut weights = vec![0.0f32; pixels];

        let mut session = self.build_session("Real-ESRGAN")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input".to_string());

        let total_tiles = xs.len() * ys.len();
        let mut completed_tiles = 0usize;
        for &origin_y in &ys {
            for &origin_x in &xs {
                cancel_if_requested_optional(cancel)?;
                let local_x = origin_x - x0;
                let local_y = origin_y - y0;
                let valid_width = tile_width.min(roi_width - local_x);
                let valid_height = tile_height.min(roi_height - local_y);
                let mut chw = vec![0.0f32; tile_pixels * 3];
                for tile_y in 0..tile_height {
                    let source_y = (origin_y + tile_y).min(height - 1);
                    for tile_x in 0..tile_width {
                        let source_x = (origin_x + tile_x).min(width - 1);
                        let source = (source_y as usize * width as usize + source_x as usize) * 4;
                        let tile_index = tile_y as usize * tile_width as usize + tile_x as usize;
                        chw[tile_index] = rgba[source] as f32 / 255.0;
                        chw[tile_pixels + tile_index] = rgba[source + 1] as f32 / 255.0;
                        chw[tile_pixels * 2 + tile_index] = rgba[source + 2] as f32 / 255.0;
                    }
                }
                let tensor = ort::value::Tensor::<f32>::from_array((
                    [1i64, 3, tile_height as i64, tile_width as i64],
                    chw,
                ))
                .map_err(|e| format!("Real-ESRGAN input tensor: {e}"))?;
                let outputs = session
                    .run(ort::inputs![input_name.as_str() => tensor])
                    .map_err(|e| format!("Real-ESRGAN inference: {e}"))?;
                let (_, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| format!("Real-ESRGAN output tensor: {e}"))?;
                let output_width = tile_width * SCALE;
                let output_height = tile_height * SCALE;
                let output_pixels = output_width as usize * output_height as usize;
                if data.len() != output_pixels * 3 {
                    return Err(format!(
                        "Real-ESRGAN output contract mismatch: expected {} RGB values, got {}",
                        output_pixels * 3,
                        data.len()
                    ));
                }
                let mut interleaved = vec![0.0f32; output_pixels * 3];
                for i in 0..output_pixels {
                    interleaved[i * 3] = data[i].clamp(0.0, 1.0);
                    interleaved[i * 3 + 1] = data[output_pixels + i].clamp(0.0, 1.0);
                    interleaved[i * 3 + 2] = data[output_pixels * 2 + i].clamp(0.0, 1.0);
                }
                let high = Rgb32FImage::from_raw(output_width, output_height, interleaved)
                    .ok_or_else(|| "Real-ESRGAN: invalid x4 output buffer".to_string())?;
                let restored = imageops::resize(
                    &high,
                    tile_width,
                    tile_height,
                    imageops::FilterType::Lanczos3,
                );

                for tile_y in 0..valid_height {
                    for tile_x in 0..valid_width {
                        let mut weight = 1.0f32;
                        if local_x > 0 && tile_x < OVERLAP {
                            weight *= (tile_x + 1) as f32 / (OVERLAP + 1) as f32;
                        }
                        if local_x + valid_width < roi_width && tile_x + OVERLAP >= valid_width {
                            weight *= (valid_width - tile_x) as f32 / (OVERLAP + 1) as f32;
                        }
                        if local_y > 0 && tile_y < OVERLAP {
                            weight *= (tile_y + 1) as f32 / (OVERLAP + 1) as f32;
                        }
                        if local_y + valid_height < roi_height && tile_y + OVERLAP >= valid_height {
                            weight *= (valid_height - tile_y) as f32 / (OVERLAP + 1) as f32;
                        }
                        let target = (origin_y + tile_y) as usize * width as usize
                            + (origin_x + tile_x) as usize;
                        let pixel = restored.get_pixel(tile_x, tile_y);
                        weights[target] += weight;
                        for channel in 0..3 {
                            accumulated[channel * pixels + target] += pixel[channel] * weight;
                        }
                    }
                }
                completed_tiles += 1;
                emit_work_progress(progress, completed_tiles, total_tiles);
            }
        }

        let mut out = rgba.to_vec();
        for y in y0..=y1 {
            for x in x0..=x1 {
                let i = y as usize * width as usize + x as usize;
                if weights[i] <= f32::EPSILON {
                    continue;
                }
                for channel in 0..3 {
                    out[i * 4 + channel] = (accumulated[channel * pixels + i] / weights[i] * 255.0)
                        .clamp(0.0, 255.0)
                        .round() as u8;
                }
            }
        }
        Ok(out)
    }

    fn run_gfpgan_face_details(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        transforms: &[SimilarityTransform],
        cancel: Option<&AtomicBool>,
        progress: Option<&dyn Fn(usize, usize)>,
    ) -> Result<Vec<u8>, String> {
        cancel_if_requested_optional(cancel)?;
        let pixels = width as usize * height as usize;
        let aligned_pixels = (FACE_ALIGNMENT_SIDE * FACE_ALIGNMENT_SIDE) as usize;
        if self.metadata.id != ModelId::Gfpgan
            || pixels == 0
            || rgba.len() != pixels * 4
            || transforms.is_empty()
        {
            return Err("GFPGAN: invalid image or aligned face list".to_string());
        }
        let mut session = self.build_session("GFPGAN")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input".to_string());
        let mut accumulated = vec![0.0f32; pixels * 3];
        let mut weights = vec![0.0f32; pixels];

        for (face_index, transform) in transforms.iter().copied().enumerate() {
            cancel_if_requested_optional(cancel)?;
            let crop = aligned_face_crop(rgba, width, height, transform);
            let mut chw = vec![0.0f32; aligned_pixels * 3];
            for (index, pixel) in crop.chunks_exact(4).enumerate() {
                chw[index] = pixel[0] as f32 / 127.5 - 1.0;
                chw[aligned_pixels + index] = pixel[1] as f32 / 127.5 - 1.0;
                chw[aligned_pixels * 2 + index] = pixel[2] as f32 / 127.5 - 1.0;
            }
            let tensor = ort::value::Tensor::<f32>::from_array((
                [
                    1i64,
                    3,
                    FACE_ALIGNMENT_SIDE as i64,
                    FACE_ALIGNMENT_SIDE as i64,
                ],
                chw,
            ))
            .map_err(|e| format!("GFPGAN input tensor: {e}"))?;
            let outputs = session
                .run(ort::inputs![input_name.as_str() => tensor])
                .map_err(|e| format!("GFPGAN inference: {e}"))?;
            let (_, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("GFPGAN output tensor: {e}"))?;
            if data.len() != aligned_pixels * 3 {
                return Err(format!(
                    "GFPGAN output contract mismatch: expected {} RGB values, got {}",
                    aligned_pixels * 3,
                    data.len()
                ));
            }

            let mut restored = RgbaImage::new(FACE_ALIGNMENT_SIDE, FACE_ALIGNMENT_SIDE);
            for index in 0..aligned_pixels {
                restored.put_pixel(
                    (index % FACE_ALIGNMENT_SIDE as usize) as u32,
                    (index / FACE_ALIGNMENT_SIDE as usize) as u32,
                    image::Rgba([
                        (data[index].clamp(0.0, 1.0) * 255.0).round() as u8,
                        (data[aligned_pixels + index].clamp(0.0, 1.0) * 255.0).round() as u8,
                        (data[aligned_pixels * 2 + index].clamp(0.0, 1.0) * 255.0).round() as u8,
                        255,
                    ]),
                );
            }
            // Transfer a wider mid/high-frequency band than the old 1.35 px
            // high-pass. At small face sizes that band disappeared during the
            // inverse warp, making Face Restore look like a no-op even at 100.
            // Low-frequency shape and colour still come exclusively from the
            // source crop, so this does not paste GFPGAN's generated identity.
            let low_frequency = imageops::blur(&restored, 4.00);
            let mut texture_transfer = crop.clone();
            for index in 0..aligned_pixels {
                for channel in 0..3 {
                    let generated_detail = restored.as_raw()[index * 4 + channel] as f32
                        - low_frequency.as_raw()[index * 4 + channel] as f32;
                    texture_transfer[index * 4 + channel] = (crop[index * 4 + channel] as f32
                        + generated_detail * 1.10)
                        .clamp(0.0, 255.0)
                        .round() as u8;
                }
            }

            let corners = [
                transform.target_to_source(0.0, 0.0),
                transform.target_to_source(FACE_ALIGNMENT_SIDE as f32 - 1.0, 0.0),
                transform.target_to_source(0.0, FACE_ALIGNMENT_SIDE as f32 - 1.0),
                transform.target_to_source(
                    FACE_ALIGNMENT_SIDE as f32 - 1.0,
                    FACE_ALIGNMENT_SIDE as f32 - 1.0,
                ),
            ];
            let min_x = corners
                .iter()
                .map(|point| point.0)
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(0.0) as u32;
            let min_y = corners
                .iter()
                .map(|point| point.1)
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(0.0) as u32;
            let max_x = corners
                .iter()
                .map(|point| point.0)
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil()
                .min(width as f32 - 1.0) as u32;
            let max_y = corners
                .iter()
                .map(|point| point.1)
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil()
                .min(height as f32 - 1.0) as u32;
            if min_x > max_x || min_y > max_y {
                continue;
            }
            for source_y in min_y..=max_y {
                for source_x in min_x..=max_x {
                    let (target_x, target_y) =
                        transform.source_to_target(source_x as f32, source_y as f32);
                    if target_x < 0.0
                        || target_y < 0.0
                        || target_x > FACE_ALIGNMENT_SIDE as f32 - 1.0
                        || target_y > FACE_ALIGNMENT_SIDE as f32 - 1.0
                    {
                        continue;
                    }
                    let crop_edge = target_x
                        .min(target_y)
                        .min(FACE_ALIGNMENT_SIDE as f32 - 1.0 - target_x)
                        .min(FACE_ALIGNMENT_SIDE as f32 - 1.0 - target_y);
                    let weight = (crop_edge / 24.0).clamp(0.0, 1.0);
                    if weight <= 0.0 {
                        continue;
                    }
                    let sample = bilinear_rgba(
                        &texture_transfer,
                        FACE_ALIGNMENT_SIDE,
                        FACE_ALIGNMENT_SIDE,
                        target_x,
                        target_y,
                    );
                    let index = source_y as usize * width as usize + source_x as usize;
                    weights[index] += weight;
                    for channel in 0..3 {
                        accumulated[channel * pixels + index] += sample[channel] as f32 * weight;
                    }
                }
            }
            emit_work_progress(progress, face_index + 1, transforms.len());
        }

        let mut out = rgba.to_vec();
        for index in 0..pixels {
            if weights[index] <= f32::EPSILON {
                continue;
            }
            for channel in 0..3 {
                out[index * 4 + channel] = (accumulated[channel * pixels + index] / weights[index])
                    .clamp(0.0, 255.0)
                    .round() as u8;
            }
        }
        Ok(out)
    }

    fn run_realesrgan_upscale(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        scale: u32,
        cancel: Option<&AtomicBool>,
        progress: Option<&dyn Fn(usize, usize)>,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        cancel_if_requested_optional(cancel)?;
        const TILE: u32 = 256;
        const OVERLAP: u32 = 32;
        let expected_id = if scale == 2 {
            ModelId::RealesrganRrdbX2
        } else if scale == 4 {
            ModelId::RealesrganRrdb
        } else {
            return Err("Real-ESRGAN upscale only supports x2 or x4".to_string());
        };
        let pixels = width as usize * height as usize;
        if self.metadata.id != expected_id || pixels == 0 || rgba.len() != pixels * 4 {
            return Err("Real-ESRGAN upscale: invalid model or RGBA input".to_string());
        }
        let output_width = width
            .checked_mul(scale)
            .ok_or_else(|| "Real-ESRGAN upscale width overflow".to_string())?;
        let output_height = height
            .checked_mul(scale)
            .ok_or_else(|| "Real-ESRGAN upscale height overflow".to_string())?;
        let output_pixels = output_width as u64 * output_height as u64;
        if output_pixels > MAX_UPSCALE_PIXELS {
            return Err(format!(
                "Real-ESRGAN upscale cần {} triệu pixel, vượt giới hạn an toàn {} triệu pixel",
                output_pixels / 1_000_000,
                MAX_UPSCALE_PIXELS / 1_000_000
            ));
        }

        let tile_width = TILE.min(width).max(2);
        let tile_height = TILE.min(height).max(2);
        let tile_width = if scale == 2 && tile_width % 2 != 0 {
            tile_width + 1
        } else {
            tile_width
        };
        let tile_height = if scale == 2 && tile_height % 2 != 0 {
            tile_height + 1
        } else {
            tile_height
        };
        let stride_x = tile_width.saturating_sub(OVERLAP).max(2);
        let stride_y = tile_height.saturating_sub(OVERLAP).max(2);
        let origins = |length: u32, tile: u32, stride: u32| {
            let mut values = Vec::new();
            let mut origin = 0u32;
            loop {
                values.push(origin);
                if origin.saturating_add(tile) >= length {
                    break;
                }
                origin = origin.saturating_add(stride);
            }
            values
        };
        let xs = origins(width, tile_width, stride_x);
        let ys = origins(height, tile_height, stride_y);
        let tile_pixels = tile_width as usize * tile_height as usize;
        let high_tile_width = tile_width * scale;
        let high_tile_height = tile_height * scale;
        let high_tile_pixels = high_tile_width as usize * high_tile_height as usize;
        // Alpha is used as an initialization sentinel while tiles are merged;
        // the actual upscaled alpha channel replaces it after RGB composition.
        let mut out = vec![0u8; output_pixels as usize * 4];

        let mut session = self.build_session("Real-ESRGAN RRDB")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input".to_string());

        let total_tiles = xs.len() * ys.len();
        let mut completed_tiles = 0usize;
        for &origin_y in &ys {
            for &origin_x in &xs {
                cancel_if_requested_optional(cancel)?;
                let valid_width = tile_width.min(width - origin_x);
                let valid_height = tile_height.min(height - origin_y);
                let mut chw = vec![0.0f32; tile_pixels * 3];
                for tile_y in 0..tile_height {
                    let source_y = (origin_y + tile_y).min(height - 1);
                    for tile_x in 0..tile_width {
                        let source_x = (origin_x + tile_x).min(width - 1);
                        let source = (source_y as usize * width as usize + source_x as usize) * 4;
                        let index = tile_y as usize * tile_width as usize + tile_x as usize;
                        chw[index] = rgba[source] as f32 / 255.0;
                        chw[tile_pixels + index] = rgba[source + 1] as f32 / 255.0;
                        chw[tile_pixels * 2 + index] = rgba[source + 2] as f32 / 255.0;
                    }
                }
                let tensor = ort::value::Tensor::<f32>::from_array((
                    [1i64, 3, tile_height as i64, tile_width as i64],
                    chw,
                ))
                .map_err(|e| format!("Real-ESRGAN RRDB input tensor: {e}"))?;
                let outputs = session
                    .run(ort::inputs![input_name.as_str() => tensor])
                    .map_err(|e| format!("Real-ESRGAN RRDB inference: {e}"))?;
                let (_, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| format!("Real-ESRGAN RRDB output tensor: {e}"))?;
                if data.len() != high_tile_pixels * 3 {
                    return Err(format!(
                        "Real-ESRGAN RRDB output mismatch: expected {}, got {}",
                        high_tile_pixels * 3,
                        data.len()
                    ));
                }

                let valid_high_width = valid_width * scale;
                let valid_high_height = valid_height * scale;
                let overlap_high = OVERLAP * scale;
                for tile_y in 0..valid_high_height {
                    for tile_x in 0..valid_high_width {
                        let mut incoming_weight = 1.0f32;
                        if origin_x > 0 && tile_x < overlap_high {
                            incoming_weight *= (tile_x + 1) as f32 / (overlap_high + 1) as f32;
                        }
                        if origin_y > 0 && tile_y < overlap_high {
                            incoming_weight *= (tile_y + 1) as f32 / (overlap_high + 1) as f32;
                        }
                        let target_x = origin_x * scale + tile_x;
                        let target_y = origin_y * scale + tile_y;
                        let target =
                            (target_y as usize * output_width as usize + target_x as usize) * 4;
                        let tile_index =
                            tile_y as usize * high_tile_width as usize + tile_x as usize;
                        for channel in 0..3 {
                            let value = (data[channel * high_tile_pixels + tile_index]
                                .clamp(0.0, 1.0)
                                * 255.0)
                                .round();
                            out[target + channel] = if out[target + 3] == 0 {
                                value as u8
                            } else {
                                (out[target + channel] as f32 * (1.0 - incoming_weight)
                                    + value * incoming_weight)
                                    .clamp(0.0, 255.0)
                                    .round() as u8
                            };
                        }
                        out[target + 3] = 255;
                    }
                }
                completed_tiles += 1;
                emit_work_progress(progress, completed_tiles, total_tiles);
            }
        }

        let mut source_alpha = GrayImage::new(width, height);
        for index in 0..pixels {
            source_alpha.put_pixel(
                (index % width as usize) as u32,
                (index / width as usize) as u32,
                image::Luma([rgba[index * 4 + 3]]),
            );
        }
        let alpha = imageops::resize(
            &source_alpha,
            output_width,
            output_height,
            imageops::FilterType::Triangle,
        );
        for (index, value) in alpha.into_raw().into_iter().enumerate() {
            out[index * 4 + 3] = value;
        }
        Ok((out, output_width, output_height))
    }

    fn run_body_parsing_masks(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<SemanticMasks, String> {
        const SIDE: u32 = 256;
        const CLASSES: usize = 6;
        cancel_if_requested_optional(cancel)?;
        if self.metadata.id != ModelId::BodyParsing
            || width == 0
            || height == 0
            || rgba.len() != width as usize * height as usize * 4
        {
            return Err("MediaPipe multiclass: invalid model or RGBA input".to_string());
        }
        let source = RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| "MediaPipe multiclass: invalid RGBA buffer".to_string())?;
        let resized = imageops::resize(&source, SIDE, SIDE, imageops::FilterType::Triangle);
        let count = (SIDE * SIDE) as usize;
        // Official SelfieMulticlass contract: float32 NHWC RGB in [0, 1].
        let mut nhwc = Vec::with_capacity(count * 3);
        for pixel in resized.pixels() {
            nhwc.push(pixel[0] as f32 / 255.0);
            nhwc.push(pixel[1] as f32 / 255.0);
            nhwc.push(pixel[2] as f32 / 255.0);
        }
        let tensor =
            ort::value::Tensor::<f32>::from_array(([1i64, SIDE as i64, SIDE as i64, 3], nhwc))
                .map_err(|error| format!("MediaPipe multiclass input tensor: {error}"))?;
        let mut session = self.build_session("MediaPipe multiclass")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input_29".to_string());
        let outputs = session
            .run(ort::inputs![input_name.as_str() => tensor])
            .map_err(|error| format!("MediaPipe multiclass inference: {error}"))?;
        let (_, logits) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|error| format!("MediaPipe multiclass output tensor: {error}"))?;
        if logits.len() != count * CLASSES {
            return Err(format!(
                "MediaPipe multiclass output mismatch: expected {}, got {}",
                count * CLASSES,
                logits.len()
            ));
        }
        let mut classes = vec![0u8; count];
        for index in 0..count {
            let offset = index * CLASSES;
            let mut best_class = 0usize;
            let mut best_logit = logits[offset];
            for class in 1..CLASSES {
                if logits[offset + class] > best_logit {
                    best_logit = logits[offset + class];
                    best_class = class;
                }
            }
            classes[index] = best_class as u8;
        }
        let plane = |wanted: &[u8]| {
            let low = GrayImage::from_raw(
                SIDE,
                SIDE,
                classes
                    .iter()
                    .map(|class| u8::from(wanted.contains(class)) * 255)
                    .collect(),
            )
            .expect("MediaPipe mask dimensions are exact");
            imageops::resize(&low, width, height, imageops::FilterType::Triangle)
                .into_raw()
                .into_iter()
                .map(|value| value as f32 / 255.0)
                .collect::<Vec<_>>()
        };
        let mut masks = SemanticMasks::empty(width, height);
        // 0 background, 1 hair, 2 body skin, 3 face skin, 4 clothes, 5 other.
        masks.background = plane(&[0]);
        masks.hair = plane(&[1]);
        masks.skin = plane(&[2, 3]);
        masks.clothes = plane(&[4]);
        masks.accessories = plane(&[5]);
        masks.ai_generated = true;
        refine_semantic_masks(rgba, width, height, &mut masks, cancel)?;
        Ok(masks)
    }

    fn run_bisenet_classes(&self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
        const SIDE: u32 = 512;
        let image = RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| "BiSeNet: invalid RGBA input".to_string())?;
        let resized = imageops::resize(&image, SIDE, SIDE, imageops::FilterType::Triangle);
        let count = (SIDE * SIDE) as usize;
        let mut chw = vec![0.0f32; count * 3];
        for (i, pixel) in resized.pixels().enumerate() {
            let red = pixel[0] as f32 / 255.0;
            let green = pixel[1] as f32 / 255.0;
            let blue = pixel[2] as f32 / 255.0;
            chw[i] = (red - 0.485) / 0.229;
            chw[count + i] = (green - 0.456) / 0.224;
            chw[count * 2 + i] = (blue - 0.406) / 0.225;
        }
        let tensor =
            ort::value::Tensor::<f32>::from_array(([1i64, 3, SIDE as i64, SIDE as i64], chw))
                .map_err(|e| format!("BiSeNet input tensor: {e}"))?;
        let mut session = self.build_session("BiSeNet")?;
        let input_name = session
            .inputs()
            .first()
            .map(|input| input.name().to_string())
            .unwrap_or_else(|| "input".to_string());
        let outputs = session
            .run(ort::inputs![input_name.as_str() => tensor])
            .map_err(|e| format!("BiSeNet inference: {e}"))?;
        let (_, logits) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("BiSeNet output tensor: {e}"))?;
        if logits.len() != count * 19 {
            return Err(format!(
                "BiSeNet output contract mismatch: expected {} logits, got {}",
                count * 19,
                logits.len()
            ));
        }
        let mut classes = GrayImage::new(SIDE, SIDE);
        for index in 0..count {
            let mut best_class = 0u8;
            let mut best_logit = logits[index];
            for class in 1..19usize {
                let logit = logits[class * count + index];
                if logit > best_logit {
                    best_logit = logit;
                    best_class = class as u8;
                }
            }
            classes.put_pixel(
                (index % SIDE as usize) as u32,
                (index / SIDE as usize) as u32,
                image::Luma([best_class]),
            );
        }
        Ok(imageops::resize(&classes, width, height, imageops::FilterType::Nearest).into_raw())
    }
}

impl IModelRunner for LocalOnnxRunner {
    fn metadata(&self) -> &ModelMetadata {
        &self.metadata
    }

    fn available(&self) -> bool {
        model_artifact_is_valid(self.metadata.id, &self.path)
    }

    fn run_image(&mut self, _rgba: &[u8], _width: u32, _height: u32) -> Result<Vec<u8>, String> {
        if self.metadata.id == ModelId::Iat {
            return self.run_iat(_rgba, _width, _height);
        }
        if self.metadata.id == ModelId::Nafnet {
            return self.run_nafnet(_rgba, _width, _height, None, None);
        }
        if self.metadata.id == ModelId::BodyParsing {
            return Ok(self
                .run_body_parsing_masks(_rgba, _width, _height, None)?
                .background
                .into_iter()
                .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect());
        }
        if self.metadata.id != ModelId::Bisenet {
            return Err(format!(
                "{} model found at {} but its export tensor contract is not enabled; using CPU fallback",
                self.metadata.display_name,
                self.path.display()
            ));
        }
        Ok(self
            .run_bisenet_classes(_rgba, _width, _height)?
            .into_iter()
            .map(|class| if class == 0 { 0 } else { 255 })
            .collect())
    }
}

fn generate_aligned_semantic_masks(
    rgba: &[u8],
    width: u32,
    height: u32,
    detector: &LocalOnnxRunner,
    parser: &LocalOnnxRunner,
    cancel: Option<&AtomicBool>,
    progress: Option<&dyn Fn(usize, usize)>,
) -> Result<SemanticMasks, String> {
    cancel_if_requested_optional(cancel)?;
    const SIDE: u32 = FACE_ALIGNMENT_SIDE;
    let detections = detector.detect_faces(rgba, width, height)?;
    if detections.is_empty() {
        return Err("YuNet không tìm thấy khuôn mặt đủ tin cậy".to_string());
    }
    let count = width as usize * height as usize;
    let mut masks = SemanticMasks {
        face: vec![0.0; count],
        skin: vec![0.0; count],
        hair: vec![0.0; count],
        eyes_and_brows: vec![0.0; count],
        lips: vec![0.0; count],
        clothes: vec![0.0; count],
        background: vec![0.0; count],
        accessories: vec![0.0; count],
        aligned_faces: Vec::with_capacity(detections.len()),
        face_count: detections.len(),
        ai_generated: true,
    };

    let mask_work_total = detections.len() + 1;
    for (face_index, detection) in detections.iter().enumerate() {
        cancel_if_requested_optional(cancel)?;
        let transform = SimilarityTransform::fit(&detection.landmarks, &FACE_ALIGNMENT_TARGET)?;
        let crop = aligned_face_crop(rgba, width, height, transform);
        let classes = parser.run_bisenet_classes(&crop, SIDE, SIDE)?;

        let corners = [
            transform.target_to_source(0.0, 0.0),
            transform.target_to_source(SIDE as f32 - 1.0, 0.0),
            transform.target_to_source(0.0, SIDE as f32 - 1.0),
            transform.target_to_source(SIDE as f32 - 1.0, SIDE as f32 - 1.0),
        ];
        let min_x = corners
            .iter()
            .map(|point| point.0)
            .fold(f32::INFINITY, f32::min)
            .floor()
            .max(0.0) as u32;
        let min_y = corners
            .iter()
            .map(|point| point.1)
            .fold(f32::INFINITY, f32::min)
            .floor()
            .max(0.0) as u32;
        let max_x = corners
            .iter()
            .map(|point| point.0)
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil()
            .min(width as f32 - 1.0) as u32;
        let max_y = corners
            .iter()
            .map(|point| point.1)
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil()
            .min(height as f32 - 1.0) as u32;
        if min_x > max_x || min_y > max_y {
            continue;
        }
        let roi_width = max_x - min_x + 1;
        let roi_height = max_y - min_y + 1;
        let roi_pixels = (roi_width * roi_height) as usize;
        let mut aligned = AlignedFaceMasks {
            transform,
            x: min_x,
            y: min_y,
            width: roi_width,
            height: roi_height,
            face: vec![0; roi_pixels],
            skin: vec![0; roi_pixels],
            hair: vec![0; roi_pixels],
            eyes_and_brows: vec![0; roi_pixels],
            lips: vec![0; roi_pixels],
            clothes: vec![0; roi_pixels],
        };
        for source_y in min_y..=max_y {
            for source_x in min_x..=max_x {
                let (target_x, target_y) =
                    transform.source_to_target(source_x as f32, source_y as f32);
                if target_x < 0.0
                    || target_y < 0.0
                    || target_x > SIDE as f32 - 1.0
                    || target_y > SIDE as f32 - 1.0
                {
                    continue;
                }
                let crop_edge = target_x
                    .min(target_y)
                    .min(SIDE as f32 - 1.0 - target_x)
                    .min(SIDE as f32 - 1.0 - target_y);
                let feather = (crop_edge / 16.0).clamp(0.0, 1.0);
                let local_index = ((source_y - min_y) * roi_width + source_x - min_x) as usize;
                let face = class_membership(&classes, SIDE, SIDE, target_x, target_y, |class| {
                    (1..=13).contains(&class)
                }) * feather;
                // Skin Extended: facial skin + ears + nose + neck. The raw
                // CelebAMask-HQ skin class excludes those regions, which made
                // the preview look broken and left visible retouch boundaries.
                let skin = class_membership(&classes, SIDE, SIDE, target_x, target_y, |class| {
                    matches!(class, 1 | 7 | 8 | 10 | 14)
                }) * feather;
                let eyes = class_membership(&classes, SIDE, SIDE, target_x, target_y, |class| {
                    (2..=6).contains(&class)
                }) * feather;
                let lips = class_membership(&classes, SIDE, SIDE, target_x, target_y, |class| {
                    (11..=13).contains(&class)
                }) * feather;
                let clothes = class_membership(&classes, SIDE, SIDE, target_x, target_y, |class| {
                    class == 15 || class == 16
                }) * feather;
                let hair = class_membership(&classes, SIDE, SIDE, target_x, target_y, |class| {
                    class == 17 || class == 18
                }) * feather;
                let encode = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                aligned.face[local_index] = encode(face);
                aligned.skin[local_index] = encode(skin);
                aligned.eyes_and_brows[local_index] = encode(eyes);
                aligned.lips[local_index] = encode(lips);
                aligned.clothes[local_index] = encode(clothes);
                aligned.hair[local_index] = encode(hair);
            }
        }
        aligned.union_into(&mut masks, width);
        masks.aligned_faces.push(aligned);
        emit_work_progress(progress, face_index + 1, mask_work_total);
    }
    masks.face_count = masks.aligned_faces.len();
    if !masks.face.iter().any(|value| *value > 0.01) {
        return Err("BiSeNet không tạo được mask mặt hợp lệ sau alignment".to_string());
    }
    refine_semantic_masks(rgba, width, height, &mut masks, cancel)?;
    emit_work_progress(progress, mask_work_total, mask_work_total);
    Ok(masks)
}

pub fn models_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("IAI").join("models")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("iai")
            .join("models")
    } else {
        PathBuf::from("models")
    }
}

fn model_path(id: ModelId) -> PathBuf {
    let relative = PathBuf::from(id.directory()).join(id.default_file());
    let mut roots = Vec::new();
    // A release-local bundle is authoritative. User model directories remain
    // a development/override fallback, but cannot silently shadow the exact
    // artifacts shipped beside the executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            roots.push(parent.join("models"));
        }
    }
    roots.push(models_dir());
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("models"));
    }
    roots
        .iter()
        .map(|root| root.join(&relative))
        .find(|path| path.is_file())
        .unwrap_or_else(|| models_dir().join(relative))
}

pub fn model_metadata() -> Vec<ModelMetadata> {
    ModelId::ALL
        .iter()
        .copied()
        .map(ModelMetadata::for_id)
        .collect()
}

pub fn missing_required_models() -> Vec<ModelMetadata> {
    model_metadata()
        .into_iter()
        .filter(|m| m.required && !model_path(m.id).is_file())
        .collect()
}

pub fn manifest_template() -> Vec<ModelManifest> {
    ModelId::ALL
        .iter()
        .map(|id| ModelManifest {
            id: id.manifest_id().to_string(),
            file: id.default_file().to_string(),
            sha256: String::new(),
            source: "See docs/AI_MODELS.md".to_string(),
            license: "See upstream model license".to_string(),
            required: !matches!(id, ModelId::RealesrganRrdbX2 | ModelId::RealesrganRrdb),
        })
        .collect()
}

pub fn mask_cache_key(rgba: &[u8], width: u32, height: u32) -> u64 {
    // FNV-1a is quick and deterministic; this cache key is only an in-memory
    // invalidation token, not a security hash.
    const MASK_ALGORITHM_VERSION: u64 = 4;
    let mut h = 0xcbf29ce484222325u64
        ^ width as u64
        ^ ((height as u64) << 32)
        ^ MASK_ALGORITHM_VERSION.rotate_left(17);
    for b in rgba.iter().step_by((rgba.len() / 4096).max(1)) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn cancel_if_requested(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Đã hủy Auto Retouch".to_string())
    } else {
        Ok(())
    }
}

fn cancel_if_requested_optional(cancel: Option<&AtomicBool>) -> Result<(), String> {
    match cancel {
        Some(cancel) => cancel_if_requested(cancel),
        None => Ok(()),
    }
}

fn emit_work_progress(progress: Option<&dyn Fn(usize, usize)>, completed: usize, total: usize) {
    if let Some(progress) = progress {
        progress(completed.min(total), total.max(1));
    }
}

fn report(status: &Mutex<RetouchStatus>, stage: RetouchStage, fraction: f32, message: String) {
    if let Ok(mut s) = status.lock() {
        s.progress = RetouchProgress {
            stage,
            fraction: fraction.clamp(0.0, 1.0),
            message,
        };
    }
}

fn stage_done(benchmark: &mut RetouchBenchmark, stage: RetouchStage, started: Instant) {
    benchmark.timings.push(StageTiming {
        stage: stage.label().to_string(),
        millis: started.elapsed().as_millis(),
    });
}

fn clamp_amount(v: u8, overall: u8) -> f32 {
    (v as f32 / 100.0) * (overall as f32 / 100.0)
}

fn semantic_mask_preview(masks: &SemanticMasks) -> Vec<u8> {
    let mut out = vec![0u8; masks.face.len() * 4];
    for index in 0..masks.face.len() {
        // Draw broad regions first and smaller semantic features last. Alpha
        // remains soft so the retouched image is visible below the overlay.
        let regions = [
            (masks.background[index], [38.0, 82.0, 142.0]),
            (masks.accessories[index], [154.0, 92.0, 230.0]),
            (masks.face[index], [235.0, 70.0, 70.0]),
            (masks.skin[index], [255.0, 92.0, 92.0]),
            (masks.hair[index], [72.0, 150.0, 255.0]),
            (masks.clothes[index], [255.0, 196.0, 72.0]),
            (masks.eyes_and_brows[index], [72.0, 255.0, 126.0]),
            (masks.lips[index], [255.0, 72.0, 210.0]),
        ];
        let mut rgb = [0.0f32; 3];
        let mut alpha = 0.0f32;
        for (weight, colour) in regions {
            let weight = weight.clamp(0.0, 1.0);
            if weight <= 0.0 {
                continue;
            }
            for channel in 0..3 {
                rgb[channel] = rgb[channel] * (1.0 - weight) + colour[channel] * weight;
            }
            alpha = alpha.max(weight);
        }
        for channel in 0..3 {
            out[index * 4 + channel] = rgb[channel].clamp(0.0, 255.0).round() as u8;
        }
        out[index * 4 + 3] = (alpha.clamp(0.0, 1.0) * 176.0).round() as u8;
    }
    out
}

fn bounded_amount(v: u8, overall: u8, safety_ceiling: f32) -> f32 {
    clamp_amount(v, overall).min(safety_ceiling)
}

fn cpu_face_mask(rgba: &[u8], width: u32, height: u32) -> Vec<f32> {
    let mut mask = vec![0.0; width as usize * height as usize];
    if width == 0 || height == 0 {
        return mask;
    }
    // Conservative fallback: an upper-center oval gated by skin-like colour.
    // It is intentionally soft and never used outside the original pixels.
    let cx = width as f32 * 0.5;
    let cy = height as f32 * 0.43;
    let rx = (width as f32 * 0.22).max(16.0);
    let ry = (height as f32 * 0.30).max(20.0);
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) as usize;
            let dx = (x as f32 - cx) / rx;
            let dy = (y as f32 - cy) / ry;
            let oval = (1.0 - (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let p = &rgba[i * 4..i * 4 + 4];
            let maxc = p[0].max(p[1]).max(p[2]) as f32;
            let skin_gate = if maxc > 25.0
                && p[0] >= p[1].saturating_sub(12)
                && p[1] >= p[2].saturating_sub(18)
            {
                1.0
            } else {
                0.55
            };
            mask[i] = (oval * skin_gate).sqrt();
        }
    }
    feather_mask(
        &mut mask,
        width,
        height,
        ((width.max(height) / 512).max(1)) as u32 * 2,
    );
    mask
}

fn feather_mask(mask: &mut [f32], width: u32, height: u32, radius: u32) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }
    // Separable prefix-sum blur. The previous O(pixels * radius^2) loop could
    // take minutes on a 24 MP image even before any retouch stage was reached.
    let w = width as usize;
    let h = height as usize;
    let r = radius as usize;
    let source = mask.to_vec();
    let mut horizontal = vec![0.0f32; source.len()];
    let mut prefix = vec![0.0f64; w.max(h) + 1];
    for y in 0..h {
        prefix[0] = 0.0;
        for x in 0..w {
            prefix[x + 1] = prefix[x] + source[y * w + x] as f64;
        }
        for x in 0..w {
            let lo = x.saturating_sub(r);
            let hi = (x + r + 1).min(w);
            horizontal[y * w + x] = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
        }
    }
    for x in 0..w {
        prefix[0] = 0.0;
        for y in 0..h {
            prefix[y + 1] = prefix[y] + horizontal[y * w + x] as f64;
        }
        for y in 0..h {
            let lo = y.saturating_sub(r);
            let hi = (y + r + 1).min(h);
            mask[y * w + x] = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
        }
    }
}

fn morph_mask(mask: &mut Vec<f32>, width: u32, height: u32, radius: u32, expand: bool) {
    if radius == 0 || width == 0 || height == 0 || mask.is_empty() {
        return;
    }
    let source = mask.clone();
    let Some(first) = source.iter().position(|value| *value > 0.005) else {
        return;
    };
    let mut min_x = first as u32 % width;
    let mut max_x = min_x;
    let mut min_y = first as u32 / width;
    let mut max_y = min_y;
    for (index, value) in source.iter().enumerate().skip(first + 1) {
        if *value <= 0.005 {
            continue;
        }
        let x = index as u32 % width;
        let y = index as u32 / width;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    if expand {
        min_x = min_x.saturating_sub(radius);
        min_y = min_y.saturating_sub(radius);
        max_x = max_x.saturating_add(radius).min(width - 1);
        max_y = max_y.saturating_add(radius).min(height - 1);
    }
    let mut result = vec![0.0f32; source.len()];
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let mut extreme = if expand { 0.0f32 } else { 1.0f32 };
            let start_y = y.saturating_sub(radius);
            let end_y = y.saturating_add(radius).min(height - 1);
            let start_x = x.saturating_sub(radius);
            let end_x = x.saturating_add(radius).min(width - 1);
            for sample_y in start_y..=end_y {
                for sample_x in start_x..=end_x {
                    let sample = source[(sample_y * width + sample_x) as usize];
                    extreme = if expand {
                        extreme.max(sample)
                    } else {
                        extreme.min(sample)
                    };
                }
            }
            // Contracting a mask at a canvas edge treats pixels outside the
            // canvas as empty instead of pinning the mask to that edge.
            if !expand
                && (x < radius
                    || y < radius
                    || x.saturating_add(radius) >= width
                    || y.saturating_add(radius) >= height)
            {
                extreme = 0.0;
            }
            result[(y * width + x) as usize] = extreme;
        }
    }
    *mask = result;
}

fn edge_refine_mask(
    rgba: &[u8],
    hard_mask: &[f32],
    soft_mask: &mut [f32],
    width: u32,
    height: u32,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    const RADIUS: i32 = 2;
    if rgba.len() != soft_mask.len() * 4 || hard_mask.len() != soft_mask.len() {
        return Ok(());
    }
    let blurred = soft_mask.to_vec();
    for y in 0..height as i32 {
        if y % 64 == 0 {
            cancel_if_requested_optional(cancel)?;
        }
        for x in 0..width as i32 {
            let index = (y as u32 * width + x as u32) as usize;
            let alpha = blurred[index];
            if !(0.005..=0.995).contains(&alpha) {
                continue;
            }
            let base = &rgba[index * 4..index * 4 + 3];
            let mut weighted_mask = 0.0f32;
            let mut total_weight = 0.0f32;
            for dy in -RADIUS..=RADIUS {
                let sample_y = (y + dy).clamp(0, height as i32 - 1) as u32;
                for dx in -RADIUS..=RADIUS {
                    let sample_x = (x + dx).clamp(0, width as i32 - 1) as u32;
                    let sample_index = (sample_y * width + sample_x) as usize;
                    let sample = &rgba[sample_index * 4..sample_index * 4 + 3];
                    let colour_distance = (0..3)
                        .map(|channel| {
                            let delta = base[channel] as f32 - sample[channel] as f32;
                            delta * delta
                        })
                        .sum::<f32>();
                    let spatial_distance = (dx * dx + dy * dy) as f32;
                    let weight = 1.0 / (1.0 + colour_distance / 1800.0 + spatial_distance * 0.20);
                    weighted_mask += hard_mask[sample_index] * weight;
                    total_weight += weight;
                }
            }
            let guided = weighted_mask / total_weight.max(f32::EPSILON);
            soft_mask[index] = (alpha * 0.35 + guided * 0.65).clamp(0.0, 1.0);
        }
    }
    Ok(())
}

fn postprocess_mask(
    rgba: &[u8],
    mask: &mut Vec<f32>,
    width: u32,
    height: u32,
    morphology: i32,
    feather_radius: u32,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    if !mask.iter().any(|value| *value > 0.005) {
        return Ok(());
    }
    let radius = morphology.unsigned_abs();
    if radius > 0 {
        morph_mask(mask, width, height, radius, morphology > 0);
    }
    let hard_mask = mask.clone();
    feather_mask(mask, width, height, feather_radius);
    edge_refine_mask(rgba, &hard_mask, mask, width, height, cancel)
}

fn refine_semantic_masks(
    rgba: &[u8],
    width: u32,
    height: u32,
    masks: &mut SemanticMasks,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    // Roughly 2 px at the user's 960x1280 test image and no more than 6 px on
    // very large files. Feature masks intentionally use a smaller radius.
    let radius = ((width.max(height) + 639) / 640).clamp(1, 6);
    let morph = ((radius + 1) / 2).max(1) as i32;
    postprocess_mask(rgba, &mut masks.face, width, height, morph, radius, cancel)?;
    postprocess_mask(rgba, &mut masks.skin, width, height, morph, radius, cancel)?;
    postprocess_mask(rgba, &mut masks.hair, width, height, -morph, radius, cancel)?;
    postprocess_mask(
        rgba,
        &mut masks.eyes_and_brows,
        width,
        height,
        1,
        radius.min(2),
        cancel,
    )?;
    postprocess_mask(
        rgba,
        &mut masks.lips,
        width,
        height,
        1,
        radius.min(2),
        cancel,
    )?;
    postprocess_mask(
        rgba,
        &mut masks.clothes,
        width,
        height,
        morph,
        radius,
        cancel,
    )?;
    postprocess_mask(
        rgba,
        &mut masks.background,
        width,
        height,
        0,
        radius,
        cancel,
    )?;
    postprocess_mask(
        rgba,
        &mut masks.accessories,
        width,
        height,
        0,
        radius,
        cancel,
    )?;
    Ok(())
}

fn blend_rgba_masked(base: &[u8], effect: &[u8], mask: &[f32], amount: f32) -> Vec<u8> {
    let mut out = base.to_vec();
    for (i, dst) in out.chunks_exact_mut(4).enumerate() {
        let a = (mask.get(i).copied().unwrap_or(0.0) * amount).clamp(0.0, 1.0);
        let src = &effect[i * 4..i * 4 + 4];
        for c in 0..3 {
            dst[c] = (dst[c] as f32 * (1.0 - a) + src[c] as f32 * a).round() as u8;
        }
        dst[3] = base[i * 4 + 3];
    }
    out
}

fn blend_uniform(base: &[u8], effect: &[u8], amount: f32) -> Vec<u8> {
    let mut out = base.to_vec();
    let a = amount.clamp(0.0, 1.0);
    for (i, dst) in out.chunks_exact_mut(4).enumerate() {
        let src = &effect[i * 4..i * 4 + 4];
        for c in 0..3 {
            dst[c] = (dst[c] as f32 * (1.0 - a) + src[c] as f32 * a).round() as u8;
        }
    }
    out
}

fn colour_match_masked(base: &[u8], effect: &mut [u8], mask: &[f32], max_shift: f32) {
    if base.len() != effect.len() || base.len() != mask.len() * 4 {
        return;
    }
    let mut base_sum = [0.0f64; 3];
    let mut effect_sum = [0.0f64; 3];
    let mut total_weight = 0.0f64;
    for (index, weight) in mask.iter().copied().enumerate() {
        let weight = weight.clamp(0.0, 1.0) as f64;
        if weight <= 0.005 {
            continue;
        }
        total_weight += weight;
        for channel in 0..3 {
            base_sum[channel] += base[index * 4 + channel] as f64 * weight;
            effect_sum[channel] += effect[index * 4 + channel] as f64 * weight;
        }
    }
    if total_weight <= f64::EPSILON {
        return;
    }
    let shifts = std::array::from_fn::<f32, 3, _>(|channel| {
        ((base_sum[channel] - effect_sum[channel]) / total_weight) as f32
    })
    .map(|shift| shift.clamp(-max_shift, max_shift));
    for (index, weight) in mask.iter().copied().enumerate() {
        if weight <= 0.005 {
            continue;
        }
        for channel in 0..3 {
            effect[index * 4 + channel] = (effect[index * 4 + channel] as f32 + shifts[channel])
                .clamp(0.0, 255.0)
                .round() as u8;
        }
    }
}

fn box_blur_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    radius: u32,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    if radius == 0 || width == 0 || height == 0 {
        return Ok(rgba.to_vec());
    }
    let w = width as usize;
    let h = height as usize;
    let r = radius as usize;
    let mut horizontal = rgba.to_vec();
    let mut out = rgba.to_vec();

    // Sliding-window box blur has constant work per pixel and samples across
    // the whole image, so tile boundaries can no longer overwrite each other
    // or create seams.
    for y in 0..h {
        if y % 64 == 0 {
            cancel_if_requested(cancel)?;
        }
        for c in 0..3 {
            let mut sum = 0u32;
            let initial_hi = r.min(w.saturating_sub(1));
            for x in 0..=initial_hi {
                sum += rgba[(y * w + x) * 4 + c] as u32;
            }
            for x in 0..w {
                let lo = x.saturating_sub(r);
                let hi = (x + r).min(w - 1);
                if x > 0 {
                    let old_lo = (x - 1).saturating_sub(r);
                    let old_hi = (x - 1 + r).min(w - 1);
                    if lo > old_lo {
                        sum -= rgba[(y * w + old_lo) * 4 + c] as u32;
                    }
                    if hi > old_hi {
                        sum += rgba[(y * w + hi) * 4 + c] as u32;
                    }
                }
                horizontal[(y * w + x) * 4 + c] = (sum / (hi - lo + 1) as u32) as u8;
            }
        }
    }

    for x in 0..w {
        if x % 64 == 0 {
            cancel_if_requested(cancel)?;
        }
        for c in 0..3 {
            let mut sum = 0u32;
            let initial_hi = r.min(h.saturating_sub(1));
            for y in 0..=initial_hi {
                sum += horizontal[(y * w + x) * 4 + c] as u32;
            }
            for y in 0..h {
                let lo = y.saturating_sub(r);
                let hi = (y + r).min(h - 1);
                if y > 0 {
                    let old_lo = (y - 1).saturating_sub(r);
                    let old_hi = (y - 1 + r).min(h - 1);
                    if lo > old_lo {
                        sum -= horizontal[(old_lo * w + x) * 4 + c] as u32;
                    }
                    if hi > old_hi {
                        sum += horizontal[(hi * w + x) * 4 + c] as u32;
                    }
                }
                out[(y * w + x) * 4 + c] = (sum / (hi - lo + 1) as u32) as u8;
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
struct ColorDiagnosis {
    cast: &'static str,
    gains: [f32; 3],
    confidence: f32,
    mean_linear_luma: f32,
}

#[inline]
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn analyse_color_cast(rgba: &[u8], width: u32, height: u32) -> ColorDiagnosis {
    let neutral = || ColorDiagnosis {
        cast: "neutral",
        gains: [1.0; 3],
        confidence: 0.0,
        mean_linear_luma: 0.18,
    };
    if width < 3 || height < 3 || rgba.len() != width as usize * height as usize * 4 {
        return neutral();
    }
    let linear_lut =
        std::array::from_fn::<f32, 256, _>(|value| srgb_to_linear(value as f32 / 255.0));
    let pixels = width as usize * height as usize;
    let step = ((pixels / 120_000).max(1) as f32).sqrt().floor() as usize;
    let step = step.max(1);
    let mut means = [0.0f64; 3];
    let mut edge_fourth = [0.0f64; 3];
    let mut samples = 0usize;
    let mut edges = 0usize;
    let w = width as usize;
    let h = height as usize;
    for y in (0..h - 2).step_by(step) {
        for x in (0..w - 2).step_by(step) {
            let index = (y * w + x) * 4;
            if rgba[index + 3] == 0 {
                continue;
            }
            let right = (y * w + x + 2) * 4;
            let below = ((y + 2) * w + x) * 4;
            for channel in 0..3 {
                let value = linear_lut[rgba[index + channel] as usize] as f64;
                means[channel] += value;
                if rgba[right + 3] != 0 && rgba[below + 3] != 0 {
                    let dx = (value - linear_lut[rgba[right + channel] as usize] as f64).abs();
                    let dy = (value - linear_lut[rgba[below + channel] as usize] as f64).abs();
                    edge_fourth[channel] += dx.powi(4) + dy.powi(4);
                }
            }
            samples += 1;
            edges += usize::from(rgba[right + 3] != 0 && rgba[below + 3] != 0);
        }
    }
    if samples < 16 {
        return neutral();
    }
    let means = means.map(|sum| (sum / samples as f64).max(1e-6) as f32);
    let edge = edge_fourth.map(|sum| {
        if edges > 0 {
            (sum / (edges * 2) as f64).max(1e-12).powf(0.25) as f32
        } else {
            0.0
        }
    });
    // Gray-edge is resilient to a large coloured wall; a smaller shades-of-
    // gray component helps when the image has few strong edges. Both estimate
    // illuminant chromaticity, not an arbitrary per-channel visual effect.
    let illuminant = std::array::from_fn::<f32, 3, _>(|channel| {
        if edge[channel] > 1e-5 {
            (0.72 * edge[channel].ln() + 0.28 * means[channel].ln()).exp()
        } else {
            means[channel]
        }
    });
    let geometric_mean = (illuminant[0] * illuminant[1] * illuminant[2])
        .max(1e-12)
        .powf(1.0 / 3.0);
    let raw_gains = illuminant.map(|value| (geometric_mean / value.max(1e-6)).clamp(0.72, 1.40));
    let cast_strength = raw_gains
        .iter()
        .map(|gain| gain.ln().abs())
        .fold(0.0f32, f32::max);
    // Avoid "correcting" tiny statistical differences in an already neutral
    // image. A clear 7%+ cast receives the full estimate.
    let correction_strength = ((cast_strength - 0.008) / 0.06).clamp(0.0, 1.0);
    let gains = raw_gains.map(|gain| gain.powf(correction_strength));
    let confidence = (cast_strength / 0.24).clamp(0.0, 1.0);
    let cast = if confidence < 0.10 {
        "neutral"
    } else if illuminant[1] > illuminant[0] * 1.08 && illuminant[1] > illuminant[2] * 1.08 {
        "green"
    } else if illuminant[2] > illuminant[0] * 1.08 && illuminant[2] > illuminant[1] * 1.08 {
        "blue"
    } else if illuminant[0] > illuminant[1] * 1.08 && illuminant[0] > illuminant[2] * 1.08 {
        "red"
    } else if illuminant[2] < illuminant[0].min(illuminant[1]) * 0.90 {
        "yellow/warm"
    } else {
        "mixed"
    };
    ColorDiagnosis {
        cast,
        gains,
        confidence,
        mean_linear_luma: 0.2126 * means[0] + 0.7152 * means[1] + 0.0722 * means[2],
    }
}

/// Combines learned IAT luminance with an explicit color-constancy estimate.
/// IAT's chroma is intentionally discarded: the validated exposure checkpoint
/// can brighten a scene but, on green-cast inputs, its raw RGB output may make
/// that cast stronger. This keeps the neural exposure decision while making
/// white balance measurable and deterministic.
fn adaptive_color_effect(
    rgba: &[u8],
    width: u32,
    height: u32,
    learned: Option<&[u8]>,
    skin_mask: &[f32],
    look: ColorLook,
) -> (Vec<u8>, ColorDiagnosis) {
    let diagnosis = analyse_color_cast(rgba, width, height);
    if rgba.len() != width as usize * height as usize * 4 {
        return (rgba.to_vec(), diagnosis);
    }
    let learned = learned.filter(|values| values.len() == rgba.len());
    let linear_lut =
        std::array::from_fn::<f32, 256, _>(|value| srgb_to_linear(value as f32 / 255.0));
    let (saturation, skin_saturation, contrast, lift, exposure, style_gains) = match look {
        ColorLook::Fresh => (1.13, 1.06, 1.045, 0.015, 1.04, [1.01, 1.0, 1.005]),
        ColorLook::Natural => (1.03, 1.02, 1.02, 0.0, 1.0, [1.0; 3]),
        ColorLook::Warm => (1.10, 1.05, 1.035, 0.010, 1.02, [1.04, 1.0, 0.96]),
        ColorLook::Cool => (1.08, 1.04, 1.03, 0.008, 1.01, [0.98, 1.0, 1.04]),
    };
    let fallback_exposure = (0.20 / diagnosis.mean_linear_luma.max(0.025))
        .sqrt()
        .clamp(0.86, 1.28);
    let mut out = rgba.to_vec();
    out.par_chunks_exact_mut(4)
        .enumerate()
        .for_each(|(index, target)| {
            let source = &rgba[index * 4..index * 4 + 4];
            if source[3] == 0 {
                return;
            }
            let source_linear = [
                linear_lut[source[0] as usize],
                linear_lut[source[1] as usize],
                linear_lut[source[2] as usize],
            ];
            let source_luma =
                0.2126 * source_linear[0] + 0.7152 * source_linear[1] + 0.0722 * source_linear[2];
            let desired_luma = if let Some(learned) = learned {
                let learned = &learned[index * 4..index * 4 + 4];
                let learned_luma = 0.2126 * linear_lut[learned[0] as usize]
                    + 0.7152 * linear_lut[learned[1] as usize]
                    + 0.0722 * linear_lut[learned[2] as usize];
                let bounded = learned_luma.clamp(source_luma * 0.65, source_luma * 1.80 + 0.004);
                ((source_luma + 1e-5).ln() * 0.45 + (bounded + 1e-5).ln() * 0.55).exp()
            } else {
                source_luma * fallback_exposure
            } * exposure;
            let mut balanced = std::array::from_fn::<f32, 3, _>(|channel| {
                source_linear[channel] * diagnosis.gains[channel] * style_gains[channel]
            });
            let balanced_luma = 0.2126 * balanced[0] + 0.7152 * balanced[1] + 0.0722 * balanced[2];
            let luma_scale = (desired_luma / (balanced_luma + 1e-5)).clamp(0.55, 1.85);
            balanced.iter_mut().for_each(|value| *value *= luma_scale);
            let mut srgb = balanced.map(linear_to_srgb);
            let luma = 0.2126 * srgb[0] + 0.7152 * srgb[1] + 0.0722 * srgb[2];
            let skin = skin_mask.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let local_saturation = saturation * (1.0 - skin) + skin_saturation * skin;
            for channel in 0..3 {
                srgb[channel] = (luma + (srgb[channel] - luma) * local_saturation)
                    .mul_add(contrast, 0.5 + lift - 0.5 * contrast)
                    .clamp(0.0, 1.0);
                target[channel] = (srgb[channel] * 255.0).round() as u8;
            }
            target[3] = source[3];
        });
    (out, diagnosis)
}

fn unsharp_effect(
    rgba: &[u8],
    width: u32,
    height: u32,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let blur = box_blur_rgba(rgba, width, height, 1, cancel)?;
    let mut out = rgba.to_vec();
    for (i, p) in out.chunks_exact_mut(4).enumerate() {
        for c in 0..3 {
            let v =
                rgba[i * 4 + c] as f32 + (rgba[i * 4 + c] as f32 - blur[i * 4 + c] as f32) * 0.7;
            p[c] = v.clamp(0.0, 255.0) as u8;
        }
    }
    Ok(out)
}

fn surface_smooth_effect(
    rgba: &[u8],
    width: u32,
    height: u32,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    edge_aware_smooth_effect(
        rgba,
        width,
        height,
        ((width.max(height) as f32 / 1400.0).round() as u32).clamp(1, 2),
        42.0,
        cancel,
    )
}

fn edge_aware_smooth_effect(
    rgba: &[u8],
    width: u32,
    height: u32,
    radius: u32,
    threshold: f32,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let blur = box_blur_rgba(rgba, width, height, radius, cancel)?;
    let mut out = rgba.to_vec();
    for (i, dst) in out.chunks_exact_mut(4).enumerate() {
        let src = &rgba[i * 4..i * 4 + 4];
        let soft = &blur[i * 4..i * 4 + 4];
        let distance = (src[0] as f32 - soft[0] as f32).abs()
            + (src[1] as f32 - soft[1] as f32).abs()
            + (src[2] as f32 - soft[2] as f32).abs();
        let smooth_weight = (1.0 - distance / threshold.max(1.0)).clamp(0.0, 1.0);
        for c in 0..3 {
            dst[c] = (src[c] as f32 * (1.0 - smooth_weight) + soft[c] as f32 * smooth_weight)
                .round() as u8;
        }
    }
    Ok(out)
}

fn skin_mask(rgba: &[u8], face_mask: &[f32]) -> Vec<f32> {
    rgba.chunks_exact(4)
        .enumerate()
        .map(|(i, p)| {
            let r = p[0] as f32;
            let g = p[1] as f32;
            let b = p[2] as f32;
            let maxc = r.max(g).max(b);
            let minc = r.min(g).min(b);
            let chroma = maxc - minc;
            let warm = ((r - b + 30.0) / 70.0).clamp(0.0, 1.0);
            let red_over_green = ((r - g + 18.0) / 42.0).clamp(0.0, 1.0);
            let not_gray = (chroma / 22.0).clamp(0.25, 1.0);
            face_mask.get(i).copied().unwrap_or(0.0)
                * warm
                * red_over_green
                * not_gray
                * (p[3] as f32 / 255.0)
        })
        .collect()
}

fn face_bounds(mask: &[f32], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            if mask[(y * width + x) as usize] >= 0.20 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                found = true;
            }
        }
    }
    found.then_some((min_x, min_y, max_x, max_y))
}

fn ellipse_region_mask(
    width: u32,
    height: u32,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    face_mask: &[f32],
) -> Vec<f32> {
    let mut out = vec![0.0; width as usize * height as usize];
    for y in 0..height {
        for x in 0..width {
            let dx = (x as f32 - cx) / rx.max(1.0);
            let dy = (y as f32 - cy) / ry.max(1.0);
            let soft = (1.0 - dx * dx - dy * dy).clamp(0.0, 1.0).sqrt();
            let i = (y * width + x) as usize;
            out[i] = soft * face_mask.get(i).copied().unwrap_or(0.0);
        }
    }
    out
}

fn feature_masks(face_mask: &[f32], width: u32, height: u32) -> (Vec<f32>, Vec<f32>) {
    let Some((x0, y0, x1, y1)) = face_bounds(face_mask, width, height) else {
        return (vec![0.0; face_mask.len()], vec![0.0; face_mask.len()]);
    };
    let fw = (x1 - x0 + 1) as f32;
    let fh = (y1 - y0 + 1) as f32;
    let eye_y = y0 as f32 + fh * 0.43;
    let mut eyes = ellipse_region_mask(
        width,
        height,
        x0 as f32 + fw * 0.33,
        eye_y,
        fw * 0.16,
        fh * 0.09,
        face_mask,
    );
    let right_eye = ellipse_region_mask(
        width,
        height,
        x0 as f32 + fw * 0.67,
        eye_y,
        fw * 0.16,
        fh * 0.09,
        face_mask,
    );
    for (left, right) in eyes.iter_mut().zip(right_eye) {
        *left = left.max(right);
    }
    let lips = ellipse_region_mask(
        width,
        height,
        x0 as f32 + fw * 0.5,
        y0 as f32 + fh * 0.70,
        fw * 0.19,
        fh * 0.085,
        face_mask,
    );
    (eyes, lips)
}

fn selective_detail_mask(
    rgba: &[u8],
    face_mask: &[f32],
    skin: &[f32],
    width: u32,
    height: u32,
) -> Vec<f32> {
    let mut out = vec![0.0; width as usize * height as usize];
    if width < 2 || height < 2 {
        return out;
    }
    let bounds = face_bounds(face_mask, width, height);
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let i = (y * width + x) as usize;
            let left = ((y * width + x - 1) * 4) as usize;
            let right = ((y * width + x + 1) * 4) as usize;
            let up = (((y - 1) * width + x) * 4) as usize;
            let down = (((y + 1) * width + x) * 4) as usize;
            let luma = |at: usize| {
                0.2126 * rgba[at] as f32
                    + 0.7152 * rgba[at + 1] as f32
                    + 0.0722 * rgba[at + 2] as f32
            };
            let edge = ((luma(left) - luma(right)).abs() + (luma(up) - luma(down)).abs()) / 72.0;
            let subject_weight = if let Some((x0, y0, x1, y1)) = bounds {
                let fw = (x1 - x0 + 1) as f32;
                let fh = (y1 - y0 + 1) as f32;
                let cx = x0 as f32 + fw * 0.5;
                let upper = y0.saturating_sub((fh * 0.18) as u32) as f32;
                let lower = (y1 as f32 + fh * 1.35).min(height as f32);
                let horizontal = (1.0 - (x as f32 - cx).abs() / (fw * 0.85)).clamp(0.0, 1.0);
                let vertical = if (upper..=lower).contains(&(y as f32)) {
                    1.0
                } else {
                    0.0
                };
                horizontal * vertical
            } else {
                0.35
            };
            out[i] = edge.clamp(0.0, 1.0)
                * subject_weight
                * (1.0 - skin.get(i).copied().unwrap_or(0.0) * 0.9);
        }
    }
    out
}

fn change_stats(before: &[u8], after: &[u8]) -> (usize, f32) {
    let mut changed = 0usize;
    let mut total_delta = 0u64;
    let mut samples = 0usize;
    for (a, b) in before.chunks_exact(4).zip(after.chunks_exact(4)) {
        let delta = (0..3).map(|c| a[c].abs_diff(b[c]) as u64).sum::<u64>();
        if delta > 0 {
            changed += 1;
        }
        total_delta += delta;
        samples += 3;
    }
    let mean = if samples == 0 {
        0.0
    } else {
        total_delta as f32 / samples as f32
    };
    (changed, mean)
}

fn estimate_noise_sigma(rgba: &[u8], width: u32, height: u32) -> f32 {
    if width < 2 || height < 2 || rgba.len() != width as usize * height as usize * 4 {
        return 0.0;
    }
    // A diagonal 2x2 high-pass cancels flat fields and linear gradients. Its
    // median absolute response is robust against normal image edges, while
    // still reacting to sensor noise, JPEG blocks and halftone texture.
    let target_samples = 16_384usize;
    let pixels = width as usize * height as usize;
    let step = ((pixels / target_samples).max(1) as f32).sqrt().floor() as usize;
    let step = step.max(1);
    let luma = |x: usize, y: usize| {
        let index = (y * width as usize + x) * 4;
        0.2126 * rgba[index] as f32
            + 0.7152 * rgba[index + 1] as f32
            + 0.0722 * rgba[index + 2] as f32
    };
    let mut responses = Vec::with_capacity(target_samples + width as usize);
    for y in (0..height as usize - 1).step_by(step) {
        for x in (0..width as usize - 1).step_by(step) {
            let response =
                (luma(x, y) + luma(x + 1, y + 1) - luma(x + 1, y) - luma(x, y + 1)).abs() * 0.5;
            responses.push(response);
        }
    }
    if responses.is_empty() {
        return 0.0;
    }
    responses.sort_unstable_by(f32::total_cmp);
    (responses[responses.len() / 2] / 0.6745).clamp(0.0, 64.0)
}

fn change_stats_masked(before: &[u8], after: &[u8], mask: &[f32]) -> (usize, f32) {
    if before.len() != after.len() || before.len() != mask.len() * 4 {
        return (0, 0.0);
    }
    let mut changed_pixels = 0usize;
    let mut total_delta = 0u64;
    let mut included_pixels = 0usize;
    for (index, weight) in mask.iter().copied().enumerate() {
        if weight <= 0.01 {
            continue;
        }
        included_pixels += 1;
        let mut changed = false;
        for channel in 0..3 {
            let delta = before[index * 4 + channel].abs_diff(after[index * 4 + channel]) as u64;
            changed |= delta != 0;
            total_delta += delta;
        }
        changed_pixels += usize::from(changed);
    }
    let mean = if included_pixels == 0 {
        0.0
    } else {
        total_delta as f32 / (included_pixels * 3) as f32
    };
    (changed_pixels, mean)
}

fn run_pipeline(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    config: RetouchConfig,
    status: Arc<Mutex<RetouchStatus>>,
    cancel: Arc<AtomicBool>,
    mask_cache: Arc<Mutex<HashMap<u64, SemanticMasks>>>,
) -> Result<RetouchResult, String> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return Err("Ảnh đầu vào không hợp lệ".to_string());
    }
    let started_total = Instant::now();
    let mut benchmark = RetouchBenchmark::default();
    benchmark.white_balance_gains = [1.0; 3];
    benchmark.detected_color_cast = "not-run".to_string();
    let mut warnings = Vec::new();
    let mut used_models: Vec<&'static str> = Vec::new();
    let mut cpu_fallback_stages: Vec<&'static str> = Vec::new();
    let prefer_gpu = config.prefer_gpu && crate::core::hw::ai_gpu_candidate();
    let mut used_directml = false;
    let needs_face_masks = config.needs_face_masks() || config.preview_masks;
    let needs_body_masks = config.needs_body_masks();
    let key = mask_cache_key(&rgba, width, height)
        ^ if needs_face_masks { 0xface_0001 } else { 0 }
        ^ if needs_body_masks { 0xb0d1_0002 } else { 0 };

    report(
        &status,
        RetouchStage::Mask,
        0.05,
        "Tạo mask pixel-aligned…".to_string(),
    );
    let t = Instant::now();
    let cached_masks = (needs_face_masks || needs_body_masks)
        .then(|| {
            mask_cache
                .lock()
                .ok()
                .and_then(|cache| cache.get(&key).cloned())
        })
        .flatten();
    let used_mask_cache = cached_masks.is_some();
    let semantic_masks = if let Some(masks) = cached_masks {
        masks
    } else if !needs_face_masks && !needs_body_masks {
        report(
            &status,
            RetouchStage::Mask,
            0.15,
            "Bỏ qua mask: chỉ chạy stage toàn ảnh".to_string(),
        );
        SemanticMasks::empty(width, height)
    } else {
        let mut masks = SemanticMasks::empty(width, height);
        let mut body_masks_ready = !needs_body_masks;
        let mut face_masks_ready = !needs_face_masks;
        if needs_body_masks {
            let body = LocalOnnxRunner::with_gpu_preference(ModelId::BodyParsing, prefer_gpu);
            if body.available() {
                report(
                    &status,
                    RetouchStage::Mask,
                    0.07,
                    "MediaPipe: tách nền, tóc, da cơ thể và quần áo…".to_string(),
                );
                match body.run_body_parsing_masks(&rgba, width, height, Some(&cancel)) {
                    Ok(body_masks) => {
                        masks.merge_max(body_masks);
                        body_masks_ready = true;
                        used_directml |= body.used_directml();
                        used_models.push("MediaPipe 6-class full-image parsing");
                    }
                    Err(error) => {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(error);
                        }
                        warnings.push(format!(
                            "Body parsing chạy lỗi ({error}) — tóc/quần áo/nền không dùng mask đoán CPU"
                        ));
                        cpu_fallback_stages.push("body-mask unavailable");
                    }
                }
            } else {
                warnings.push(
                    "Thiếu hoặc sai checksum MediaPipe Selfie Multiclass — tóc/quần áo/nền không dùng mask đoán CPU"
                        .to_string(),
                );
                cpu_fallback_stages.push("body-mask unavailable");
            }
        }

        if needs_face_masks {
            let detector = LocalOnnxRunner::with_gpu_preference(ModelId::FaceDetector, prefer_gpu);
            let parser = LocalOnnxRunner::with_gpu_preference(ModelId::Bisenet, prefer_gpu);
            if detector.available() && parser.available() {
                let mask_progress = |completed: usize, total: usize| {
                    let ratio = completed as f32 / total.max(1) as f32;
                    let message = if completed < total {
                        format!("BiSeNet face {completed}/{}", total.saturating_sub(1))
                    } else {
                        "Refine + feather mask về ảnh gốc".to_string()
                    };
                    report(&status, RetouchStage::Mask, 0.05 + ratio * 0.10, message);
                };
                match generate_aligned_semantic_masks(
                    &rgba,
                    width,
                    height,
                    &detector,
                    &parser,
                    Some(&cancel),
                    Some(&mask_progress),
                ) {
                    Ok(face_masks) => {
                        masks.merge_max(face_masks);
                        face_masks_ready = true;
                        used_directml |= detector.used_directml() || parser.used_directml();
                        used_models.push("YuNet detector + landmarks");
                        used_models.push("BiSeNet 19-class face parsing");
                    }
                    Err(error) => {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(error);
                        }
                        warnings.push(format!(
                            "Face detection/parsing chạy lỗi ({error}) — dùng mask mặt CPU"
                        ));
                        masks.merge_max(SemanticMasks::cpu_fallback(&rgba, width, height));
                        cpu_fallback_stages.push("face-mask");
                    }
                }
            } else {
                let mut missing = Vec::new();
                if !detector.available() {
                    missing.push(ModelId::FaceDetector.display_name());
                }
                if !parser.available() {
                    missing.push(ModelId::Bisenet.display_name());
                }
                warnings.push(format!(
                    "Thiếu hoặc sai checksum {} — dùng mask mặt CPU",
                    missing.join(" + ")
                ));
                masks.merge_max(SemanticMasks::cpu_fallback(&rgba, width, height));
                cpu_fallback_stages.push("face-mask");
            }
        }
        if masks.ai_generated && body_masks_ready && face_masks_ready {
            let entry_bytes = masks.approximate_bytes();
            if entry_bytes <= MAX_MASK_CACHE_BYTES {
                if let Ok(mut cache) = mask_cache.lock() {
                    let retained_bytes = cache.values().fold(0usize, |total, cached| {
                        total.saturating_add(cached.approximate_bytes())
                    });
                    if cache.len() >= 8
                        || retained_bytes.saturating_add(entry_bytes) > MAX_MASK_CACHE_BYTES
                    {
                        cache.clear();
                    }
                    cache.insert(key, masks.clone());
                }
            }
        }
        masks
    };
    let mask = semantic_masks.face.clone();
    stage_done(&mut benchmark, RetouchStage::Mask, t);
    cancel_if_requested(&cancel)?;

    let t = Instant::now();
    // NAFNet is a real-noise restoration model. A full replacement can erase
    // film grain, halftone dots and fine skin texture, so this control is an
    // intentionally bounded blend rather than a raw model-output opacity.
    let requested_denoise = if config.enable_denoise {
        bounded_amount(config.denoise_amount, config.overall_amount, 0.32)
    } else {
        0.0
    };
    let noise_sigma = estimate_noise_sigma(&rgba, width, height);
    let auto_noise_factor = if !config.auto_denoise || requested_denoise <= 0.0 {
        1.0
    } else if noise_sigma < 0.80 {
        0.0
    } else {
        ((noise_sigma - 0.80) / 3.20).clamp(0.25, 1.0)
    };
    let denoise_amount = requested_denoise * auto_noise_factor;
    benchmark.estimated_noise_sigma = noise_sigma;
    benchmark.effective_denoise_amount = denoise_amount;
    let nafnet = LocalOnnxRunner::with_gpu_preference(ModelId::Nafnet, prefer_gpu);
    let nafnet_available = nafnet.available();
    report(
        &status,
        RetouchStage::Denoise,
        0.20,
        if config.auto_denoise && requested_denoise > 0.0 && denoise_amount <= f32::EPSILON {
            format!("Auto noise: ảnh sạch (σ={noise_sigma:.2}) — bỏ qua NAFNet")
        } else if nafnet_available && denoise_amount > 0.0 {
            "NAFNet: khử noise theo tile 512/overlap 64…".to_string()
        } else {
            "Khử noise…".to_string()
        },
    );
    let mut current = if denoise_amount > 0.0 {
        let (denoised, blend_amount) = if nafnet_available {
            let tile_progress = |completed: usize, total: usize| {
                let ratio = completed as f32 / total.max(1) as f32;
                report(
                    &status,
                    RetouchStage::Denoise,
                    0.18 + ratio * 0.15,
                    format!("NAFNet tile {completed}/{total}"),
                );
            };
            match nafnet.run_nafnet(&rgba, width, height, Some(&cancel), Some(&tile_progress)) {
                Ok(output) => {
                    used_directml |= nafnet.used_directml();
                    used_models.push("NAFNet-SIDD-width32");
                    (output, denoise_amount)
                }
                Err(error) => {
                    warnings.push(format!("NAFNet chạy lỗi ({error}) — dùng denoise CPU"));
                    cpu_fallback_stages.push("denoise");
                    (
                        edge_aware_smooth_effect(&rgba, width, height, 1, 30.0, &cancel)?,
                        denoise_amount * 0.45,
                    )
                }
            }
        } else {
            warnings.push("Thiếu hoặc sai checksum NAFNet — dùng denoise CPU".to_string());
            cpu_fallback_stages.push("denoise");
            (
                edge_aware_smooth_effect(&rgba, width, height, 1, 30.0, &cancel)?,
                denoise_amount * 0.45,
            )
        };
        blend_uniform(&rgba, &denoised, blend_amount)
    } else {
        rgba.clone()
    };
    stage_done(&mut benchmark, RetouchStage::Denoise, t);
    cancel_if_requested(&cancel)?;

    let t = Instant::now();
    // IAT contributes learned luminance only. White balance is estimated
    // independently so a green cast cannot be amplified by IAT's RGB output.
    let iat_amount = if config.enable_color {
        bounded_amount(config.color_amount, config.overall_amount, 0.85)
    } else {
        0.0
    };
    let mut iat = LocalOnnxRunner::with_gpu_preference(ModelId::Iat, prefer_gpu);
    let iat_available = iat.available();
    report(
        &status,
        RetouchStage::ColorExposure,
        0.35,
        if iat_available && iat_amount > 0.0 {
            "IAT luminance + Auto White Balance: phân tích ám màu…".to_string()
        } else {
            "Auto White Balance: phân tích ám màu…".to_string()
        },
    );
    if iat_amount > 0.0 {
        let learned = if iat_available {
            match iat.run_image(&current, width, height) {
                Ok(output) => {
                    used_directml |= iat.used_directml();
                    used_models.push("IAT luminance + adaptive white balance");
                    Some(output)
                }
                Err(error) => {
                    warnings.push(format!(
                        "IAT luminance chạy lỗi ({error}) — Auto White Balance vẫn chạy"
                    ));
                    cpu_fallback_stages.push("learned exposure");
                    None
                }
            }
        } else {
            warnings.push(
                "Thiếu hoặc sai checksum IAT — Auto White Balance vẫn chạy, exposure dùng thống kê"
                    .to_string(),
            );
            cpu_fallback_stages.push("learned exposure");
            None
        };
        let (color, diagnosis) = adaptive_color_effect(
            &current,
            width,
            height,
            learned.as_deref(),
            &semantic_masks.skin,
            config.color_look,
        );
        benchmark.detected_color_cast = diagnosis.cast.to_string();
        benchmark.white_balance_gains = diagnosis.gains;
        benchmark.color_cast_confidence = diagnosis.confidence;
        report(
            &status,
            RetouchStage::ColorExposure,
            0.48,
            format!(
                "Ám màu={}; WB R/G/B={:.2}/{:.2}/{:.2}; look={:?}",
                diagnosis.cast,
                diagnosis.gains[0],
                diagnosis.gains[1],
                diagnosis.gains[2],
                config.color_look
            ),
        );
        current = blend_uniform(&current, &color, iat_amount);
    }
    stage_done(&mut benchmark, RetouchStage::ColorExposure, t);
    cancel_if_requested(&cancel)?;

    let t = Instant::now();
    report(
        &status,
        RetouchStage::FaceRestore,
        0.53,
        "Phục hồi khuôn mặt có bảo vệ identity…".to_string(),
    );
    let skin = semantic_masks.skin.clone();
    let eyes = semantic_masks.eyes_and_brows.clone();
    let lips = semantic_masks.lips.clone();
    let skin_amount = if config.enable_skin {
        bounded_amount(config.skin_amount, config.overall_amount, 0.45) * 0.60
    } else {
        0.0
    };
    if skin_amount > 0.0 {
        let smooth = surface_smooth_effect(&current, width, height, &cancel)?;
        current = blend_rgba_masked(&current, &smooth, &skin, skin_amount);
    }
    let face_amount = if !config.enable_face_restore {
        0.0
    } else if config.protect_identity {
        bounded_amount(config.face_restore_amount, config.overall_amount, 0.60)
    } else {
        bounded_amount(config.face_restore_amount, config.overall_amount, 0.85)
    };
    // Eye/lip masks overlap the general face mask. Their ceilings prevent the
    // stacked blends from bypassing Protect Identity at high slider values.
    let eye_face_amount = if config.enable_eyes {
        bounded_amount(
            config.eyes_amount,
            config.overall_amount,
            if config.protect_identity { 0.50 } else { 0.85 },
        )
    } else {
        0.0
    };
    let lip_face_amount = if config.enable_lips {
        bounded_amount(
            config.lips_amount,
            config.overall_amount,
            if config.protect_identity { 0.45 } else { 0.80 },
        )
    } else {
        0.0
    };
    let face_transforms = semantic_masks
        .aligned_faces
        .iter()
        .map(|face| face.transform)
        .collect::<Vec<_>>();
    let face_effect = if face_amount > 0.0 || eye_face_amount > 0.0 || lip_face_amount > 0.0 {
        let gfpgan = LocalOnnxRunner::with_gpu_preference(ModelId::Gfpgan, prefer_gpu);
        if gfpgan.available() && !face_transforms.is_empty() {
            let face_progress = |completed: usize, total: usize| {
                let ratio = completed as f32 / total.max(1) as f32;
                report(
                    &status,
                    RetouchStage::FaceRestore,
                    0.48 + ratio * 0.14,
                    format!("GFPGAN face {completed}/{total}"),
                );
            };
            match gfpgan.run_gfpgan_face_details(
                &current,
                width,
                height,
                &face_transforms,
                Some(&cancel),
                Some(&face_progress),
            ) {
                Ok(output) => {
                    used_directml |= gfpgan.used_directml();
                    used_models.push("GFPGAN v1.4 texture transfer");
                    output
                }
                Err(error) => {
                    warnings.push(format!("GFPGAN chạy lỗi ({error}) — dùng face detail CPU"));
                    cpu_fallback_stages.push("face-restore");
                    unsharp_effect(&current, width, height, &cancel)?
                }
            }
        } else {
            warnings.push(if !gfpgan.available() {
                "Thiếu hoặc sai checksum GFPGAN — dùng face detail CPU".to_string()
            } else {
                "GFPGAN bỏ qua vì không có khuôn mặt đã alignment — dùng face detail CPU"
                    .to_string()
            });
            cpu_fallback_stages.push("face-restore");
            unsharp_effect(&current, width, height, &cancel)?
        }
    } else {
        current.clone()
    };
    current = blend_rgba_masked(&current, &face_effect, &mask, face_amount);
    current = blend_rgba_masked(&current, &face_effect, &eyes, eye_face_amount);
    current = blend_rgba_masked(&current, &face_effect, &lips, lip_face_amount);
    stage_done(&mut benchmark, RetouchStage::FaceRestore, t);

    let t = Instant::now();
    report(
        &status,
        RetouchStage::SelectiveDetail,
        0.70,
        "Tăng detail chọn lọc, feather biên…".to_string(),
    );
    let detail_mask = if semantic_masks.ai_generated {
        semantic_masks
            .hair
            .iter()
            .zip(&semantic_masks.eyes_and_brows)
            .zip(&semantic_masks.lips)
            .zip(&semantic_masks.clothes)
            .map(|(((hair, eyes), lips), clothes)| {
                let mut value = 0.0f32;
                if config.enable_hair {
                    value = value.max(*hair);
                }
                if config.enable_eyes {
                    value = value.max(*eyes);
                }
                if config.enable_lips {
                    value = value.max(*lips);
                }
                if config.enable_clothes {
                    value = value.max(*clothes);
                }
                value
            })
            .collect::<Vec<_>>()
    } else if config.enable_hair
        || config.enable_eyes
        || config.enable_lips
        || config.enable_clothes
    {
        selective_detail_mask(&current, &mask, &skin, width, height)
    } else {
        vec![0.0; width as usize * height as usize]
    };
    let selected_detail_max = [
        (config.enable_hair, config.hair_detail_amount),
        (config.enable_eyes, config.eyes_amount),
        (config.enable_lips, config.lips_amount),
        (config.enable_clothes, config.clothes_amount),
    ]
    .into_iter()
    .filter_map(|(enabled, amount)| enabled.then_some(amount))
    .max()
    .unwrap_or(0);
    let detail_amount = clamp_amount(selected_detail_max, config.overall_amount);
    let has_detail_roi = detail_mask.iter().any(|value| *value > 0.015);
    let mut detail = if detail_amount > 0.0 && has_detail_roi {
        let realesrgan =
            LocalOnnxRunner::with_gpu_preference(ModelId::RealesrganGeneral, prefer_gpu);
        if realesrgan.available() {
            let tile_progress = |completed: usize, total: usize| {
                let ratio = completed as f32 / total.max(1) as f32;
                report(
                    &status,
                    RetouchStage::SelectiveDetail,
                    0.65 + ratio * 0.19,
                    format!("Real-ESRGAN detail tile {completed}/{total}"),
                );
            };
            match realesrgan.run_realesrgan_detail(
                &current,
                width,
                height,
                &detail_mask,
                Some(&cancel),
                Some(&tile_progress),
            ) {
                Ok(output) => {
                    used_directml |= realesrgan.used_directml();
                    used_models.push("Real-ESRGAN General x4v3 ROI");
                    output
                }
                Err(error) => {
                    warnings.push(format!(
                        "Real-ESRGAN detail chạy lỗi ({error}) — dùng detail CPU"
                    ));
                    cpu_fallback_stages.push("selective-detail");
                    unsharp_effect(&current, width, height, &cancel)?
                }
            }
        } else {
            warnings.push("Thiếu hoặc sai checksum Real-ESRGAN — dùng detail CPU".to_string());
            cpu_fallback_stages.push("selective-detail");
            unsharp_effect(&current, width, height, &cancel)?
        }
    } else {
        current.clone()
    };
    if semantic_masks.ai_generated {
        // Real-ESRGAN may shift the colour of a small ROI. Match each semantic
        // region back to its local source mean before feathered compositing.
        // This preserves the recovered texture while avoiding coloured seams.
        if config.enable_hair {
            colour_match_masked(&current, &mut detail, &semantic_masks.hair, 12.0);
            current = blend_rgba_masked(
                &current,
                &detail,
                &semantic_masks.hair,
                bounded_amount(config.hair_detail_amount, config.overall_amount, 0.80),
            );
        }
        if config.enable_eyes {
            colour_match_masked(&current, &mut detail, &semantic_masks.eyes_and_brows, 12.0);
            current = blend_rgba_masked(
                &current,
                &detail,
                &semantic_masks.eyes_and_brows,
                bounded_amount(config.eyes_amount, config.overall_amount, 0.75),
            );
        }
        if config.enable_lips {
            colour_match_masked(&current, &mut detail, &semantic_masks.lips, 12.0);
            current = blend_rgba_masked(
                &current,
                &detail,
                &semantic_masks.lips,
                bounded_amount(config.lips_amount, config.overall_amount, 0.70),
            );
        }
        if config.enable_clothes {
            colour_match_masked(&current, &mut detail, &semantic_masks.clothes, 12.0);
            current = blend_rgba_masked(
                &current,
                &detail,
                &semantic_masks.clothes,
                bounded_amount(config.clothes_amount, config.overall_amount, 0.75),
            );
        }
    } else {
        current = blend_rgba_masked(&current, &detail, &detail_mask, detail_amount);
    }
    stage_done(&mut benchmark, RetouchStage::SelectiveDetail, t);

    let t = Instant::now();
    report(
        &status,
        RetouchStage::Composite,
        0.84,
        "Composite mask + giữ nguyên vùng ngoài…".to_string(),
    );
    // Every stage above has already applied `overall_amount` exactly once and
    // selective stages have already preserved pixels outside their masks. The
    // old final face-mask blend multiplied all strengths a second time and also
    // discarded global denoise/tone changes outside the face.
    let composite = current;
    stage_done(&mut benchmark, RetouchStage::Composite, t);
    current = composite;

    let (changed_pixels, mean_absolute_delta) = change_stats(&rgba, &current);
    benchmark.changed_pixels = changed_pixels;
    benchmark.mean_absolute_delta = mean_absolute_delta;
    let (face_changed_pixels, face_mean_absolute_delta) =
        change_stats_masked(&rgba, &current, &mask);
    benchmark.face_changed_pixels = face_changed_pixels;
    benchmark.face_mean_absolute_delta = face_mean_absolute_delta;
    if changed_pixels == 0 && config.upscale == UpscaleMode::Off {
        warnings.push(
            "Kết quả không đổi pixel nào; hãy tăng Overall hoặc bật ít nhất một mục xử lý"
                .to_string(),
        );
    }

    if config.upscale != UpscaleMode::Off {
        let t = Instant::now();
        let factor = config.upscale.factor();
        if !upscale_within_budget(width, height, factor) {
            report(
                &status,
                RetouchStage::Upscale,
                0.94,
                format!("Bỏ qua upscale x{factor}: vượt ngân sách RAM an toàn"),
            );
            warnings.push(format!(
                "Upscale x{factor} cần {} triệu pixel, vượt giới hạn {} triệu — giữ kích thước gốc",
                width as u64 * height as u64 * factor as u64 * factor as u64 / 1_000_000,
                MAX_UPSCALE_PIXELS / 1_000_000
            ));
            benchmark.output_width = width;
            benchmark.output_height = height;
        } else {
            let model_id = if factor == 2 {
                ModelId::RealesrganRrdbX2
            } else {
                ModelId::RealesrganRrdb
            };
            let runner = LocalOnnxRunner::with_gpu_preference(model_id, prefer_gpu);
            report(
                &status,
                RetouchStage::Upscale,
                0.94,
                if runner.available() {
                    format!("Real-ESRGAN RRDB x{factor}: upscale tile 256/overlap 32…")
                } else {
                    format!("Upscale x{factor}…")
                },
            );
            let mut ai_output = None;
            if runner.available() {
                let tile_progress = |completed: usize, total: usize| {
                    let ratio = completed as f32 / total.max(1) as f32;
                    report(
                        &status,
                        RetouchStage::Upscale,
                        0.86 + ratio * 0.13,
                        format!("Real-ESRGAN x{factor} tile {completed}/{total}"),
                    );
                };
                match runner.run_realesrgan_upscale(
                    &current,
                    width,
                    height,
                    factor,
                    Some(&cancel),
                    Some(&tile_progress),
                ) {
                    Ok((rgba, output_width, output_height)) => {
                        used_directml |= runner.used_directml();
                        used_models.push(if factor == 2 {
                            "Real-ESRGAN RRDB x2"
                        } else {
                            "Real-ESRGAN RRDB x4"
                        });
                        ai_output = Some((rgba, output_width, output_height));
                    }
                    Err(error) => warnings.push(format!(
                        "Real-ESRGAN RRDB x{factor} chạy lỗi ({error}) — dùng Lanczos CPU"
                    )),
                }
            } else {
                warnings.push(format!(
                    "Thiếu hoặc sai checksum {} — dùng Lanczos CPU",
                    model_id.display_name()
                ));
            }
            if let Some((rgba, output_width, output_height)) = ai_output {
                current = rgba;
                benchmark.output_width = output_width;
                benchmark.output_height = output_height;
            } else {
                cpu_fallback_stages.push("upscale");
                let img = RgbaImage::from_raw(width, height, current)
                    .ok_or_else(|| "Không tạo được ảnh upscale".to_string())?;
                let up = imageops::resize(
                    &img,
                    width.saturating_mul(factor),
                    height.saturating_mul(factor),
                    imageops::FilterType::Lanczos3,
                );
                current = up.into_raw();
                benchmark.output_width = width.saturating_mul(factor);
                benchmark.output_height = height.saturating_mul(factor);
            }
        }
        stage_done(&mut benchmark, RetouchStage::Upscale, t);
    } else {
        benchmark.output_width = width;
        benchmark.output_height = height;
    }
    let mut provider_parts = Vec::new();
    if !used_models.is_empty() {
        let execution = if used_directml {
            "DirectML GPU (CPU operator fallback)"
        } else {
            "ONNX Runtime CPU"
        };
        provider_parts.push(format!("{execution} [{}]", used_models.join(", ")));
    }
    if used_mask_cache {
        provider_parts.push("mask cache".to_string());
    }
    if semantic_masks.ai_generated {
        provider_parts.push(format!("faces={}", semantic_masks.face_count));
    }
    if !cpu_fallback_stages.is_empty() {
        cpu_fallback_stages.sort_unstable();
        cpu_fallback_stages.dedup();
        provider_parts.push(format!("CPU fallback [{}]", cpu_fallback_stages.join(", ")));
    }
    benchmark.provider = if provider_parts.is_empty() {
        "CPU (no enabled retouch stage)".to_string()
    } else {
        provider_parts.join(" + ")
    };
    benchmark.total_millis = started_total.elapsed().as_millis();
    let tracked_buffers = rgba.len() + current.len() + mask.len() * std::mem::size_of::<f32>();
    benchmark.peak_memory_bytes = crate::core::mem_report::process_working_set()
        .map(|memory| memory.peak_working_set as usize)
        .unwrap_or(tracked_buffers)
        .max(tracked_buffers);
    let mask_preview_rgba = config.preview_masks.then(|| {
        let preview = semantic_mask_preview(&semantic_masks);
        if benchmark.output_width == width && benchmark.output_height == height {
            preview
        } else {
            let image = RgbaImage::from_raw(width, height, preview)
                .expect("semantic mask preview dimensions are exact");
            imageops::resize(
                &image,
                benchmark.output_width,
                benchmark.output_height,
                imageops::FilterType::Triangle,
            )
            .into_raw()
        }
    });
    report(
        &status,
        RetouchStage::Done,
        1.0,
        format!("Hoàn tất trong {} ms", benchmark.total_millis),
    );
    Ok(RetouchResult {
        rgba: current,
        width: benchmark.output_width,
        height: benchmark.output_height,
        face_mask: mask,
        mask_preview_rgba,
        benchmark,
        warnings,
    })
}

pub struct RetouchJob {
    pub doc_id: u32,
    pub started: Instant,
    pub cancel: Arc<AtomicBool>,
    pub status: Arc<Mutex<RetouchStatus>>,
    rx: Receiver<Result<RetouchResult, String>>,
    pub abandoned: bool,
}

pub struct RetouchFinished {
    pub doc_id: u32,
    pub result: Result<RetouchResult, String>,
    pub abandoned: bool,
}

#[derive(Default)]
pub struct RetouchEngine {
    jobs: Vec<RetouchJob>,
    mask_cache: Arc<Mutex<HashMap<u64, SemanticMasks>>>,
}

impl RetouchEngine {
    pub fn is_busy(&self, doc_id: u32) -> bool {
        self.jobs.iter().any(|j| j.doc_id == doc_id && !j.abandoned)
    }
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }
    pub fn job_for_doc(&self, doc_id: u32) -> Option<&RetouchJob> {
        self.jobs
            .iter()
            .find(|j| j.doc_id == doc_id && !j.abandoned)
    }

    pub fn run_async(
        &mut self,
        doc_id: u32,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        config: RetouchConfig,
    ) -> bool {
        if self.is_busy(doc_id) {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(RetouchStatus::default()));
        let worker_status = status.clone();
        let worker_cancel = cancel.clone();
        let worker_mask_cache = self.mask_cache.clone();
        std::thread::spawn(move || {
            if let Ok(mut s) = worker_status.lock() {
                s.running = true;
                s.error = None;
            }
            let result = run_pipeline(
                rgba,
                width,
                height,
                config,
                worker_status.clone(),
                worker_cancel,
                worker_mask_cache,
            );
            if let Err(e) = &result {
                if let Ok(mut s) = worker_status.lock() {
                    s.error = Some(e.clone());
                }
            }
            if let Ok(mut s) = worker_status.lock() {
                s.running = false;
                if let Ok(r) = &result {
                    s.last_benchmark = Some(r.benchmark.clone());
                }
            }
            let _ = tx.send(result);
        });
        self.jobs.push(RetouchJob {
            doc_id,
            started: Instant::now(),
            cancel,
            status,
            rx,
            abandoned: false,
        });
        true
    }

    pub fn cancel_doc(&mut self, doc_id: u32) -> bool {
        if let Some(job) = self
            .jobs
            .iter_mut()
            .find(|j| j.doc_id == doc_id && !j.abandoned)
        {
            job.cancel.store(true, Ordering::Relaxed);
            job.abandoned = true;
            return true;
        }
        false
    }

    pub fn poll_finished(&mut self) -> Vec<RetouchFinished> {
        let mut done = Vec::new();
        let mut i = 0;
        while i < self.jobs.len() {
            let result = match self.jobs[i].rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("Retouch worker stopped".to_string())),
            };
            if let Some(result) = result {
                let job = self.jobs.remove(i);
                done.push(RetouchFinished {
                    doc_id: job.doc_id,
                    result,
                    abandoned: job.abandoned,
                });
            } else {
                i += 1;
            }
        }
        done
    }
}

pub fn benchmark_line(result: &RetouchResult) -> String {
    let stages = result
        .benchmark
        .timings
        .iter()
        .map(|t| format!("{}={}ms", t.stage, t.millis))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Retouch: {}; total={}ms; peak-working-set={}MB; output={}x{}; changed={}px; mean-delta={:.2}; face-changed={}px; face-mean-delta={:.2}; noise-sigma={:.2}; denoise-effective={:.2}; color-cast={}; wb-gains={:.2}/{:.2}/{:.2}; color-confidence={:.2}; provider={}",
        stages,
        result.benchmark.total_millis,
        result.benchmark.peak_memory_bytes / (1024 * 1024),
        result.width,
        result.height,
        result.benchmark.changed_pixels,
        result.benchmark.mean_absolute_delta,
        result.benchmark.face_changed_pixels,
        result.benchmark.face_mean_absolute_delta,
        result.benchmark.estimated_noise_sigma,
        result.benchmark.effective_denoise_amount,
        result.benchmark.detected_color_cast,
        result.benchmark.white_balance_gains[0],
        result.benchmark.white_balance_gains[1],
        result.benchmark.white_balance_gains[2],
        result.benchmark.color_cast_confidence,
        result.benchmark.provider
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_and_manifest_cover_the_first_pipeline_models() {
        assert_eq!(model_metadata().len(), 9);
        assert!(manifest_template().iter().any(|m| m.id == "body-parsing"));
        assert!(manifest_template().iter().any(|m| m.id == "gfpgan"));
        assert!(manifest_template()
            .iter()
            .any(|m| m.id == "realesrgan-rrdb"));
        assert_eq!(ModelId::FaceDetector.directory(), "face-detector");
        assert_eq!(ModelId::Bisenet.directory(), "bisenet");
        assert!(ModelId::ALL
            .iter()
            .all(|id| id.expected_sha256().len() == 64));
    }

    #[test]
    fn stage_strengths_have_safe_full_slider_ceilings() {
        let defaults = RetouchConfig::default();
        assert_eq!(defaults.overall_amount, 100);
        assert_eq!(defaults.denoise_amount, 100);
        assert_eq!(defaults.clothes_amount, 100);
        assert_eq!(defaults.color_look, ColorLook::Fresh);
        assert!(defaults.any_effect_enabled());
        assert!(RetouchConfig::default().auto_denoise);
        assert_eq!(bounded_amount(100, 100, 0.32), 0.32);
        assert_eq!(bounded_amount(50, 50, 0.40), 0.25);
    }

    #[test]
    fn noise_estimator_skips_flat_fields_and_detects_pixel_noise() {
        let flat = [128, 128, 128, 255].repeat(64 * 64);
        assert!(estimate_noise_sigma(&flat, 64, 64) < 0.01);

        let mut checker = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64 {
            for x in 0..64 {
                let value = if (x + y) % 2 == 0 { 70 } else { 190 };
                checker.extend_from_slice(&[value, value, value, 255]);
            }
        }
        assert!(estimate_noise_sigma(&checker, 64, 64) > 20.0);
    }

    #[test]
    fn color_analyser_detects_and_reduces_a_green_cast() {
        let mut input = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64u8 {
            for x in 0..64u8 {
                let edge = if (x / 8 + y / 8) % 2 == 0 { 45 } else { 0 };
                input.extend_from_slice(&[80 + edge, 118 + edge, 72 + edge, 255]);
            }
        }
        let diagnosis = analyse_color_cast(&input, 64, 64);
        assert_eq!(diagnosis.cast, "green");
        assert!(diagnosis.gains[1] < diagnosis.gains[0]);
        assert!(diagnosis.gains[1] < diagnosis.gains[2]);
        let (output, _) = adaptive_color_effect(&input, 64, 64, None, &[], ColorLook::Natural);
        let means = |values: &[u8]| {
            let mut sums = [0u64; 3];
            for pixel in values.chunks_exact(4) {
                for channel in 0..3 {
                    sums[channel] += pixel[channel] as u64;
                }
            }
            sums.map(|sum| sum as f32 / (values.len() / 4) as f32)
        };
        let before = means(&input);
        let after = means(&output);
        let spread = |rgb: [f32; 3]| {
            rgb.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - rgb.iter().copied().fold(f32::INFINITY, f32::min)
        };
        assert!(spread(after) < spread(before));
    }

    #[test]
    fn learned_exposure_cannot_inject_its_color_cast() {
        let input = [96, 96, 96, 255].repeat(24 * 24);
        let learned_green = [80, 180, 70, 255].repeat(24 * 24);
        let (output, _) = adaptive_color_effect(
            &input,
            24,
            24,
            Some(&learned_green),
            &[],
            ColorLook::Natural,
        );
        assert!(output
            .chunks_exact(4)
            .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2]));
        assert!(output[0] > input[0]);
    }

    #[test]
    fn color_only_pipeline_reports_cast_and_changes_color() {
        let mut input = Vec::with_capacity(32 * 32 * 4);
        for y in 0..32u8 {
            for x in 0..32u8 {
                let edge = if (x / 4 + y / 4) % 2 == 0 { 36 } else { 0 };
                input.extend_from_slice(&[72 + edge, 112 + edge, 68 + edge, 255]);
            }
        }
        let result = run_pipeline(
            input.clone(),
            32,
            32,
            RetouchConfig {
                enable_denoise: false,
                enable_color: true,
                enable_face_restore: false,
                enable_hair: false,
                enable_skin: false,
                enable_eyes: false,
                enable_lips: false,
                enable_clothes: false,
                color_amount: 100,
                color_look: ColorLook::Fresh,
                ..Default::default()
            },
            Arc::new(Mutex::new(RetouchStatus::default())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();
        assert_ne!(result.rgba, input);
        assert_eq!(result.benchmark.detected_color_cast, "green");
        assert!(result.benchmark.white_balance_gains[1] < result.benchmark.white_balance_gains[0]);
        assert!(result.benchmark.changed_pixels > 0);
    }

    #[test]
    #[ignore = "manual visual color-stage render; set IAI_COLOR_SAMPLE and IAI_COLOR_OUTPUT"]
    fn render_adaptive_color_sample() {
        let input_path = std::env::var("IAI_COLOR_SAMPLE").expect("IAI_COLOR_SAMPLE");
        let output_path = std::env::var("IAI_COLOR_OUTPUT").expect("IAI_COLOR_OUTPUT");
        let image = image::open(input_path).unwrap().to_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        let mut runner = LocalOnnxRunner::new(ModelId::Iat);
        let learned = runner.run_image(&rgba, width, height).ok();
        let (output, diagnosis) = adaptive_color_effect(
            &rgba,
            width,
            height,
            learned.as_deref(),
            &[],
            ColorLook::Fresh,
        );
        RgbaImage::from_raw(width, height, output)
            .unwrap()
            .save(output_path)
            .unwrap();
        eprintln!(
            "cast={} gains={:?} confidence={:.3}",
            diagnosis.cast, diagnosis.gains, diagnosis.confidence
        );
    }

    #[test]
    fn upscale_budget_blocks_unsafe_large_outputs() {
        assert!(upscale_within_budget(6000, 4000, 2));
        assert!(!upscale_within_budget(6000, 4000, 4));
        assert!(!upscale_within_budget(u32::MAX, u32::MAX, 4));
    }

    #[test]
    fn semantic_preview_is_transparent_outside_masks() {
        let masks = SemanticMasks {
            face: vec![0.0, 1.0, 1.0],
            skin: vec![0.0, 1.0, 0.0],
            hair: vec![0.0; 3],
            eyes_and_brows: vec![0.0; 3],
            lips: vec![0.0; 3],
            clothes: vec![0.0; 3],
            background: vec![0.0; 3],
            accessories: vec![0.0; 3],
            aligned_faces: Vec::new(),
            face_count: 0,
            ai_generated: true,
        };
        let preview = semantic_mask_preview(&masks);
        assert_eq!(&preview[0..4], &[0, 0, 0, 0]);
        assert_eq!(&preview[4..7], &[255, 92, 92]);
        assert_eq!(preview[7], 176);
        assert_eq!(&preview[8..11], &[235, 70, 70]);
        assert_eq!(preview[11], 176);
    }

    #[test]
    fn mask_morphology_expands_and_contracts_without_shifting_alignment() {
        let mut point = vec![0.0; 25];
        point[12] = 1.0;
        morph_mask(&mut point, 5, 5, 1, true);
        assert_eq!(point.iter().filter(|value| **value > 0.5).count(), 9);
        assert_eq!(point[12], 1.0);

        morph_mask(&mut point, 5, 5, 1, false);
        assert_eq!(point.iter().filter(|value| **value > 0.5).count(), 1);
        assert_eq!(point[12], 1.0);
    }

    #[test]
    fn local_colour_match_changes_only_the_selected_region() {
        let base = vec![80, 100, 120, 255, 20, 30, 40, 255];
        let mut effect = vec![100, 120, 140, 255, 200, 210, 220, 255];
        let untouched = effect[4..8].to_vec();
        colour_match_masked(&base, &mut effect, &[1.0, 0.0], 32.0);
        assert_eq!(&effect[0..4], &base[0..4]);
        assert_eq!(&effect[4..8], untouched.as_slice());
    }

    #[test]
    fn tile_runner_honours_cancel_before_loading_model() {
        let runner = LocalOnnxRunner::new(ModelId::Nafnet);
        let cancel = AtomicBool::new(true);
        let error = runner
            .run_nafnet(&[0, 0, 0, 255], 1, 1, Some(&cancel), None)
            .unwrap_err();
        assert!(error.contains("hủy"));
    }

    #[test]
    fn mask_blend_preserves_pixels_outside_mask() {
        let base = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let effect = vec![200, 200, 200, 255, 240, 240, 240, 255];
        let out = blend_rgba_masked(&base, &effect, &[1.0, 0.0], 1.0);
        assert_eq!(&out[4..8], &base[4..8]);
        assert_eq!(&out[0..3], &[200, 200, 200]);
    }

    #[test]
    fn cpu_pipeline_keeps_dimensions_and_can_upscale() {
        let input = vec![128u8; 8 * 8 * 4];
        let status = Arc::new(Mutex::new(RetouchStatus::default()));
        let result = run_pipeline(
            input,
            8,
            8,
            RetouchConfig {
                overall_amount: 0,
                upscale: UpscaleMode::X2,
                ..Default::default()
            },
            status,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();
        assert_eq!((result.width, result.height), (16, 16));
        assert_eq!(result.rgba.len(), 16 * 16 * 4);
        assert!(!result.benchmark.timings.is_empty());
    }

    #[test]
    fn default_cpu_pipeline_produces_a_visible_pixel_change() {
        let width = 64;
        let height = 64;
        let mut input = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let checker = if (x / 4 + y / 4) % 2 == 0 { 22 } else { 0 };
                input.extend_from_slice(&[
                    (72 + x * 2 + checker).min(255) as u8,
                    (54 + y * 2 + checker).min(255) as u8,
                    (48 + (x + y) + checker).min(255) as u8,
                    255,
                ]);
            }
        }
        let original = input.clone();
        let result = run_pipeline(
            input,
            width as u32,
            height as u32,
            RetouchConfig {
                denoise_amount: 0,
                color_amount: 0,
                ..Default::default()
            },
            Arc::new(Mutex::new(RetouchStatus::default())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();

        assert_ne!(result.rgba, original);
        assert!(result.benchmark.changed_pixels > 0);
        assert!(result.benchmark.mean_absolute_delta > 0.0);
        assert!(result.benchmark.mean_absolute_delta <= 6.0);
    }

    #[test]
    fn cpu_mask_fallback_is_not_persisted_in_ai_mask_cache() {
        let cache = Arc::new(Mutex::new(HashMap::new()));
        let input = vec![128u8; 16 * 16 * 4];
        let run = || {
            run_pipeline(
                input.clone(),
                16,
                16,
                RetouchConfig {
                    overall_amount: 0,
                    enable_hair: false,
                    enable_skin: false,
                    enable_clothes: false,
                    ..Default::default()
                },
                Arc::new(Mutex::new(RetouchStatus::default())),
                Arc::new(AtomicBool::new(false)),
                cache.clone(),
            )
            .unwrap()
        };
        let first = run();
        assert_eq!(cache.lock().unwrap().len(), 0);
        let second = run();
        assert_eq!(first.face_mask, second.face_mask);
        assert_eq!(cache.lock().unwrap().len(), 0);
    }

    #[test]
    fn exported_iat_contract_runs_in_ort_when_model_is_present() {
        let mut runner = LocalOnnxRunner::new(ModelId::Iat);
        if !runner.available() {
            return;
        }
        let input = vec![96u8; 32 * 48 * 4];
        let output = runner.run_image(&input, 48, 32).unwrap();
        assert_eq!(output.len(), input.len());
        assert!(output.chunks_exact(4).all(|pixel| pixel[3] == 96));
        assert_ne!(output, input);
    }

    #[cfg(windows)]
    #[test]
    fn directml_preference_runs_or_falls_back_to_cpu_safely() {
        let mut runner = LocalOnnxRunner::with_gpu_preference(ModelId::Iat, true);
        if !runner.available() {
            return;
        }
        let input = [82, 96, 110, 255].repeat(24 * 24);
        let output = runner.run_image(&input, 24, 24).unwrap();
        assert_eq!(output.len(), input.len());
        eprintln!(
            "retouch provider test: {}",
            if runner.used_directml() {
                "DirectML"
            } else {
                "CPU fallback"
            }
        );
    }

    #[test]
    fn exported_body_parser_covers_the_full_canvas() {
        let runner = LocalOnnxRunner::new(ModelId::BodyParsing);
        if !runner.available() {
            return;
        }
        let input = [80, 110, 145, 255].repeat(40 * 28);
        let masks = runner.run_body_parsing_masks(&input, 28, 40, None).unwrap();
        assert_eq!(masks.background.len(), 28 * 40);
        assert_eq!(masks.hair.len(), 28 * 40);
        assert_eq!(masks.clothes.len(), 28 * 40);
        assert!(masks.ai_generated);
        assert!(masks
            .background
            .iter()
            .chain(&masks.hair)
            .chain(&masks.skin)
            .chain(&masks.clothes)
            .chain(&masks.accessories)
            .any(|value| *value > 0.01));
    }

    #[test]
    fn disabling_every_stage_preserves_every_pixel() {
        let input = [70, 90, 120, 255].repeat(16 * 16);
        let result = run_pipeline(
            input.clone(),
            16,
            16,
            RetouchConfig {
                enable_denoise: false,
                enable_color: false,
                enable_face_restore: false,
                enable_hair: false,
                enable_skin: false,
                enable_eyes: false,
                enable_lips: false,
                enable_clothes: false,
                preview_masks: false,
                upscale: UpscaleMode::Off,
                ..Default::default()
            },
            Arc::new(Mutex::new(RetouchStatus::default())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();
        assert_eq!(result.rgba, input);
        assert_eq!(result.benchmark.changed_pixels, 0);
        assert_eq!(result.benchmark.provider, "CPU (no enabled retouch stage)");
    }

    #[test]
    fn iat_large_image_preview_path_preserves_source_dimensions() {
        let mut runner = LocalOnnxRunner::new(ModelId::Iat);
        if !runner.available() {
            return;
        }
        let (width, height) = (1600u32, 1000u32);
        let mut input = vec![96u8; (width * height * 4) as usize];
        for pixel in input.chunks_exact_mut(4) {
            pixel[3] = 177;
        }
        let output = runner.run_image(&input, width, height).unwrap();
        assert_eq!(output.len(), input.len());
        assert!(output.chunks_exact(4).all(|pixel| pixel[3] == 177));
        assert_ne!(output, input);
    }

    #[test]
    fn exported_nafnet_contract_runs_in_ort_when_model_is_present() {
        let runner = LocalOnnxRunner::new(ModelId::Nafnet);
        if !runner.available() {
            return;
        }
        let mut input = Vec::with_capacity(32 * 48 * 4);
        for y in 0..32u8 {
            for x in 0..48u8 {
                input.extend_from_slice(&[80u8.saturating_add(x), 70u8.saturating_add(y), 64, 211]);
            }
        }
        let completed = std::cell::Cell::new(0usize);
        let total = std::cell::Cell::new(0usize);
        let progress = |done: usize, count: usize| {
            completed.set(done);
            total.set(count);
        };
        let output = runner
            .run_nafnet(&input, 48, 32, None, Some(&progress))
            .unwrap();
        assert_eq!(output.len(), input.len());
        assert!(output.chunks_exact(4).all(|pixel| pixel[3] == 211));
        assert_ne!(output, input);
        assert_eq!(completed.get(), total.get());
        assert_eq!(total.get(), 1);
    }

    #[test]
    fn auto_noise_skips_nafnet_for_a_clean_flat_image() {
        let input = [96, 96, 96, 255].repeat(32 * 32);
        let result = run_pipeline(
            input,
            32,
            32,
            RetouchConfig {
                overall_amount: 100,
                denoise_amount: 100,
                color_amount: 0,
                face_restore_amount: 0,
                hair_detail_amount: 0,
                skin_amount: 0,
                eyes_amount: 0,
                lips_amount: 0,
                clothes_amount: 0,
                ..Default::default()
            },
            Arc::new(Mutex::new(RetouchStatus::default())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();

        assert!(result.benchmark.estimated_noise_sigma < 0.01);
        assert_eq!(result.benchmark.effective_denoise_amount, 0.0);
        assert!(!result.benchmark.provider.contains("NAFNet"));
    }

    #[test]
    fn pipeline_reports_only_models_that_completed_inference() {
        if !LocalOnnxRunner::new(ModelId::Nafnet).available()
            || !LocalOnnxRunner::new(ModelId::Iat).available()
            || !LocalOnnxRunner::new(ModelId::RealesrganGeneral).available()
        {
            return;
        }
        let mut input = Vec::with_capacity(32 * 32 * 4);
        for y in 0..32u8 {
            for x in 0..32u8 {
                input.extend_from_slice(&[90 + x, 72 + y, 68, 255]);
            }
        }
        let result = run_pipeline(
            input,
            32,
            32,
            RetouchConfig {
                auto_denoise: false,
                ..Default::default()
            },
            Arc::new(Mutex::new(RetouchStatus::default())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(HashMap::new())),
        )
        .unwrap();
        assert!(result.benchmark.provider.contains("NAFNet-SIDD-width32"));
        assert!(result
            .benchmark
            .provider
            .contains("IAT luminance + adaptive white balance"));
        assert!(result
            .benchmark
            .provider
            .contains("Real-ESRGAN General x4v3 ROI"));
        assert!(result
            .warnings
            .iter()
            .all(|warning| !warning.contains("Thiếu NAFNet")
                && !warning.contains("Thiếu IAT")
                && !warning.contains("Thiếu Real-ESRGAN")));
    }

    #[test]
    fn exported_realesrgan_contract_restores_roi_and_preserves_alpha() {
        let runner = LocalOnnxRunner::new(ModelId::RealesrganGeneral);
        if !runner.available() {
            return;
        }
        let mut input = Vec::with_capacity(40 * 48 * 4);
        for y in 0..40u8 {
            for x in 0..48u8 {
                input.extend_from_slice(&[70 + x, 60 + y, 56, 173]);
            }
        }
        let mask = vec![1.0; 40 * 48];
        let output = runner
            .run_realesrgan_detail(&input, 48, 40, &mask, None, None)
            .unwrap();
        assert_eq!(output.len(), input.len());
        assert!(output.chunks_exact(4).all(|pixel| pixel[3] == 173));
        assert_ne!(output, input);
    }

    #[test]
    fn yunet_landmarks_align_real_face_for_bisenet_semantic_masks() {
        let detector = LocalOnnxRunner::new(ModelId::FaceDetector);
        let parser = LocalOnnxRunner::new(ModelId::Bisenet);
        let sample = Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/Blake_Lively.jpg");
        if !detector.available() || !parser.available() || !sample.is_file() {
            return;
        }
        let image = image::open(sample).unwrap().to_rgba8();
        let detections = detector
            .detect_faces(image.as_raw(), image.width(), image.height())
            .unwrap();
        assert!(!detections.is_empty());
        assert!(detections[0].score >= 0.65);
        assert!(detections[0].landmarks[0][0] < detections[0].landmarks[1][0]);

        let masks = generate_aligned_semantic_masks(
            image.as_raw(),
            image.width(),
            image.height(),
            &detector,
            &parser,
            None,
            None,
        )
        .unwrap();
        assert!(masks.ai_generated);
        assert!(masks.face_count >= 1);
        assert_eq!(masks.aligned_faces.len(), masks.face_count);
        assert!(masks
            .aligned_faces
            .iter()
            .all(|face| face.face.iter().any(|value| *value > 127)));
        assert!(masks.face.iter().filter(|value| **value > 0.5).count() > 100);
        assert!(masks.skin.iter().filter(|value| **value > 0.5).count() > 100);
    }

    #[test]
    fn gfpgan_transfers_texture_without_replacing_unaligned_image() {
        let detector = LocalOnnxRunner::new(ModelId::FaceDetector);
        let restorer = LocalOnnxRunner::new(ModelId::Gfpgan);
        let sample = Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/Blake_Lively.jpg");
        if !detector.available() || !restorer.available() || !sample.is_file() {
            return;
        }
        let image = image::open(sample).unwrap().to_rgba8();
        let detections = detector
            .detect_faces(image.as_raw(), image.width(), image.height())
            .unwrap();
        let transforms = detections
            .iter()
            .map(|detection| {
                SimilarityTransform::fit(&detection.landmarks, &FACE_ALIGNMENT_TARGET).unwrap()
            })
            .collect::<Vec<_>>();
        let output = restorer
            .run_gfpgan_face_details(
                image.as_raw(),
                image.width(),
                image.height(),
                &transforms,
                None,
                None,
            )
            .unwrap();
        assert_eq!(output.len(), image.as_raw().len());
        assert!(output.chunks_exact(4).all(|pixel| pixel[3] == 255));
        let (changed, mean_delta) = change_stats(image.as_raw(), &output);
        assert!(changed > 100);
        assert!(
            mean_delta < 15.0,
            "texture-only transfer delta was {mean_delta}"
        );
    }

    #[test]
    fn rrdb_x2_and_x4_upscale_contracts_run_in_ort() {
        let input = vec![112u8; 16 * 16 * 4];
        for (id, scale) in [
            (ModelId::RealesrganRrdbX2, 2u32),
            (ModelId::RealesrganRrdb, 4u32),
        ] {
            let runner = LocalOnnxRunner::new(id);
            if !runner.available() {
                continue;
            }
            let (output, width, height) = runner
                .run_realesrgan_upscale(&input, 16, 16, scale, None, None)
                .unwrap();
            assert_eq!((width, height), (16 * scale, 16 * scale));
            assert_eq!(output.len(), (width * height * 4) as usize);
            assert!(output.chunks_exact(4).all(|pixel| pixel[3] == 112));
        }
    }

    #[test]
    #[ignore = "manual visual quality render"]
    fn render_full_ai_pipeline_quality_preview() {
        let sample = Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/Blake_Lively.jpg");
        if !sample.is_file() {
            return;
        }
        let image = image::open(sample).unwrap().to_rgba8();
        let output_dir = Path::new("tmp/retouch-quality");
        std::fs::create_dir_all(output_dir).unwrap();
        image.save(output_dir.join("blake-original.png")).unwrap();
        let strong = RetouchConfig {
            overall_amount: 100,
            denoise_amount: 100,
            color_amount: 100,
            face_restore_amount: 100,
            hair_detail_amount: 100,
            skin_amount: 100,
            eyes_amount: 100,
            lips_amount: 100,
            clothes_amount: 100,
            preview_masks: true,
            ..Default::default()
        };
        let no_color_strong = RetouchConfig {
            color_amount: 0,
            ..strong.clone()
        };
        for (name, config) in [
            (
                "default",
                RetouchConfig {
                    preview_masks: true,
                    ..Default::default()
                },
            ),
            ("strong", strong),
            ("no-color-strong", no_color_strong),
        ] {
            let result = run_pipeline(
                image.as_raw().clone(),
                image.width(),
                image.height(),
                config,
                Arc::new(Mutex::new(RetouchStatus::default())),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
            )
            .unwrap();
            RgbaImage::from_raw(result.width, result.height, result.rgba.clone())
                .unwrap()
                .save(output_dir.join(format!("blake-ai-{name}.png")))
                .unwrap();
            if let Some(preview) = result.mask_preview_rgba.clone() {
                RgbaImage::from_raw(result.width, result.height, preview)
                    .unwrap()
                    .save(output_dir.join(format!("blake-mask-{name}.png")))
                    .unwrap();
            }
            std::fs::write(
                output_dir.join(format!("blake-ai-{name}.txt")),
                format!(
                    "{}\n{}\n",
                    benchmark_line(&result),
                    result.warnings.join("\n")
                ),
            )
            .unwrap();
            println!("{name}: {}", benchmark_line(&result));
            for warning in &result.warnings {
                println!("{name} warning: {warning}");
            }
            assert!(result.benchmark.provider.contains("YuNet"));
            assert!(result.benchmark.provider.contains("BiSeNet"));
            assert!(result.benchmark.provider.contains("NAFNet"));
            if name != "no-color-strong" {
                assert!(result.benchmark.provider.contains("IAT"));
            }
            assert!(result.benchmark.provider.contains("GFPGAN"));
            assert!(result.benchmark.provider.contains("Real-ESRGAN General"));
        }
    }

    #[test]
    #[ignore = "manual 1024px/12MP/24MP CPU capacity benchmark"]
    fn benchmark_large_ai_pipeline_capacity() {
        let sample = Path::new("tmp/model-sources/gfpgan/inputs/whole_imgs/Blake_Lively.jpg");
        if !sample.is_file() {
            return;
        }
        let source = image::open(sample).unwrap().to_rgba8();
        let output_dir = Path::new("tmp/retouch-quality");
        std::fs::create_dir_all(output_dir).unwrap();
        for (name, width, height) in [
            ("1024px", 1024u32, 679u32),
            ("12mp", 4242u32, 2828u32),
            ("24mp", 6000u32, 4000u32),
        ] {
            let input = imageops::resize(&source, width, height, imageops::FilterType::CatmullRom);
            let result = run_pipeline(
                input.into_raw(),
                width,
                height,
                RetouchConfig {
                    // Capacity runs exercise every neural stage even when the
                    // source image is already clean.
                    auto_denoise: false,
                    ..Default::default()
                },
                Arc::new(Mutex::new(RetouchStatus::default())),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(HashMap::new())),
            )
            .unwrap();
            let line = benchmark_line(&result);
            println!("{name}: {line}");
            std::fs::write(
                output_dir.join(format!("capacity-{name}.txt")),
                format!("{line}\n{}\n", result.warnings.join("\n")),
            )
            .unwrap();
            assert_eq!((result.width, result.height), (width, height));
            assert!(result.benchmark.provider.contains("YuNet"));
            assert!(result.benchmark.provider.contains("BiSeNet"));
            assert!(result.benchmark.provider.contains("NAFNet"));
            assert!(result.benchmark.provider.contains("IAT"));
            assert!(result.benchmark.provider.contains("GFPGAN"));
            assert!(result.benchmark.provider.contains("Real-ESRGAN General"));
        }
    }
}
