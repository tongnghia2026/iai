//! Colour for iAi: colour spaces and conversions, the CMS (lcms2: ICC
//! profiles, CMYK, the monitor profile), the working colour space, the
//! perceptual spaces (Oklab, CAM16-UCS) and gamut mapping.

pub mod cms;
pub mod color;
pub mod gamut_map;
pub mod perceptual_color;
pub mod ucs;
pub mod working_color;
