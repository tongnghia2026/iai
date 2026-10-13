//! Hybrid vector-canvas building blocks.
//!
//! The raster tile map remains the authoritative display fallback.  Nothing in
//! this module mutates the vector model or its raster twin.

pub mod cache;
pub mod composite;
pub mod eligibility;
pub mod mesh;
pub mod renderer;
pub mod scene;
pub mod telemetry;

pub const VECTOR_SHADER: &str = include_str!("vector.wgsl");
pub const VECTOR_COMPOSITE_SHADER: &str = include_str!("vector_composite.wgsl");

fn runtime_enabled_value(value: Option<&str>) -> bool {
    value.is_none_or(|value| !matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "off"))
}

/// Production switch. The qualified hybrid path is enabled by default; setting
/// `IAI_GPU_VECTOR_CANVAS=0`, `false`, or `off` restores the old raster
/// compositor byte-for-byte as an emergency fallback.
pub fn runtime_enabled() -> bool {
    let value = std::env::var("IAI_GPU_VECTOR_CANVAS").ok();
    runtime_enabled_value(value.as_deref())
}

#[cfg(test)]
mod tests {
    #[test]
    fn vector_canvas_defaults_on_and_accepts_explicit_off_values() {
        assert!(super::runtime_enabled_value(None));
        for value in ["1", "true", "on", "TRUE"] {
            assert!(super::runtime_enabled_value(Some(value)), "{value}");
        }
        for value in ["0", "false", "off", "FALSE"] {
            assert!(!super::runtime_enabled_value(Some(value)), "{value}");
        }
    }

    #[test]
    fn vector_shader_parses() {
        naga::front::wgsl::parse_str(super::VECTOR_SHADER).expect("vector.wgsl must parse");
    }

    #[test]
    fn vector_composite_shader_parses() {
        let module = naga::front::wgsl::parse_str(super::VECTOR_COMPOSITE_SHADER)
            .expect("vector_composite.wgsl must parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("vector_composite.wgsl must validate");
    }
}
