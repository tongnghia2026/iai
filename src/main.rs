//! Thin process entry point. All application code lives in the `iai` library.

fn main() {
    // The same exe doubles as its own watcher process (see `iai::diag`).
    if iai::diag::run_watcher_if_requested() {
        return;
    }
    if iai::update::print_build_info_if_requested() {
        return;
    }
    // `iai.exe mcp` / `iai.exe run`: for AI agents and scripts, no window.
    // The gateway is the Pro pack's; the free build says so and ends.
    #[cfg(feature = "pro")]
    let gateway = iai::agent::run_if_asked();
    #[cfg(not(feature = "pro"))]
    let gateway = iai::no_gateway::refuse_if_asked();
    if let Some(code) = gateway {
        std::process::exit(code);
    }
    iai::update::finish_previous();
    iai::crash::install_panic_handler();
    let build = iai::update::this_build();
    iai::diag::start(&format!("{} ({})", build.commit, build.day()));

    if let Err(e) = iai::bootstrap::run() {
        iai::crash::report_fatal(&e.to_string());
        std::process::exit(1);
    }
    // A build downloaded while the app ran takes this exe's place now.
    let updated = iai::update::apply_on_exit();
    iai::diag::finish();
    if let Some(exe) = updated {
        iai::update::relaunch(&exe);
    }
    // A print worker left inside the driver (a printer that never answered)
    // would have that driver unloaded under it by an ordinary exit.
    if iai::core::print_gdi::workers_in_driver() > 0 {
        iai::crash::end_process_now();
    }
}
