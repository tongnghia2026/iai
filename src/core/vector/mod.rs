//! Vector core — the UI/GPU-independent foundation for integrated vector design
//! (see KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt, Mục 3.10 / 3.11 / Bước 1).
//!
//! Files are pre-partitioned so features grow by ADDING modules, never by
//! bloating the already-large existing files:
//!   affine   - AffineTransform                              [T1.1] DONE
//!   path     - PathData / Contour / Node / FillRule         [T1.2] DONE
//!   ops      - node/segment editing operations              [T1.3] DONE
//!   flatten  - adaptive Bézier flattening                   [T1.4] DONE
//!   hittest  - fill/stroke hit-testing                      [T1.5] DONE
//!   color    - ColorValue (RGB/CMYK/opacity)                [T2.1] DONE
//!   style    - VectorStyle / StrokeStyle                    [T2.2] DONE
//!   object   - VectorObjectData (what a Path layer holds)   [T4.1] DONE
//!   raster   - VectorObjectData -> RGBA cache (CPU)         [T4.1 min / T5.x full]
//!   from_shape - primitive geometry -> PathData (curves)    [Giai đoạn 4]
//! These stay declared here as they land; nothing above depends on UI or GPU.

pub mod affine;
pub mod color;
pub mod flatten;
pub mod from_shape;
pub mod hittest;
pub mod object;
pub mod ops;
pub mod path;
pub mod raster;
pub mod style;
