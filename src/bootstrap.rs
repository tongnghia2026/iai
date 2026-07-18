// Process bootstrap — build the global thread pool, create the event loop, run the app.
//
// Split out of `main.rs` so the binary stays a thin shell and this sequence is
// reachable (and testable) from the library.

use crate::app::App;
use winit::event_loop::EventLoop;

/// Anything that can stop IAI before or during the event loop.
#[derive(Debug)]
pub enum BootstrapError {
    EventLoop(winit::error::EventLoopError),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // User-facing strings stay Vietnamese to match the rest of the UI.
            BootstrapError::EventLoop(e) => write!(f, "Lỗi vòng lặp sự kiện: {e}"),
        }
    }
}

impl std::error::Error for BootstrapError {}

/// Size the global rayon pool from the machine tier. Must run BEFORE any
/// parallel work: weak machines keep cores free for the UI thread so a bake
/// never freezes the window. An already-built pool error is harmless.
fn init_thread_pool() {
    let hw = crate::core::hw::get();
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(crate::core::hw::rayon_threads())
        .build_global();
    eprintln!(
        "iai: {:?} tier — {} cores, {:.1} GiB RAM, {} rayon threads",
        hw.tier,
        hw.logical_cores,
        hw.total_ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
        crate::core::hw::rayon_threads(),
    );
}

/// Run IAI to completion. Blocks until the event loop exits.
pub fn run() -> Result<(), BootstrapError> {
    init_thread_pool();

    let event_loop = EventLoop::new().map_err(BootstrapError::EventLoop)?;
    let mut app = App::new();
    app.start_background_init();
    event_loop
        .run_app(&mut app)
        .map_err(BootstrapError::EventLoop)
}
