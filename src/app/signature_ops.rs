//! Layer ▸ Insert Signature: the picture of the signature is asked for
//! first, then `layer.place_signature` lifts its ink off the paper and puts
//! it on the page (see `iai_imaging::signature`).

use std::path::PathBuf;

use iai_engine::CommandCall;
use serde_json::json;

use super::state::App;

/// The picture kinds a signature is photographed or scanned as.
const PICTURES: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "jpe", "bmp", "tif", "tiff", "webp",
];

impl App {
    /// `layer.place_signature` asked for without a picture (the menu): the
    /// picture is chosen first. True when the call is dealt with.
    pub(in crate::app) fn signature_step(&mut self, call: &CommandCall) -> bool {
        if call.command != "layer.place_signature" || call.args.get("path").is_some() {
            return false;
        }
        if self.jobs.pending_file_dialog.is_some() {
            return true;
        }
        let Some(window) = self.win.window.as_ref() else {
            return true;
        };
        let parent = crate::file_io::dialog_parent(window);
        let job = crate::app::jobs::Job::spawn("file-dialog", move || {
            crate::file_io::dialog_pick_one(
                parent,
                "Chọn ảnh chữ ký",
                "Ảnh chữ ký (PNG, JPG…)",
                PICTURES,
            )
            .map(crate::file_io::FileDialogResult::PickedSignature)
        });
        self.jobs.pending_file_dialog = Some(job);
        true
    }

    /// The picture of a signature chosen: placed on the page shown.
    pub(in crate::app) fn place_signature(&mut self, path: PathBuf) {
        let call = CommandCall::new(
            "layer.place_signature",
            json!({"path": path.display().to_string()}),
        );
        if self.execute_command(call).is_ok() {
            self.shell.status_msg =
                "Đã chèn chữ ký thành layer riêng — kéo để đặt chỗ, Edit ▸ Free Transform để co giãn".to_string();
        }
    }
}
