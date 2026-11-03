//! The models iAi knows, in one table: what each is for, its family (the
//! shape of its inputs and outputs, which decides what code can run it), the
//! file and subfolder it is kept under, its size, its SHA-256, its licence and
//! whether that licence lets the author hand it out again.
//!
//! The rest of the crate asks this table, and `cargo xtask bundle-models`
//! includes this very file to build the model packs from it, which is why it
//! names nothing outside `std`. Where a model can be fetched from is not here
//! but in the list `docs/author/models.json` (see `list.rs`), which the author
//! can change without a new build.

/// One megabyte, as the sizes below are counted for people (10^6 bytes).
pub const MB: u64 = 1_000_000;

/// What a model does for the user: one row of the "Model AI" window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Feature {
    RemoveObject,
    SelectSubject,
    ReadText,
    FindFaces,
    FaceLandmarks,
    BodyPose,
    BodyParts,
    FaceRestore,
    Upscale,
}

impl Feature {
    pub const ALL: [Feature; 9] = [
        Feature::RemoveObject,
        Feature::SelectSubject,
        Feature::ReadText,
        Feature::FindFaces,
        Feature::FaceLandmarks,
        Feature::BodyPose,
        Feature::BodyParts,
        Feature::FaceRestore,
        Feature::Upscale,
    ];

    /// Its name in `models.json`.
    pub const fn id(self) -> &'static str {
        match self {
            Feature::RemoveObject => "remove-object",
            Feature::SelectSubject => "select-subject",
            Feature::ReadText => "read-text",
            Feature::FindFaces => "find-faces",
            Feature::FaceLandmarks => "face-landmarks",
            Feature::BodyPose => "body-pose",
            Feature::BodyParts => "body-parts",
            Feature::FaceRestore => "face-restore",
            Feature::Upscale => "upscale",
        }
    }

    pub fn from_id(id: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.id() == id)
    }

    /// What the user reads in the window.
    pub const fn label(self) -> &'static str {
        match self {
            Feature::RemoveObject => "Xóa vật thể bằng AI",
            Feature::SelectSubject => "Chọn chủ thể, tách nền",
            Feature::ReadText => "Đọc chữ trên căn cước",
            Feature::FindFaces => "Tìm khuôn mặt",
            Feature::FaceLandmarks => "Điểm mốc khuôn mặt (chỉnh chân dung)",
            Feature::BodyPose => "Dáng người (chỉnh dáng)",
            Feature::BodyParts => "Tách da, tóc, môi, răng (chân dung)",
            Feature::FaceRestore => "Phục hồi khuôn mặt",
            Feature::Upscale => "Làm nét, phóng to",
        }
    }

    /// Only the Pro build has the feature.
    pub const fn pro(self) -> bool {
        !matches!(self, Feature::RemoveObject | Feature::SelectSubject)
    }
}

/// A kind of model: the inputs and outputs a file must have for the code of
/// its slots to run it. A file of the family that is not one the table names
/// is still run, as the user's own ("tự đặt").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Lama,
    BiRefNet,
    PpOcrRec,
    YuNet,
    FaceMesh,
    BlazePose,
    Sapiens2Seg,
    FaceRestore,
    RealEsrgan,
}

impl Family {
    pub const ALL: [Family; 9] = [
        Family::Lama,
        Family::BiRefNet,
        Family::PpOcrRec,
        Family::YuNet,
        Family::FaceMesh,
        Family::BlazePose,
        Family::Sapiens2Seg,
        Family::FaceRestore,
        Family::RealEsrgan,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Family::Lama => "lama",
            Family::BiRefNet => "birefnet",
            Family::PpOcrRec => "ppocr-rec",
            Family::YuNet => "yunet",
            Family::FaceMesh => "face-mesh",
            Family::BlazePose => "blazepose",
            Family::Sapiens2Seg => "sapiens2-seg",
            Family::FaceRestore => "face-restore",
            Family::RealEsrgan => "real-esrgan",
        }
    }

    /// Its name for people.
    pub const fn label(self) -> &'static str {
        match self {
            Family::Lama => "LaMa",
            Family::BiRefNet => "BiRefNet",
            Family::PpOcrRec => "PP-OCR",
            Family::YuNet => "YuNet",
            Family::FaceMesh => "Face Mesh",
            Family::BlazePose => "BlazePose",
            Family::Sapiens2Seg => "Sapiens2",
            Family::FaceRestore => "GFPGAN / RestoreFormer",
            Family::RealEsrgan => "Real-ESRGAN",
        }
    }

    /// The subfolder a model of this family the table does not name is put
    /// in when it is installed: that of the family's first slot.
    pub fn dir(self) -> &'static str {
        MODELS
            .iter()
            .find(|m| m.family == self)
            .map_or("", |m| m.dir)
    }

    /// The slots a model of this family can fill.
    pub fn slots(self) -> impl Iterator<Item = Slot> {
        MODELS
            .iter()
            .filter(move |m| m.family == self)
            .map(|m| m.slot)
    }
}

/// A place in the code that loads a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    Lama,
    BiRefNetTiny,
    Ocr,
    FaceDetector,
    FaceMesh,
    Pose,
    BodyParts,
    Gfpgan,
    RealesrganGeneral,
    RealesrganRrdbX2,
    RealesrganRrdb,
}

impl Slot {
    pub const ALL: [Slot; 11] = [
        Slot::Lama,
        Slot::BiRefNetTiny,
        Slot::Ocr,
        Slot::FaceDetector,
        Slot::FaceMesh,
        Slot::Pose,
        Slot::BodyParts,
        Slot::Gfpgan,
        Slot::RealesrganGeneral,
        Slot::RealesrganRrdbX2,
        Slot::RealesrganRrdb,
    ];

    /// The model the table names for this slot.
    pub const fn model(self) -> &'static KnownModel {
        match self {
            Slot::Lama => &LAMA,
            Slot::BiRefNetTiny => &BIREFNET_TINY,
            Slot::Ocr => &PP_OCR_V4_REC,
            Slot::FaceDetector => &YUNET,
            Slot::FaceMesh => &FACE_MESH,
            Slot::Pose => &POSE,
            Slot::BodyParts => &SAPIENS2_SEG,
            Slot::Gfpgan => &GFPGAN,
            Slot::RealesrganGeneral => &REALESRGAN_GENERAL,
            Slot::RealesrganRrdbX2 => &REALESRGAN_X2,
            Slot::RealesrganRrdb => &REALESRGAN_X4,
        }
    }

    pub fn from_id(id: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|s| s.model().id == id)
    }
}

/// Which model pack (link 2) carries a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pack {
    /// The one pack of every model the author hands out (03/11: one folder,
    /// on the author's Drive, for the Pro pack's customers).
    Basic,
    /// In no pack: its licence does not let the author hand it out.
    None,
}

impl Pack {
    pub const fn id(self) -> &'static str {
        match self {
            Pack::Basic => "basic",
            Pack::None => "none",
        }
    }
}

/// A file a model cannot run without, kept beside it under this name: the
/// external weights of a model past protobuf's 2 GB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Companion {
    pub file: &'static str,
    /// Bytes, 0 when not yet measured.
    pub size: u64,
    pub approx_mb: u32,
    /// Lowercase hex, empty when not yet measured.
    pub sha256: &'static str,
}

/// A model of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownModel {
    pub slot: Slot,
    /// Its name in `models.json`.
    pub id: &'static str,
    /// Its name for people.
    pub name: &'static str,
    pub feature: Feature,
    pub family: Family,
    /// The subfolder of a model folder it is kept in ("" for the folder
    /// itself), as machines in use already have it.
    pub dir: &'static str,
    pub file: &'static str,
    /// Bytes, 0 when not yet measured on the author's file.
    pub size: u64,
    /// About how many megabytes, for people (0 when not known).
    pub approx_mb: u32,
    /// Lowercase hex, empty when not yet measured on the author's file.
    pub sha256: &'static str,
    pub license: &'static str,
    /// The licence's text, under `licenses/` of the repository; it goes into
    /// a model pack with the model. Empty for a model in no pack.
    pub license_file: &'static str,
    /// Its licence lets the author hand the file out (in a pack, from the
    /// author's own GitHub Releases or Drive). When not, the app does not
    /// fetch it either: it shows where to get it.
    pub redistributable: bool,
    pub pack: Pack,
    /// Only the Pro build runs it.
    pub pro: bool,
    pub companions: &'static [Companion],
}

impl KnownModel {
    /// The file's path under a model folder, `/`-separated.
    pub fn relative_path(&self) -> String {
        if self.dir.is_empty() {
            self.file.to_string()
        } else {
            format!("{}/{}", self.dir, self.file)
        }
    }

    /// Bytes of the model and what comes with it, the measured size where
    /// there is one, else the approximate one.
    pub fn total_bytes(&self) -> u64 {
        let one = |size: u64, approx: u32| {
            if size > 0 {
                size
            } else {
                u64::from(approx) * MB
            }
        };
        self.companions
            .iter()
            .fold(one(self.size, self.approx_mb), |sum, c| {
                sum + one(c.size, c.approx_mb)
            })
    }
}

/// Carve/LaMa-ONNX, opset 17, fixed 512×512 (`lama.rs`).
const LAMA: KnownModel = KnownModel {
    slot: Slot::Lama,
    id: "lama",
    name: "LaMa (xóa vật thể)",
    feature: Feature::RemoveObject,
    family: Family::Lama,
    dir: "",
    file: "lama_fp32.onnx",
    size: 208044816,
    approx_mb: 200,
    sha256: "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6",
    license: "Apache-2.0",
    license_file: "Apache-2.0.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: false,
    companions: &[],
};

/// BiRefNet general, Swin-tiny backbone, epoch 232: the release asset of the
/// authors' GitHub, hashed from a fresh download on 2026-10-07 and again on
/// 2026-11-02 (`select_subject.rs`).
const BIREFNET_TINY: KnownModel = KnownModel {
    slot: Slot::BiRefNetTiny,
    id: "birefnet-tiny",
    name: "BiRefNet Tiny (chọn chủ thể)",
    feature: Feature::SelectSubject,
    family: Family::BiRefNet,
    dir: "",
    file: "birefnet-general-tiny-epoch_232.onnx",
    size: 224_005_088,
    approx_mb: 224,
    sha256: "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333",
    license: "MIT",
    license_file: "BiRefNet-MIT.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: false,
    companions: &[],
};

/// PaddleOCR PP-OCRv4 line recogniser, the ONNX RapidOCR publishes (`ocr.rs`).
const PP_OCR_V4_REC: KnownModel = KnownModel {
    slot: Slot::Ocr,
    id: "ppocr-v4-rec",
    name: "PP-OCRv4 (đọc chữ)",
    feature: Feature::ReadText,
    family: Family::PpOcrRec,
    dir: "ocr",
    file: "ch_PP-OCRv4_rec_infer.onnx",
    size: 10857958,
    approx_mb: 11,
    sha256: "48fc40f24f6d2a207a2b1091d3437eb3cc3eb6b676dc3ef9c37384005483683b",
    license: "Apache-2.0",
    license_file: "Apache-2.0.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

/// OpenCV Zoo YuNet at commit 47534e27; size and hash measured on the LFS
/// file of that commit on 2026-11-02 (`retouch.rs`).
const YUNET: KnownModel = KnownModel {
    slot: Slot::FaceDetector,
    id: "yunet",
    name: "YuNet (tìm khuôn mặt)",
    feature: Feature::FindFaces,
    family: Family::YuNet,
    dir: "face-detector",
    file: "face_detection_yunet_2026may.onnx",
    size: 229_738,
    approx_mb: 1,
    sha256: "ebafce4e3c118d6554634be5c27ab333b4c047a9a8c3faf1d7cf93101c22f0f0",
    license: "MIT",
    license_file: "YuNet-MIT.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

/// MediaPipe Face Mesh V2, converted by the author from Google's
/// `face_landmarker.task` (`face_mesh.rs`, `docs/AI_MODELS.md`).
const FACE_MESH: KnownModel = KnownModel {
    slot: Slot::FaceMesh,
    id: "face-mesh",
    name: "Face Mesh (478 điểm mốc mặt)",
    feature: Feature::FaceLandmarks,
    family: Family::FaceMesh,
    dir: "face-mesh",
    file: "face_landmarks_detector.onnx",
    size: 4920995,
    approx_mb: 0,
    sha256: "58b89dbbaca1d944bdca16ea3c3a0853c74d473c9b789f76d1ead580e59ed4cd",
    license: "Apache-2.0",
    license_file: "Apache-2.0.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

/// MediaPipe Pose (BlazePose GHUM heavy), converted by the author (`pose.rs`).
/// No hash was recorded for it yet: until the author adds one, the file is
/// taken by its name and shape, never as checked.
const POSE: KnownModel = KnownModel {
    slot: Slot::Pose,
    id: "pose",
    name: "BlazePose (dáng người)",
    feature: Feature::BodyPose,
    family: Family::BlazePose,
    dir: "pose",
    file: "pose_landmarks_detector.onnx",
    size: 55181696,
    approx_mb: 55,
    sha256: "e8c01bf5015def4137637fe38d65033ad6392a18ff8573dce5a22b247192d02f",
    license: "Apache-2.0",
    license_file: "Apache-2.0.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

/// Meta Sapiens2 seg 0.4B, exported by the author at 512×384 with its weights
/// beside it (`body_parts.rs`, `scripts/export_sapiens2_seg_onnx.py`).
const SAPIENS2_SEG: KnownModel = KnownModel {
    slot: Slot::BodyParts,
    id: "sapiens2-seg",
    name: "Sapiens2 (tách vùng cơ thể)",
    feature: Feature::BodyParts,
    family: Family::Sapiens2Seg,
    dir: "sapiens2-seg",
    file: "sapiens2_seg_0.4b_512x384.onnx",
    size: 5550800,
    approx_mb: 6,
    sha256: "90b5d000ef43eec947486bcca0d6419412a59336032801b34af4c2f30782e1f9",
    license: "Sapiens2 License",
    license_file: "Sapiens2-LICENSE.md",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[Companion {
        file: "sapiens2_seg_0.4b_512x384.onnx.data",
        size: 1626537984,
        approx_mb: 1600,
        sha256: "1bb7df2dbb1b0c513cab993282c012065c668090869a8a4a4afcc3dece1f3f93",
    }],
};

/// Tencent ARC GFPGAN v1.4 (Apache-2.0; its upstream notice lists third-party
/// terms). The author hands it out in the pack all the same, to take it down
/// if its makers ask (03/11) (`retouch.rs`).
const GFPGAN: KnownModel = KnownModel {
    slot: Slot::Gfpgan,
    id: "gfpgan-1.4",
    name: "GFPGAN v1.4 (phục hồi mặt)",
    feature: Feature::FaceRestore,
    family: Family::FaceRestore,
    dir: "gfpgan",
    file: "GFPGANv1.4.onnx",
    size: 340357897,
    approx_mb: 340,
    sha256: "0dac008dbd9c025b9a0acff35e769792a7b8b7c52464d17d3981369834803087",
    license: "Apache-2.0",
    license_file: "Apache-2.0.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

/// xinntao Real-ESRGAN, exported by the author from the official checkpoints.
const REALESRGAN_GENERAL: KnownModel = KnownModel {
    slot: Slot::RealesrganGeneral,
    id: "realesrgan-general-x4v3",
    name: "Real-ESRGAN General x4v3 (làm nét nhanh)",
    feature: Feature::Upscale,
    family: Family::RealEsrgan,
    dir: "realesrgan",
    file: "RealESRGANv3-general-x4v3.onnx",
    size: 4866645,
    approx_mb: 5,
    sha256: "ac0920068b2c43da1944e788b1728e88b5e2b005a225e29c4c4c4e029bdbe926",
    license: "BSD-3-Clause",
    license_file: "Real-ESRGAN-BSD-3-Clause.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

const REALESRGAN_X2: KnownModel = KnownModel {
    slot: Slot::RealesrganRrdbX2,
    id: "realesrgan-x2plus",
    name: "Real-ESRGAN x2plus (làm nét tự nhiên)",
    feature: Feature::Upscale,
    family: Family::RealEsrgan,
    dir: "realesrgan",
    file: "RealESRGAN_x2plus.onnx",
    size: 67076397,
    approx_mb: 67,
    sha256: "517dcc8f74067504bee3be68fdae3ca399b52870a0dec6e8de7f8a2654dfdfed",
    license: "BSD-3-Clause",
    license_file: "Real-ESRGAN-BSD-3-Clause.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

const REALESRGAN_X4: KnownModel = KnownModel {
    slot: Slot::RealesrganRrdb,
    id: "realesrgan-x4plus",
    name: "Real-ESRGAN x4plus (phóng to x4)",
    feature: Feature::Upscale,
    family: Family::RealEsrgan,
    dir: "realesrgan",
    file: "RealESRGAN_x4plus.onnx",
    size: 67051889,
    approx_mb: 67,
    sha256: "b644aab83761a02489ef1cfd581a4ac677ba43380adb6a8c3c0a3ad801c9f17e",
    license: "BSD-3-Clause",
    license_file: "Real-ESRGAN-BSD-3-Clause.txt",
    redistributable: true,
    pack: Pack::Basic,
    pro: true,
    companions: &[],
};

/// The table, one row a slot, in [`Slot::ALL`] order.
pub const MODELS: &[KnownModel] = &[
    LAMA,
    BIREFNET_TINY,
    PP_OCR_V4_REC,
    YUNET,
    FACE_MESH,
    POSE,
    SAPIENS2_SEG,
    GFPGAN,
    REALESRGAN_GENERAL,
    REALESRGAN_X2,
    REALESRGAN_X4,
];

/// File endings of a model's weights before they were converted to ONNX:
/// the app cannot run any of them.
pub const NOT_CONVERTED: &[&str] = &[
    "pth",
    "pt",
    "safetensors",
    "ckpt",
    "bin",
    "tflite",
    "task",
    "pb",
    "h5",
    "keras",
    "pdparams",
    "gguf",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_has_one_row_a_slot_in_order_and_the_slot_finds_it() {
        assert_eq!(MODELS.len(), Slot::ALL.len());
        for (row, slot) in MODELS.iter().zip(Slot::ALL) {
            assert_eq!(row.slot, slot);
            assert_eq!(slot.model(), row);
            assert_eq!(Slot::from_id(row.id), Some(slot));
        }
    }

    #[test]
    fn every_row_is_well_formed() {
        let mut ids = std::collections::HashSet::new();
        for m in MODELS {
            assert!(ids.insert(m.id), "{} twice", m.id);
            let hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
            assert!(m.sha256.is_empty() || hex(m.sha256), "{}", m.id);
            for c in m.companions {
                assert!(c.sha256.is_empty() || hex(c.sha256), "{}", c.file);
            }
            assert!(m.file.ends_with(".onnx"), "{}", m.id);
            // What is handed out goes with its licence; what may not be is
            // in no pack.
            assert_eq!(m.redistributable, m.pack != Pack::None, "{}", m.id);
            assert_eq!(m.redistributable, !m.license_file.is_empty(), "{}", m.id);
            assert_eq!(m.pro, m.feature.pro(), "{}", m.id);
        }
    }

    #[test]
    fn the_subfolders_are_the_ones_machines_in_use_have() {
        let folders: Vec<(&str, &str)> = MODELS.iter().map(|m| (m.id, m.dir)).collect();
        assert_eq!(
            folders,
            [
                ("lama", ""),
                ("birefnet-tiny", ""),
                ("ppocr-v4-rec", "ocr"),
                ("yunet", "face-detector"),
                ("face-mesh", "face-mesh"),
                ("pose", "pose"),
                ("sapiens2-seg", "sapiens2-seg"),
                ("gfpgan-1.4", "gfpgan"),
                ("realesrgan-general-x4v3", "realesrgan"),
                ("realesrgan-x2plus", "realesrgan"),
                ("realesrgan-x4plus", "realesrgan"),
            ]
        );
        assert_eq!(Family::RealEsrgan.dir(), "realesrgan");
        assert_eq!(Family::RealEsrgan.slots().count(), 3);
    }
}
