//! Thin process entry point. All application code lives in the `iai` library.

fn main() {
    // The same exe doubles as its own watcher process (see `iai::diag`).
    if iai::diag::run_watcher_if_requested() {
        return;
    }
    iai::crash::install_panic_handler();
    iai::diag::start();

    if let Err(e) = iai::bootstrap::run() {
        iai::crash::report_fatal(&e.to_string());
        std::process::exit(1);
    }
    iai::diag::finish();
}
