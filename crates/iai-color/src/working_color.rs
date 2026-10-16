//! Scene-linear working-space definitions and matrix transforms.
//!
//! Matrices are kept here as the single CPU source of truth. The GPU receives
//! the selected working-to-linear-sRGB matrix through its existing scene
//! transform payload, avoiding an independent set of shader constants.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkingColorSpace {
    /// Compatibility space for display-referred sources and legacy documents.
    LinearSrgb,
    /// ACES AP1 primaries. Default for new RAW scene masters.
    AcesCg,
    /// Linear ProPhoto RGB, retained for benchmark/reference conversion.
    #[default]
    LinearProPhoto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputColorSpace {
    Srgb,
    DisplayP3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorPipelineMetadata {
    pub working: WorkingColorSpace,
    pub output: OutputColorSpace,
    pub algorithm_version: u16,
}

impl Default for ColorPipelineMetadata {
    fn default() -> Self {
        Self {
            working: WorkingColorSpace::LinearProPhoto,
            output: OutputColorSpace::Srgb,
            algorithm_version: 2,
        }
    }
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

// D65 linear sRGB <-> ACEScg (AP1), including the D65/D60 adaptation.
const ACESCG_FROM_SRGB: [[f32; 3]; 3] = [
    [0.613_132_4, 0.339_538, 0.047_416_7],
    [0.070_124_4, 0.916_394, 0.013_451_5],
    [0.020_587_7, 0.109_574_6, 0.869_785_4],
];
const SRGB_FROM_ACESCG: [[f32; 3]; 3] = [
    [1.704_858_7, -0.621_716, -0.083_299_4],
    [-0.130_076_8, 1.140_735_7, -0.010_559_8],
    [-0.023_964_1, -0.128_975_5, 1.153_014],
];

// D65 linear sRGB <-> D50 linear ProPhoto, including Bradford adaptation.
const PROPHOTO_FROM_SRGB: [[f32; 3]; 3] = [
    [0.529_346, 0.330_073, 0.140_581],
    [0.098_374, 0.873_461, 0.028_165],
    [0.016_883, 0.117_673, 0.865_444],
];
const SRGB_FROM_PROPHOTO: [[f32; 3]; 3] = [
    [2.034_08, -0.727_334, -0.306_746],
    [-0.228_813, 1.231_73, -0.002_917],
    [-0.008_565, -0.153_273, 1.161_839],
];

#[inline]
pub fn apply_matrix(matrix: &[[f32; 3]; 3], rgb: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * rgb[0] + matrix[0][1] * rgb[1] + matrix[0][2] * rgb[2],
        matrix[1][0] * rgb[0] + matrix[1][1] * rgb[1] + matrix[1][2] * rgb[2],
        matrix[2][0] * rgb[0] + matrix[2][1] * rgb[1] + matrix[2][2] * rgb[2],
    ]
}

#[inline]
pub fn multiply_matrices(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (row, values) in out.iter_mut().enumerate() {
        for (column, value) in values.iter_mut().enumerate() {
            *value = a[row][0] * b[0][column] + a[row][1] * b[1][column] + a[row][2] * b[2][column];
        }
    }
    out
}

impl WorkingColorSpace {
    pub fn name(self) -> &'static str {
        match self {
            Self::LinearSrgb => "Linear sRGB (legacy)",
            Self::AcesCg => "ACEScg (AP1)",
            Self::LinearProPhoto => "Linear ProPhoto RGB",
        }
    }

    pub fn from_linear_srgb(self, rgb: [f32; 3]) -> [f32; 3] {
        apply_matrix(self.from_linear_srgb_matrix(), rgb)
    }

    pub fn to_linear_srgb(self, rgb: [f32; 3]) -> [f32; 3] {
        apply_matrix(self.to_linear_srgb_matrix(), rgb)
    }

    pub fn from_linear_srgb_matrix(self) -> &'static [[f32; 3]; 3] {
        match self {
            Self::LinearSrgb => &IDENTITY,
            Self::AcesCg => &ACESCG_FROM_SRGB,
            Self::LinearProPhoto => &PROPHOTO_FROM_SRGB,
        }
    }

    pub fn to_linear_srgb_matrix(self) -> &'static [[f32; 3]; 3] {
        match self {
            Self::LinearSrgb => &IDENTITY,
            Self::AcesCg => &SRGB_FROM_ACESCG,
            Self::LinearProPhoto => &SRGB_FROM_PROPHOTO,
        }
    }

    /// Y coefficients in the space's native reference system.
    pub fn luminance_coefficients(self) -> [f32; 3] {
        match self {
            Self::LinearSrgb => [0.212_672_9, 0.715_152_2, 0.072_175],
            Self::AcesCg => [0.272_228_7, 0.674_081_8, 0.053_689_5],
            Self::LinearProPhoto => [0.288_04, 0.711_874, 0.000_086],
        }
    }

    /// Coefficients used by the versioned render kernels. Linear-sRGB keeps
    /// the historical rounded Rec.709 values bit-for-bit; wide spaces use
    /// their native Y row.
    pub fn render_luminance_coefficients(self) -> [f32; 3] {
        match self {
            Self::LinearSrgb => [0.2126, 0.7152, 0.0722],
            _ => self.luminance_coefficients(),
        }
    }

    pub fn luminance(self, rgb: [f32; 3]) -> f32 {
        let c = self.luminance_coefficients();
        c[0] * rgb[0] + c[1] * rgb[1] + c[2] * rgb[2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_space_roundtrips_signed_hdr_vectors() {
        for space in [WorkingColorSpace::AcesCg, WorkingColorSpace::LinearProPhoto] {
            for rgb in [
                [0.0, 0.0, 0.0],
                [0.18; 3],
                [-0.1, 0.4, 2.5],
                [4.0, -0.2, 0.7],
            ] {
                let back = space.to_linear_srgb(space.from_linear_srgb(rgb));
                for channel in 0..3 {
                    assert!(
                        (back[channel] - rgb[channel]).abs() < 8.0e-5,
                        "{space:?}: {rgb:?} -> {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn neutral_axis_stays_neutral() {
        for space in [WorkingColorSpace::AcesCg, WorkingColorSpace::LinearProPhoto] {
            let value = space.from_linear_srgb([0.5; 3]);
            assert!((value[0] - value[1]).abs() < 8.0e-5 && (value[1] - value[2]).abs() < 8.0e-5);
        }
    }
}
