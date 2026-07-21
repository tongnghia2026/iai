//! Vector core — the UI/GPU-independent foundation for integrated vector design
//! (see KE_HOACH_PHAT_TRIEN_VECTOR_IAI.txt, Mục 3.10 / 3.11 / Bước 1).
//!
//! Files are pre-partitioned so features grow by ADDING modules, never by
//! bloating the already-large existing files:
//!   affine   - AffineTransform                              [T1.1] DONE
//!   path     - PathData / Contour / Node / FillRule         [T1.2] DONE
//!   ops      - node/segment editing operations              [T1.3] pending
//!   flatten  - adaptive Bézier flattening                   [T1.4] pending
//!   hittest  - fill/stroke hit-testing                      [T1.5] pending
//!   style    - VectorStyle / StrokeStyle                    [T2.2] pending
//!   color    - ColorValue (RGB/CMYK/opacity)                [T2.1] pending
//!   raster   - PathData -> TileMap cache                    [T5.x] pending
//! These stay declared here as they land; nothing above depends on UI or GPU.

pub mod affine;
pub mod path;
