//! App-side mail-merge orchestration: picking the data file, holding the
//! pre-export session for the dialog, and running the batch PDF export off the
//! UI thread. The pure merge logic lives in [`crate::core::mail_merge`]; this
//! file only wires it to the native dialogs, the job queue and the flowing-text
//! document model.

use crate::app::state::App;
use crate::core::mail_merge::{self, MergeTable};
use crate::core::text_document::TextDocument;
use crate::file_io;
use std::path::PathBuf;
use std::sync::Arc;

/// The parsed data source plus the analysis shown in the mail-merge dialog,
/// kept between "data file chosen" and "export started".
pub struct MailMergeSession {
    pub template: Arc<TextDocument>,
    pub table: MergeTable,
    /// Display name of the chosen data file.
    pub data_file: String,
    /// Template fields that have a matching column in the data.
    pub matched: Vec<String>,
    /// Template fields with no matching column (typos / missing data).
    pub missing: Vec<String>,
}

/// Streamed progress from the batch-export worker.
pub enum MailMergeProgress {
    Step { done: usize, total: usize },
    Done(Result<String, String>),
    Cancelled,
}

impl App {
    /// Toolbar "Trộn thư": open the data-file picker for the active flowing-text
    /// document. Refuses if the template has no `{{field}}` placeholders or a
    /// merge/export is already running.
    pub fn start_mail_merge(&mut self) {
        if self.jobs.pending_mail_merge_data.is_some()
            || self.jobs.pending_mail_merge_export.is_some()
        {
            self.shell.status_msg = "Đang xử lý trộn thư khác".to_string();
            return;
        }
        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        let Some(flow) = doc.flow_text.as_ref() else {
            self.shell.status_msg = "Trộn thư chỉ dùng cho tài liệu văn bản".to_string();
            return;
        };
        let template = flow.document_arc();
        if mail_merge::find_fields(template.as_ref()).is_empty() {
            self.shell.status_msg =
                "Mẫu chưa có trường trộn nào. Thêm chỗ giữ chỗ dạng {{Tên trường}} rồi thử lại."
                    .to_string();
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new()
                .add_filter(
                    "Bảng dữ liệu",
                    &["xlsx", "xls", "xlsm", "ods", "csv", "tsv"],
                )
                .add_filter("Excel", &["xlsx", "xls", "xlsm", "ods"])
                .add_filter("CSV", &["csv", "tsv", "txt"])
                .set_title("Chọn nguồn dữ liệu trộn thư");
            if let Some(parent) = &parent {
                dialog = dialog.set_parent(parent);
            }
            let result = dialog
                .pick_file()
                .map(|path| mail_merge::read_data_file(&path).map(|table| (path, table)));
            let _ = tx.send(result);
        });
        self.jobs.pending_mail_merge_data = Some(rx);
        self.shell.status_msg = "Đang mở nguồn dữ liệu…".to_string();
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Drain the data-file picker worker; on success open the mail-merge dialog.
    pub(crate) fn poll_mail_merge_data(&mut self) {
        let result = match self.jobs.pending_mail_merge_data.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        match result {
            Ok(payload) => {
                self.jobs.pending_mail_merge_data = None;
                match payload {
                    None => self.shell.status_msg = "Đã hủy chọn dữ liệu".to_string(),
                    Some(Err(error)) => self.shell.status_msg = error,
                    Some(Ok((path, table))) => self.open_mail_merge_dialog(path, table),
                }
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.jobs.pending_mail_merge_data = None;
                self.shell.status_msg = "Tác vụ đọc dữ liệu dừng ngoài dự kiến".to_string();
            }
        }
    }

    /// Build the session and seed a sensible default filename pattern (the first
    /// data column, which is usually a name).
    fn open_mail_merge_dialog(&mut self, path: PathBuf, table: MergeTable) {
        let idx = self.docs.active_doc_idx;
        let Some(doc) = self.docs.documents.get(idx) else {
            return;
        };
        let Some(flow) = doc.flow_text.as_ref() else {
            self.shell.status_msg = "Tài liệu hiện tại không phải văn bản".to_string();
            return;
        };
        let template = flow.document_arc();
        if table.is_empty() {
            self.shell.status_msg = "Nguồn dữ liệu không có dòng nào để trộn".to_string();
            return;
        }
        let analysis = mail_merge::analyze(template.as_ref(), &table);
        let data_file = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        if self.shell.ui.mail_merge_pattern.trim().is_empty() {
            if let Some(first) = table.headers.first() {
                self.shell.ui.mail_merge_pattern = format!("{{{{{first}}}}}");
            }
        }
        let rows = table.row_count();
        self.shell.ui.mail_merge = Some(MailMergeSession {
            template,
            table,
            data_file,
            matched: analysis.matched,
            missing: analysis.missing,
        });
        self.shell.status_msg = format!("Đã đọc {rows} dòng dữ liệu trộn thư");
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Dialog "Xuất hàng loạt": pick an output folder and export one PDF per
    /// data row on a worker thread. Consumes the session.
    pub fn run_mail_merge(&mut self) {
        let Some(session) = self.shell.ui.mail_merge.take() else {
            return;
        };
        if self.jobs.pending_mail_merge_export.is_some() {
            self.shell.ui.mail_merge = Some(session);
            return;
        }
        let template = session.template;
        let table = session.table;
        let pattern = self.shell.ui.mail_merge_pattern.clone();
        let total = table.row_count();
        let parent = self
            .win
            .window
            .as_ref()
            .and_then(|w| file_io::dialog_parent(w));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Chọn thư mục lưu các hợp đồng");
            if let Some(parent) = &parent {
                dialog = dialog.set_parent(parent);
            }
            let Some(folder) = dialog.pick_folder() else {
                let _ = tx.send(MailMergeProgress::Cancelled);
                return;
            };
            let mut font_system = cosmic_text::FontSystem::new();
            let mut used = std::collections::HashSet::new();
            let mut errors: Vec<String> = Vec::new();
            let mut written = 0usize;
            for i in 0..total {
                let Some(row) = table.row(i) else { continue };
                let merged = mail_merge::merge_document(template.as_ref(), &row);
                let mut stem = mail_merge::expand_filename(&pattern, &row);
                if stem.is_empty() {
                    stem = format!("hop-dong-{:03}", i + 1);
                }
                let stem = mail_merge::unique_stem(&stem, &mut used);
                let path = folder.join(format!("{stem}.pdf"));
                let layout = crate::core::text_layout::DocumentLayout::build(
                    &merged,
                    96.0,
                    &mut font_system,
                );
                match layout.write_text_pdf(&mut font_system, &path) {
                    Ok(()) => written += 1,
                    Err(e) => errors.push(format!("Dòng {}: {e}", i + 1)),
                }
                let _ = tx.send(MailMergeProgress::Step { done: i + 1, total });
            }
            let folder_name = folder.to_string_lossy();
            let summary = if errors.is_empty() {
                Ok(format!(
                    "Đã xuất {written}/{total} hợp đồng vào {folder_name}"
                ))
            } else {
                Ok(format!(
                    "Đã xuất {written}/{total} hợp đồng ({} lỗi) vào {folder_name}",
                    errors.len()
                ))
            };
            let _ = tx.send(MailMergeProgress::Done(summary));
        });
        self.jobs.pending_mail_merge_export = Some(rx);
        self.shell.status_msg = format!("Đang trộn thư: 0/{total}…");
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Close the dialog without exporting.
    pub fn cancel_mail_merge(&mut self) {
        self.shell.ui.mail_merge = None;
    }

    /// Drain the batch-export worker: update the progress line, then the final
    /// summary.
    pub(crate) fn poll_mail_merge_export(&mut self) {
        loop {
            let result = match self.jobs.pending_mail_merge_export.as_ref() {
                Some(rx) => rx.try_recv(),
                None => return,
            };
            match result {
                Ok(MailMergeProgress::Step { done, total }) => {
                    self.shell.status_msg = format!("Đang trộn thư: {done}/{total}…");
                }
                Ok(MailMergeProgress::Cancelled) => {
                    self.jobs.pending_mail_merge_export = None;
                    self.shell.status_msg = "Đã hủy trộn thư".to_string();
                }
                Ok(MailMergeProgress::Done(outcome)) => {
                    self.jobs.pending_mail_merge_export = None;
                    self.shell.status_msg = match outcome {
                        Ok(message) => message,
                        Err(error) => error,
                    };
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    if let Some(window) = &self.win.window {
                        window.request_redraw();
                    }
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.jobs.pending_mail_merge_export = None;
                    self.shell.status_msg = "Tác vụ trộn thư dừng ngoài dự kiến".to_string();
                    return;
                }
            }
        }
    }
}
