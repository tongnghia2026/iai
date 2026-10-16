//! iAi on the GPU (wgpu), without a window: the layer compositor and its
//! tile atlas, the vector renderer, the Develop detail and guided filters, and
//! how an instance is asked for. Drawing to a window and egui stay in the app.

#![forbid(unsafe_code)]

pub mod compositor;
pub mod detail_gpu;
pub mod guided_gpu;
pub mod tile_atlas;
pub mod vector;

/// The graphics backends to look for an adapter on. `IAI_GPU_BACKEND` (`dx12`,
/// `vulkan`, `gl`, or several separated by commas) narrows the choice: the way
/// round a driver that misbehaves, and how the OpenGL path gets tested on a
/// machine that would never pick it.
pub fn wanted_backends() -> wgpu::Backends {
    std::env::var("IAI_GPU_BACKEND")
        .ok()
        .map(|list| wgpu::Backends::from_comma_list(&list))
        .filter(|backends| !backends.is_empty())
        .unwrap_or(wgpu::Backends::all())
}

/// How the app and its GPU tests ask wgpu for an instance. wgpu's own
/// switches still apply on top (`WGPU_DX12_COMPILER`, `WGPU_DEBUG`,
/// `WGPU_VALIDATION`): they are how one machine is made to take the path of
/// another when a report comes from a machine nobody here can look at.
pub fn instance_descriptor() -> wgpu::InstanceDescriptor {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wanted_backends();
    desc.flags = desc.flags.with_env();
    // Said outright, where wgpu's own default would take any `dxcompiler.dll`
    // it finds on the PATH.
    desc.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
    desc.backend_options = desc.backend_options.with_env();
    desc
}

/// Whether an instance made from `desc` compiles DX12 shaders with FXC, the
/// compiler Windows ships: old, and minutes over a shader the size of the
/// compositor's (see `compositor::shader_without_develop`).
pub fn compiles_with_fxc(desc: &wgpu::InstanceDescriptor) -> bool {
    matches!(
        desc.backend_options.dx12.shader_compiler,
        wgpu::Dx12Compiler::Fxc
    )
}

/// The instance for an adapter that turned out to be DX12 with FXC behind it
/// (a card whose driver offers no Vulkan): DX12 alone, with FXC's optimiser
/// off. wgpu only passes FXC "skip optimisation" along with its `DEBUG` flag,
/// which otherwise adds names and debug information and no validation.
pub fn fxc_instance_descriptor() -> wgpu::InstanceDescriptor {
    let mut desc = instance_descriptor();
    desc.backends = wgpu::Backends::DX12;
    desc.flags = (desc.flags | wgpu::InstanceFlags::DEBUG).with_env();
    desc
}

/// Whether a shader is one the OpenGL and DX12 (FXC) backends will take:
/// for this crate's tests, and the app's (feature `shader-check`).
#[cfg(any(test, feature = "shader-check"))]
pub mod shader_validation {
    /// Translate every entry point of `src` to GLSL, the way the OpenGL
    /// backend does when it builds a pipeline. A machine whose only driver is
    /// OpenGL (an older Intel chip with neither Vulkan nor DX12) gets no
    /// pipeline from a shader refused here, though it is valid WGSL: nothing
    /// that pipeline draws ever shows, with no error on screen. Seen 11/10/2026
    /// as "the canvas shows only the checkerboard".
    pub fn translate_to_glsl(src: &str) -> Result<(), String> {
        let module = naga::front::wgsl::parse_str(src).map_err(|e| e.emit_to_string(src))?;
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .map_err(|e| format!("{e:?}"))?;
        // OpenGL 4.3: the oldest with the storage buffers the compositor needs.
        let options = naga::back::glsl::Options {
            version: naga::back::glsl::Version::Desktop(430),
            ..Default::default()
        };
        for entry in &module.entry_points {
            let pipeline_options = naga::back::glsl::PipelineOptions {
                shader_stage: entry.stage,
                entry_point: entry.name.clone(),
                multiview: None,
            };
            let mut glsl = String::new();
            naga::back::glsl::Writer::new(
                &mut glsl,
                &module,
                &info,
                &options,
                &pipeline_options,
                naga::proc::BoundsCheckPolicies::default(),
            )
            .and_then(|mut writer| writer.write())
            .map_err(|e| format!("entry point {}: {e}", entry.name))?;
        }
        Ok(())
    }

    pub fn assert_opengl_takes(label: &str, src: &str) {
        if let Err(why) = translate_to_glsl(src) {
            panic!("{label}: OpenGL cannot take this shader: {why}");
        }
    }

    /// `src` as the HLSL the DX12 backend hands its shader compiler for
    /// `entry`, near enough to time and bisect that compiler by hand.
    pub fn hlsl_of(src: &str, stage: naga::ShaderStage, entry: &str) -> String {
        let module = naga::front::wgsl::parse_str(src).expect("parse");
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("validate");
        let options = naga::back::hlsl::Options {
            shader_model: naga::back::hlsl::ShaderModel::V5_1,
            fake_missing_bindings: true,
            ..Default::default()
        };
        let pipeline_options = naga::back::hlsl::PipelineOptions {
            entry_point: Some((stage, entry.to_string())),
        };
        let mut hlsl = String::new();
        naga::back::hlsl::Writer::new(&mut hlsl, &options, &pipeline_options)
            .write(&module, &info, None)
            .expect("HLSL");
        hlsl
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// How long the pipelines the first frame waits for take to build, on the
        /// device the environment picks. The app shows no window until they are
        /// built, so this is the wait before it appears.
        /// `IAI_GPU_BACKEND=dx12 cargo test --lib probe_pipeline_build_time -- --ignored --nocapture`
        /// `IAI_PROBE_WITHOUT_DEVELOP=1` times the compositor a DX12 / FXC card
        /// gets; add `WGPU_DEBUG=1` for FXC's optimiser off, as the app sets it.
        #[test]
        #[ignore]
        fn probe_pipeline_build_time() {
            let Some((device, _queue)) = crate::vector::renderer::headless_device() else {
                eprintln!("no adapter");
                return;
            };
            let info = device.adapter_info();
            let max = device.limits().max_texture_dimension_2d;
            let without_develop = std::env::var_os("IAI_PROBE_WITHOUT_DEVELOP").is_some();
            let began = std::time::Instant::now();
            let compositor = if without_develop {
                crate::compositor::CompositorState::without_develop(&device, 1, 1, max)
            } else {
                crate::compositor::CompositorState::new(&device, 1, 1, max)
            };
            let compositor_took = began.elapsed();
            let began = std::time::Instant::now();
            let _detail = crate::detail_gpu::DetailGpuRuntime::new(&device);
            eprintln!(
                "{:?} on {}: compositor{} {:.1} s, develop detail {:.1} s (atlas as {})",
                info.backend,
                info.name,
                if without_develop {
                    " without Develop"
                } else {
                    ""
                },
                compositor_took.as_secs_f32(),
                began.elapsed().as_secs_f32(),
                if compositor.tile_atlas.stores_bytes {
                    "bytes"
                } else {
                    "sRGB"
                }
            );
        }

        /// Opt-in: IAI_SHADER_DUMP is a folder; the compositor's fragment shaders
        /// are written there as HLSL, to run `fxc.exe /T ps_5_1 /E fs_main` on.
        #[test]
        #[ignore]
        fn probe_dump_compositor_hlsl() {
            let Ok(dir) = std::env::var("IAI_SHADER_DUMP") else {
                return;
            };
            let dir = std::path::Path::new(&dir);
            let fragment = naga::ShaderStage::Fragment;
            for (file, src) in [
                ("compositor.hlsl", crate::compositor::COMPOSITOR_SHADER),
                ("adjustment.hlsl", crate::compositor::ADJUSTMENT_SHADER),
            ] {
                std::fs::write(dir.join(file), hlsl_of(src, fragment, "fs_main")).unwrap();
            }
        }

        #[test]
        fn the_opengl_check_refuses_one_texture_read_through_two_samplers() {
            let shader = "
                @group(0) @binding(0) var t: texture_2d<f32>;
                @group(0) @binding(1) var a: sampler;
                @group(0) @binding(2) var b: sampler;
                @vertex fn vs_main() -> @builtin(position) vec4<f32> { return vec4<f32>(0.0); }
                @fragment fn fs_main() -> @location(0) vec4<f32> {
                    let uv = vec2<f32>(0.5);
                    return textureSampleLevel(t, a, uv, 0.0) + textureSampleLevel(t, b, uv, 0.0);
                }";
            let refused = translate_to_glsl(shader).expect_err("GLSL has no such thing");
            assert!(refused.contains("multiple samplers"), "{refused}");
        }

        #[test]
        fn opengl_takes_the_vector_and_develop_shaders() {
            assert_opengl_takes("vector", crate::vector::VECTOR_SHADER);
            assert_opengl_takes("vector composite", crate::vector::VECTOR_COMPOSITE_SHADER);
            assert_opengl_takes("develop detail", include_str!("detail.wgsl"));
            assert_opengl_takes("guided filter", include_str!("guided.wgsl"));
        }
    }
}
