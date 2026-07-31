//! Hybrid vector-canvas building blocks.
//!
//! The raster tile map remains the authoritative display fallback.  Nothing in
//! this module mutates the vector model or its raster twin.

pub mod cache;
pub mod eligibility;
pub mod mesh;
pub mod renderer;
pub mod scene;
pub mod telemetry;

pub const VECTOR_SHADER: &str = include_str!("vector.wgsl");

/// Emergency switch. It is deliberately opt-in while the native draw pass is
/// being qualified on each backend; an unset/false value preserves the old
/// raster compositor byte-for-byte.
pub fn runtime_enabled() -> bool {
    std::env::var("IAI_GPU_VECTOR_CANVAS")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "on"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn vector_shader_parses() {
        naga::front::wgsl::parse_str(super::VECTOR_SHADER).expect("vector.wgsl must parse");
    }
}
