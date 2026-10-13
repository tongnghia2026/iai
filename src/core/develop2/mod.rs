//! Develop Engine 2 typed/versioned processing graph.
//!
//! This is an independent orchestration layer. Version 2 initially adopts the
//! already-tested iAi scene kernels behind explicit contracts; replacing a
//! kernel no longer requires changing UI, history, masks, or document code.

pub mod color;
pub mod scopes;

use crate::core::develop::{DevelopEngineVersion, DevelopSettings};
use crate::core::develop_scene::{BaseLook, SceneSource};
use crate::core::tile::TileMap;
use crate::core::working_color::WorkingColorSpace;

pub const GRAPH_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorModel {
    LinearProPhoto,
    LinearAcesCg,
    LinearSrgb,
    EncodedSrgb,
}

impl ColorModel {
    /// The colorimetric primaries backing this scene/display color model.
    pub fn primaries(self) -> color::RgbPrimaries {
        match self {
            ColorModel::LinearProPhoto => color::PROPHOTO,
            ColorModel::LinearAcesCg => color::ACESCG,
            ColorModel::LinearSrgb | ColorModel::EncodedSrgb => color::SRGB,
        }
    }
}

/// Map a scene master's working space onto the graph's typed color model.
fn scene_color_model(working: WorkingColorSpace) -> ColorModel {
    match working {
        WorkingColorSpace::LinearProPhoto => ColorModel::LinearProPhoto,
        WorkingColorSpace::AcesCg => ColorModel::LinearAcesCg,
        WorkingColorSpace::LinearSrgb => ColorModel::LinearSrgb,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceDomain {
    Scene,
    Display,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Precision {
    F16StorageF32Compute,
    F32,
    U16Sink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferContract {
    pub color: ColorModel,
    pub domain: ReferenceDomain,
    pub precision: Precision,
    pub signed: bool,
    pub bounded: bool,
}

impl BufferContract {
    /// A scene master with the given actual color model. RAW masters are
    /// linear ProPhoto; display-referred layers are linear sRGB. The graph now
    /// describes the real color model instead of assuming ProPhoto for both.
    pub const fn scene_master(color: ColorModel) -> Self {
        Self {
            color,
            domain: ReferenceDomain::Scene,
            precision: Precision::F16StorageF32Compute,
            signed: true,
            bounded: false,
        }
    }

    pub const SCENE_MASTER: Self = Self::scene_master(ColorModel::LinearProPhoto);
    pub const fn display_linear(color: ColorModel) -> Self {
        Self {
            color,
            domain: ReferenceDomain::Display,
            precision: Precision::F32,
            signed: true,
            bounded: false,
        }
    }
    pub const DISPLAY_LINEAR: Self = Self {
        color: ColorModel::LinearSrgb,
        domain: ReferenceDomain::Display,
        precision: Precision::F32,
        signed: true,
        bounded: false,
    };
    pub const DISPLAY_SINK: Self = Self {
        color: ColorModel::EncodedSrgb,
        domain: ReferenceDomain::Display,
        precision: Precision::U16Sink,
        signed: false,
        bounded: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    Input = 0,
    SceneTechnical = 1,
    RenderTransform = 2,
    Creative = 3,
    Spatial = 4,
    Output = 5,
    Encode = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    SceneInput,
    WhiteBalance,
    Exposure,
    ToneZones,
    NaturalRender,
    PerceptualColor,
    CurvesAndGrading,
    LocalAndDetail,
    OutputGamut,
    SrgbEncode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeSpec {
    pub kind: NodeKind,
    pub version: u16,
    pub stage: Stage,
    pub input: BufferContract,
    pub output: BufferContract,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevelopGraph {
    pub schema_version: u16,
    pub engine_version: DevelopEngineVersion,
    pub nodes: Vec<NodeSpec>,
    pub signature: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    UnsupportedSchema(u16),
    WrongEngine(DevelopEngineVersion),
    Empty,
    StageOrder { previous: Stage, next: Stage },
    ContractMismatch { at: usize },
    MissingOutputBoundary,
    InputBoundary(BoundaryError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryError {
    /// The input→connection transform contained a non-finite coefficient.
    NonFinite,
    /// The adopted white did not resolve to the D50 connection white.
    WhitePointDrift,
}

/// How the scene master's color characterization was established at the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputProvenance {
    /// RAW scene master built from the camera matrix into a linear working space.
    RawCameraMatrix,
    /// Display-referred layer linearized under the sRGB assumption (no embedded
    /// ICC has been consulted at this boundary yet).
    DisplayReferredAssumedSrgb,
}

/// A profile-aware description of the input/scene boundary: which color model
/// the scene master really uses, how that was determined, and the colorimetric
/// transform into the D50 profile-connection space (CIE XYZ). This is used to
/// validate the boundary and, in future phases, to feed camera characterization
/// and profile-aware gamut work. It does not itself alter rendered pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputBoundary {
    pub source: ColorModel,
    pub provenance: InputProvenance,
    /// Source linear RGB → CIE XYZ adapted to D50, quantized to render f32.
    pub to_pcs_xyz_d50: [[f32; 3]; 3],
}

impl InputBoundary {
    pub fn validate(&self) -> Result<(), GraphError> {
        if self
            .to_pcs_xyz_d50
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(GraphError::InputBoundary(BoundaryError::NonFinite));
        }
        // Neutral source white must resolve to the D50 connection white.
        let white = crate::core::working_color::apply_matrix(&self.to_pcs_xyz_d50, [1.0, 1.0, 1.0]);
        let target = color::xyz_from_xy(color::D50);
        if (0..3).any(|i| (white[i] as f64 - target[i]).abs() > 1.5e-3) {
            return Err(GraphError::InputBoundary(BoundaryError::WhitePointDrift));
        }
        Ok(())
    }
}

/// Describe the profile-aware input boundary for a concrete scene master.
pub fn describe_input_boundary(scene: &SceneSource) -> InputBoundary {
    let source = scene_color_model(scene.color_pipeline.working);
    let provenance = match scene.look {
        BaseLook::Raw => InputProvenance::RawCameraMatrix,
        BaseLook::Identity => InputProvenance::DisplayReferredAssumedSrgb,
    };
    let to_pcs_xyz_d50 = color::to_f32(&color::rgb_to_pcs_xyz_d50(source.primaries()));
    InputBoundary {
        source,
        provenance,
        to_pcs_xyz_d50,
    }
}

impl DevelopGraph {
    pub fn validate(&self) -> Result<(), GraphError> {
        if self.schema_version != GRAPH_SCHEMA_VERSION {
            return Err(GraphError::UnsupportedSchema(self.schema_version));
        }
        if self.engine_version != DevelopEngineVersion::Develop3 {
            return Err(GraphError::WrongEngine(self.engine_version));
        }
        if self.nodes.is_empty() {
            return Err(GraphError::Empty);
        }
        for (index, pair) in self.nodes.windows(2).enumerate() {
            if pair[1].stage < pair[0].stage {
                return Err(GraphError::StageOrder {
                    previous: pair[0].stage,
                    next: pair[1].stage,
                });
            }
            if pair[0].output != pair[1].input {
                return Err(GraphError::ContractMismatch { at: index + 1 });
            }
        }
        if self.nodes.last().map(|node| node.output) != Some(BufferContract::DISPLAY_SINK) {
            return Err(GraphError::MissingOutputBoundary);
        }
        Ok(())
    }
}

/// Compile the public UI snapshot into the canonical Develop3 recipe. This
/// assumes the default RAW ProPhoto scene master; use [`compile_for_scene`] when
/// the concrete scene master's working space is known.
pub fn compile(settings: &DevelopSettings) -> Result<DevelopGraph, GraphError> {
    compile_with_scene_color(settings, ColorModel::LinearProPhoto)
}

/// Compile the recipe for a concrete scene master, so the graph's scene-stage
/// contracts describe the real color model (ProPhoto for RAW, sRGB for a
/// display-referred layer) rather than a fixed assumption.
pub fn compile_for_scene(
    settings: &DevelopSettings,
    scene: &SceneSource,
) -> Result<DevelopGraph, GraphError> {
    compile_with_scene_color(settings, scene_color_model(scene.color_pipeline.working))
}

fn compile_with_scene_color(
    settings: &DevelopSettings,
    scene_color: ColorModel,
) -> Result<DevelopGraph, GraphError> {
    if settings.develop_engine_version != DevelopEngineVersion::Develop3 {
        return Err(GraphError::WrongEngine(settings.develop_engine_version));
    }
    let scene = BufferContract::scene_master(scene_color);
    let display_working = BufferContract::display_linear(scene_color);
    let display = BufferContract::DISPLAY_LINEAR;
    let nodes = vec![
        node(NodeKind::SceneInput, Stage::Input, scene, scene, true),
        node(
            NodeKind::WhiteBalance,
            Stage::SceneTechnical,
            scene,
            scene,
            settings.temperature.abs() > 0.001 || settings.tint.abs() > 0.001,
        ),
        node(
            NodeKind::Exposure,
            Stage::SceneTechnical,
            scene,
            scene,
            settings.exposure.abs() > 0.001,
        ),
        node(
            NodeKind::ToneZones,
            Stage::SceneTechnical,
            scene,
            scene,
            settings.highlights.abs() > 0.001
                || settings.shadows.abs() > 0.001
                || settings.midtones.abs() > 0.001
                || settings.whites.abs() > 0.001
                || settings.blacks.abs() > 0.001,
        ),
        node(
            NodeKind::NaturalRender,
            Stage::RenderTransform,
            scene,
            display_working,
            true,
        ),
        node(
            NodeKind::PerceptualColor,
            Stage::Creative,
            display_working,
            display_working,
            settings.saturation.abs() > 0.001
                || settings.vibrance.abs() > 0.001
                || settings.mixer_hue.iter().any(|v| v.abs() > 0.001)
                || settings.mixer_saturation.iter().any(|v| v.abs() > 0.001)
                || settings.mixer_luminance.iter().any(|v| v.abs() > 0.001),
        ),
        node(
            NodeKind::CurvesAndGrading,
            Stage::Creative,
            display_working,
            display_working,
            settings.contrast.abs() > 0.001
                || settings.grade_shadow_strength.abs() > 0.001
                || settings.grade_highlight_strength.abs() > 0.001
                || settings.curve_highlights.abs() > 0.001
                || settings.curve_lights.abs() > 0.001
                || settings.curve_darks.abs() > 0.001
                || settings.curve_shadows.abs() > 0.001,
        ),
        node(
            NodeKind::LocalAndDetail,
            Stage::Spatial,
            display_working,
            display_working,
            settings.has_spatial_effects() || settings.has_detail() || settings.has_locals(),
        ),
        node(
            NodeKind::OutputGamut,
            Stage::Output,
            display_working,
            display,
            true,
        ),
        node(
            NodeKind::SrgbEncode,
            Stage::Encode,
            display,
            BufferContract::DISPLAY_SINK,
            true,
        ),
    ];
    let signature = graph_signature(settings, &nodes);
    let graph = DevelopGraph {
        schema_version: GRAPH_SCHEMA_VERSION,
        engine_version: settings.develop_engine_version,
        nodes,
        signature,
    };
    graph.validate()?;
    Ok(graph)
}

fn node(
    kind: NodeKind,
    stage: Stage,
    input: BufferContract,
    output: BufferContract,
    active: bool,
) -> NodeSpec {
    NodeSpec {
        kind,
        version: 1,
        stage,
        input,
        output,
        active,
    }
}

fn graph_signature(settings: &DevelopSettings, nodes: &[NodeSpec]) -> u64 {
    // Stable FNV-1a over the canonical serialized settings plus node schema.
    let mut hash = 0xcbf29ce484222325u64;
    let json = serde_json::to_vec(settings).unwrap_or_default();
    for byte in json.into_iter().chain(nodes.iter().flat_map(|node| {
        [
            node.kind as u8,
            node.version as u8,
            node.stage as u8,
            node.active as u8,
            // Fold in the buffer color models so recipes for different scene
            // masters (RAW ProPhoto vs. display-referred sRGB) do not collide.
            node.input.color as u8,
            node.output.color as u8,
        ]
    })) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Execute one full-resolution scene graph. Preview and export callers reach
/// this same entry point; proxy policy remains outside the color semantics.
///
/// The graph is compiled for the concrete scene master so its scene-stage
/// contracts describe the real color model, and the profile-aware input
/// boundary is validated before rendering. Pixel production still adopts the
/// proven Scene1 kernels, so output is unchanged while the contract layer
/// becomes color-accurate.
pub fn execute_scene(
    scene: &SceneSource,
    settings: &DevelopSettings,
    selection: Option<crate::core::develop::DevelopSelection>,
) -> Result<TileMap, GraphError> {
    let graph = compile_for_scene(settings, scene)?;
    describe_input_boundary(scene).validate()?;
    debug_assert!(graph.nodes.iter().any(|n| n.kind == NodeKind::SrgbEncode));
    Ok(crate::core::develop_scene::apply_scene1_to_tilemap(
        scene, settings, selection,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_recipe_has_one_declared_output_boundary() {
        let graph = compile(&DevelopSettings::default()).unwrap();
        assert_eq!(
            graph.nodes.first().unwrap().input,
            BufferContract::SCENE_MASTER
        );
        assert_eq!(
            graph.nodes.last().unwrap().output,
            BufferContract::DISPLAY_SINK
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| n.stage == Stage::Encode)
                .count(),
            1
        );
    }

    #[test]
    fn signature_is_stable_and_parameter_sensitive() {
        let a = DevelopSettings::default();
        let mut b = a.clone();
        b.exposure = 10.0;
        assert_eq!(
            compile(&a).unwrap().signature,
            compile(&a).unwrap().signature
        );
        assert_ne!(
            compile(&a).unwrap().signature,
            compile(&b).unwrap().signature
        );
    }

    #[test]
    fn develop3_midtones_only_activates_tone_zones_and_changes_signature() {
        let mut neutral = DevelopSettings {
            develop_engine_version: DevelopEngineVersion::Develop3,
            ..Default::default()
        };
        let neutral_graph = compile(&neutral).unwrap();
        neutral.midtones = 50.0;
        let graph = compile(&neutral).unwrap();
        let tone = graph
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::ToneZones)
            .expect("canonical graph must contain ToneZones");
        assert!(tone.active, "Midtones-only must activate ToneZones");
        assert_ne!(graph.signature, neutral_graph.signature);
    }

    #[test]
    fn validation_rejects_a_gamma_encoded_scene_connection() {
        let mut graph = compile(&DevelopSettings::default()).unwrap();
        graph.nodes[1].input = BufferContract::DISPLAY_SINK;
        assert_eq!(
            graph.validate(),
            Err(GraphError::ContractMismatch { at: 1 })
        );
    }

    #[test]
    fn raw_scene_master_boundary_is_prophoto_camera() {
        let scene = SceneSource::new(4, 4);
        let boundary = describe_input_boundary(&scene);
        assert_eq!(boundary.source, ColorModel::LinearProPhoto);
        assert_eq!(boundary.provenance, InputProvenance::RawCameraMatrix);
        assert_eq!(boundary.validate(), Ok(()));

        let graph = compile_for_scene(&DevelopSettings::default(), &scene).unwrap();
        assert_eq!(
            graph.nodes.first().unwrap().input.color,
            ColorModel::LinearProPhoto
        );
        assert_eq!(graph.validate(), Ok(()));
    }

    #[test]
    fn display_referred_scene_master_boundary_is_srgb() {
        let mut scene = SceneSource::new(4, 4);
        scene.look = BaseLook::Identity;
        scene.color_pipeline.working = WorkingColorSpace::LinearSrgb;
        let boundary = describe_input_boundary(&scene);
        assert_eq!(boundary.source, ColorModel::LinearSrgb);
        assert_eq!(
            boundary.provenance,
            InputProvenance::DisplayReferredAssumedSrgb
        );
        assert_eq!(boundary.validate(), Ok(()));

        let settings = DevelopSettings::default();
        let graph = compile_for_scene(&settings, &scene).unwrap();
        assert_eq!(
            graph.nodes.first().unwrap().input.color,
            ColorModel::LinearSrgb
        );
        assert_eq!(graph.validate(), Ok(()));
        // A display-referred recipe must not collide with the RAW ProPhoto one.
        assert_ne!(graph.signature, compile(&settings).unwrap().signature);
    }

    #[test]
    fn input_boundary_rejects_non_finite_and_white_drift() {
        let mut nan = describe_input_boundary(&SceneSource::new(1, 1));
        nan.to_pcs_xyz_d50[0][0] = f32::NAN;
        assert_eq!(
            nan.validate(),
            Err(GraphError::InputBoundary(BoundaryError::NonFinite))
        );

        let identity = InputBoundary {
            source: ColorModel::LinearSrgb,
            provenance: InputProvenance::DisplayReferredAssumedSrgb,
            to_pcs_xyz_d50: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert_eq!(
            identity.validate(),
            Err(GraphError::InputBoundary(BoundaryError::WhitePointDrift))
        );
    }
}
