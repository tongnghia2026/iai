//! Thin process entry point. All application code lives in the `iai` library.

fn main() {
    iai::crash::install_panic_handler();

    if let Err(e) = iai::bootstrap::run() {
        iai::crash::report_fatal(&e.to_string());
        std::process::exit(1);
    }
}
