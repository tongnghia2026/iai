//! Shared ONNX Runtime execution-provider setup.
//!
//! Builds a session that runs on the GPU through DirectML when a real GPU
//! adapter is present, and transparently falls back to CPU when DirectML is
//! unavailable or the adapter cannot compile the graph. Select Subject and
//! Smart Fill share this; `retouch.rs` keeps its own equivalent builder because
//! it tracks the chosen provider per model slot.

use std::path::Path;

type OrtSession = ort::session::Session;

/// Whether AI inference should try the GPU. Only true when wgpu selected a real
/// GPU adapter (a software rasteriser is excluded, since DirectML would only add
/// overhead there). Detected once by `GpuState` and cached, so this is cheap and
/// safe to call from a worker thread.
pub fn prefer_gpu() -> bool {
    crate::core::hw::ai_gpu_candidate()
}

/// Build an ONNX session for `path`. When `prefer_gpu` is true and DirectML can
/// be registered on this adapter the session runs on the GPU; otherwise it is a
/// plain CPU session. Returns the session and whether the GPU path was taken so
/// callers can report it. GPU failures fall back to CPU rather than erroring, so
/// inference still works on machines without a usable DirectML device.
pub fn build_session(path: &Path, prefer_gpu: bool) -> Result<(OrtSession, bool), String> {
    let cpu_session = || -> Result<OrtSession, String> {
        OrtSession::builder()
            .map_err(|e| format!("ORT CPU builder: {e}"))?
            .commit_from_file(path)
            .map_err(|e| format!("ORT load model CPU: {e}"))
    };

    if !prefer_gpu {
        return Ok((cpu_session()?, false));
    }

    // DirectML requires sequential graph execution and no memory pattern. If
    // registration or graph compilation is unsupported on this adapter, rebuild
    // a clean CPU session instead.
    let gpu_attempt = (|| -> Result<OrtSession, String> {
        let provider = ort::ep::DirectML::default()
            .with_performance_preference(ort::ep::directml::PerformancePreference::HighPerformance)
            .with_device_filter(ort::ep::directml::DeviceFilter::Gpu)
            .build()
            .error_on_failure();
        OrtSession::builder()
            .map_err(|e| format!("ORT DirectML builder: {e}"))?
            .with_parallel_execution(false)
            .map_err(|e| format!("DirectML sequential mode: {e}"))?
            .with_memory_pattern(false)
            .map_err(|e| format!("DirectML memory pattern: {e}"))?
            .with_execution_providers([provider])
            .map_err(|e| format!("register DirectML: {e}"))?
            .commit_from_file(path)
            .map_err(|e| format!("load model DirectML: {e}"))
    })();

    match gpu_attempt {
        Ok(session) => Ok((session, true)),
        Err(_) => Ok((cpu_session()?, false)),
    }
}
