//! Windows-only Canvas Editor host used by the Phase 1 WebView2 spike.
//!
//! The Cargo feature is deliberately off by default. The legacy cosmic-text
//! editor keeps rendering underneath this child HWND, so a missing WebView2
//! runtime fails back to the existing editor without touching document state.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{mpsc, Arc};

use serde::Deserialize;
use winit::window::Window;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use wry::http::{Request, Response, StatusCode};
use wry::{NewWindowResponse, Rect, WebView, WebViewBuilder};

use super::state::App;
use crate::core::document::{CanvasEditorDocument, DocumentId};

const IPC_PROTOCOL_VERSION: u32 = 1;
const CANVAS_EDITOR_VERSION: &str = "1.0.2";
const MAX_CONTROL_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_SNAPSHOT_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const INDEX_HTML: &[u8] = include_bytes!("../../web/document-editor/dist/index.html");
const EDITOR_JS: &[u8] = include_bytes!("../../web/document-editor/dist/editor.js");
const EDITOR_CSS: &[u8] = include_bytes!("../../web/document-editor/dist/editor.css");
const NOTO_SANS_WOFF2: &[u8] =
    include_bytes!("../../web/document-editor/dist/assets/noto-sans-vietnamese-400-normal.woff2");
const NOTO_SANS_WOFF: &[u8] =
    include_bytes!("../../web/document-editor/dist/assets/noto-sans-vietnamese-400-normal.woff");

fn should_focus_parent_after_releasing_webview(
    had_webview: bool,
    window_occluded: bool,
    window_minimized: bool,
) -> bool {
    had_webview && !window_occluded && !window_minimized
}

fn focus_parent_window(parent: &Window) {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    parent.focus_window();
    if let Ok(handle) = parent.window_handle() {
        if let RawWindowHandle::Win32(handle) = handle.as_raw() {
            // Hiding a focused child HWND does not reliably move keyboard focus
            // back to its parent. Explicit SetFocus keeps egui TextEdit/DragValue
            // fields and application shortcuts alive after leaving a text tab.
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(handle.hwnd.get() as _);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct IpcEnvelope {
    protocol_version: u32,
    request_id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    editor_version: Option<String>,
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    document: Option<serde_json::Value>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum BridgeMessage {
    Ready {
        request_id: String,
        editor_version: String,
    },
    Pong {
        request_id: String,
    },
    DocumentChanged {
        revision: u64,
    },
    Snapshot {
        request_id: String,
        revision: u64,
        document: serde_json::Value,
    },
    SaveRequested {
        revision: u64,
    },
    CloseRequested {
        revision: u64,
    },
    Error {
        request_id: String,
        code: String,
        message: String,
    },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotPurpose {
    Probe,
    Autosave {
        document_id: DocumentId,
    },
    Save {
        document_id: DocumentId,
        force_save_as: bool,
    },
    Switch {
        document_id: DocumentId,
        target_id: DocumentId,
    },
    Close {
        document_id: DocumentId,
    },
    Exit {
        document_id: DocumentId,
    },
}

#[derive(Debug)]
struct PendingSnapshot {
    request_id: String,
    purpose: SnapshotPurpose,
    deadline: std::time::Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeferredSwitch {
    document_id: DocumentId,
    target_id: DocumentId,
}

fn canvas_editor_theme_name(mode: crate::ui::theme::ThemeMode) -> &'static str {
    match mode {
        crate::ui::theme::ThemeMode::Dark => "dark",
    }
}

fn defer_switch_intent(
    pending_snapshot: &mut Option<PendingSnapshot>,
    deferred_switch: &mut Option<DeferredSwitch>,
    document_id: DocumentId,
    target_id: DocumentId,
) {
    if let Some(pending) = pending_snapshot {
        if let SnapshotPurpose::Switch {
            document_id: pending_document_id,
            target_id: pending_target_id,
        } = &mut pending.purpose
        {
            if *pending_document_id == document_id {
                *pending_target_id = target_id;
                return;
            }
        }
    }
    *deferred_switch = Some(DeferredSwitch {
        document_id,
        target_id,
    });
}

fn deferred_exit_document_id(
    deferred_exit: bool,
    pending_snapshot: bool,
    bridge_verified: bool,
    webview_active_document_id: Option<DocumentId>,
    host_active_document_id: Option<DocumentId>,
) -> Option<DocumentId> {
    (deferred_exit
        && !pending_snapshot
        && bridge_verified
        && webview_active_document_id == host_active_document_id)
        .then_some(host_active_document_id)
        .flatten()
}

fn promote_pending_snapshot_to_exit(
    pending_snapshot: &mut Option<PendingSnapshot>,
    document_id: DocumentId,
) -> bool {
    let Some(pending) = pending_snapshot else {
        return false;
    };
    let pending_document_id = match pending.purpose {
        SnapshotPurpose::Autosave { document_id }
        | SnapshotPurpose::Save { document_id, .. }
        | SnapshotPurpose::Switch { document_id, .. }
        | SnapshotPurpose::Close { document_id }
        | SnapshotPurpose::Exit { document_id } => document_id,
        SnapshotPurpose::Probe => return false,
    };
    if pending_document_id != document_id {
        return false;
    }
    pending.purpose = SnapshotPurpose::Exit { document_id };
    true
}

fn promote_pending_snapshot_to_close(
    pending_snapshot: &mut Option<PendingSnapshot>,
    document_id: DocumentId,
) -> bool {
    let Some(pending) = pending_snapshot else {
        return false;
    };
    let pending_document_id = match pending.purpose {
        SnapshotPurpose::Autosave { document_id }
        | SnapshotPurpose::Save { document_id, .. }
        | SnapshotPurpose::Switch { document_id, .. }
        | SnapshotPurpose::Close { document_id }
        | SnapshotPurpose::Exit { document_id } => document_id,
        SnapshotPurpose::Probe => return false,
    };
    if pending_document_id != document_id {
        return false;
    }
    // Never downgrade an application exit to a tab close. Every other
    // same-document snapshot contains the exact payload Close needs.
    if !matches!(pending.purpose, SnapshotPurpose::Exit { .. }) {
        pending.purpose = SnapshotPurpose::Close { document_id };
    }
    true
}

fn promote_pending_autosave_to_save(
    pending_snapshot: &mut Option<PendingSnapshot>,
    document_id: DocumentId,
    force_save_as: bool,
) -> bool {
    let Some(pending) = pending_snapshot else {
        return false;
    };
    if pending.purpose != (SnapshotPurpose::Autosave { document_id }) {
        return false;
    }
    pending.purpose = SnapshotPurpose::Save {
        document_id,
        force_save_as,
    };
    true
}

fn defer_close_intent(
    pending_snapshot: &mut Option<PendingSnapshot>,
    deferred_switch: &mut Option<DeferredSwitch>,
    deferred_close: &mut Option<DocumentId>,
    document_id: DocumentId,
) {
    *deferred_switch = None;
    *deferred_close =
        (!promote_pending_snapshot_to_close(pending_snapshot, document_id)).then_some(document_id);
}

#[derive(Debug, Clone)]
struct BridgeDocumentState {
    document: serde_json::Value,
    revision: u64,
    snapshot_revision: u64,
    dirty: bool,
}

impl BridgeDocumentState {
    fn blank() -> Self {
        Self::from_document(blank_editor_document())
    }

    fn from_document(document: serde_json::Value) -> Self {
        Self {
            document,
            revision: 0,
            snapshot_revision: 0,
            dirty: false,
        }
    }

    #[cfg(test)]
    fn has_unsnapshotted_changes(&self) -> bool {
        self.revision != self.snapshot_revision
    }

    fn cache_snapshot(&mut self, revision: u64, document: serde_json::Value) -> Result<(), String> {
        if revision < self.revision {
            return Err(format!(
                "Canvas Editor returned stale snapshot revision {revision}; expected at least {} — no file was written",
                self.revision
            ));
        }
        if revision != self.snapshot_revision {
            self.dirty = true;
        }
        self.revision = revision;
        self.document = document;
        self.snapshot_revision = revision;
        Ok(())
    }

    fn mark_saved(&mut self, revision: u64) {
        if self.revision == revision && self.snapshot_revision == revision {
            self.dirty = false;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleCompletion {
    Save {
        document_id: DocumentId,
        revision: u64,
        force_save_as: bool,
    },
    Switch(DocumentId),
    Close(DocumentId),
    Exit,
}

#[derive(Debug)]
struct CoreSnapshotUpdate {
    document_id: DocumentId,
    revision: u64,
    document: CanvasEditorDocument,
}

fn validate_core_snapshot_update(
    document_id: DocumentId,
    revision: u64,
    document: serde_json::Value,
) -> Result<CoreSnapshotUpdate, String> {
    Ok(CoreSnapshotUpdate {
        document_id,
        revision,
        document: CanvasEditorDocument::try_new(document)?,
    })
}

fn snapshot_timed_out(deadline: std::time::Instant, now: std::time::Instant) -> bool {
    now >= deadline
}

fn abort_pending_bridge_work(
    pending_snapshot: &mut Option<PendingSnapshot>,
    deferred_switch: &mut Option<DeferredSwitch>,
    deferred_close: &mut Option<DocumentId>,
    deferred_exit: &mut bool,
) {
    *pending_snapshot = None;
    *deferred_switch = None;
    *deferred_close = None;
    *deferred_exit = false;
}

fn parse_bridge_message(raw: &str) -> Result<BridgeMessage, String> {
    if raw.len() > MAX_SNAPSHOT_MESSAGE_BYTES {
        return Err("IPC message exceeds 8 MiB".to_string());
    }
    let envelope: IpcEnvelope =
        serde_json::from_str(raw).map_err(|error| format!("invalid IPC JSON: {error}"))?;
    if envelope.kind != "snapshot" && raw.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err("IPC control message exceeds 16 KiB".to_string());
    }
    if envelope.protocol_version != IPC_PROTOCOL_VERSION {
        return Err(format!(
            "unsupported IPC protocol {}",
            envelope.protocol_version
        ));
    }
    if envelope.request_id.is_empty() || envelope.request_id.len() > 128 {
        return Err("invalid IPC request_id".to_string());
    }

    match envelope.kind.as_str() {
        "ready" => {
            let editor_version = envelope
                .editor_version
                .ok_or_else(|| "ready message has no editor_version".to_string())?;
            if editor_version != CANVAS_EDITOR_VERSION {
                return Err(format!(
                    "Canvas Editor version mismatch: expected {CANVAS_EDITOR_VERSION}, got {editor_version}"
                ));
            }
            Ok(BridgeMessage::Ready {
                request_id: envelope.request_id,
                editor_version,
            })
        }
        "pong" => Ok(BridgeMessage::Pong {
            request_id: envelope.request_id,
        }),
        "document_changed" => Ok(BridgeMessage::DocumentChanged {
            revision: envelope
                .revision
                .ok_or_else(|| "document_changed has no revision".to_string())?,
        }),
        "snapshot" => {
            let revision = envelope
                .revision
                .ok_or_else(|| "snapshot has no revision".to_string())?;
            let document = envelope
                .document
                .ok_or_else(|| "snapshot has no document".to_string())?;
            validate_editor_document(&document)?;
            Ok(BridgeMessage::Snapshot {
                request_id: envelope.request_id,
                revision,
                document,
            })
        }
        "save_requested" => Ok(BridgeMessage::SaveRequested {
            revision: envelope
                .revision
                .ok_or_else(|| "save_requested has no revision".to_string())?,
        }),
        "close_requested" => Ok(BridgeMessage::CloseRequested {
            revision: envelope
                .revision
                .ok_or_else(|| "close_requested has no revision".to_string())?,
        }),
        "error" => {
            let code = envelope
                .code
                .ok_or_else(|| "error message has no code".to_string())?;
            let message = envelope
                .message
                .ok_or_else(|| "error message has no description".to_string())?;
            if code.len() > 128 || message.len() > 1024 {
                return Err("error message fields exceed limits".to_string());
            }
            Ok(BridgeMessage::Error {
                request_id: envelope.request_id,
                code,
                message,
            })
        }
        _ => Ok(BridgeMessage::Other),
    }
}

fn validate_editor_document(document: &serde_json::Value) -> Result<(), String> {
    let object = document
        .as_object()
        .ok_or_else(|| "Canvas Editor document must be an object".to_string())?;
    if !object.get("main").is_some_and(serde_json::Value::is_array) {
        return Err("Canvas Editor document.main must be an array".to_string());
    }
    for optional_zone in ["header", "footer"] {
        if object
            .get(optional_zone)
            .is_some_and(|value| !value.is_array())
        {
            return Err(format!(
                "Canvas Editor document.{optional_zone} must be an array"
            ));
        }
    }
    Ok(())
}

/// Canvas Editor is allowed to normalize element metadata while loading. The
/// probe therefore compares the user-visible main-zone text instead of the
/// complete JSON object byte-for-byte.
fn editor_main_text(document: &serde_json::Value) -> Option<String> {
    let elements = document.get("main")?.as_array()?;
    let mut text = String::new();
    for element in elements {
        if let Some(value) = element.get("value").and_then(serde_json::Value::as_str) {
            text.push_str(value);
        }
    }
    Some(text)
}

fn probe_snapshot_matches(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    editor_main_text(expected) == editor_main_text(actual)
}

fn phase_two_probe_document() -> serde_json::Value {
    serde_json::json!({
        "header": [],
        "main": [{ "value": "Tiếng Việt — Nguyễn Thị Thu — Ắ Ề Ễ Ự" }],
        "footer": []
    })
}

fn blank_editor_document() -> serde_json::Value {
    serde_json::json!({
        "header": [],
        "main": [{ "value": "" }],
        "footer": []
    })
}

fn asset_response(request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let (status, mime, body) = match request.uri().path() {
        "/" | "/index.html" => (StatusCode::OK, "text/html; charset=utf-8", INDEX_HTML),
        "/editor.js" => (StatusCode::OK, "text/javascript; charset=utf-8", EDITOR_JS),
        "/editor.css" => (StatusCode::OK, "text/css; charset=utf-8", EDITOR_CSS),
        "/assets/noto-sans-vietnamese-400-normal.woff2" => {
            (StatusCode::OK, "font/woff2", NOTO_SANS_WOFF2)
        }
        "/assets/noto-sans-vietnamese-400-normal.woff" => {
            (StatusCode::OK, "font/woff", NOTO_SANS_WOFF)
        }
        _ => (
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            b"not found" as &[u8],
        ),
    };
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, mime)
        .header(CACHE_CONTROL, "no-store")
        .body(Cow::Borrowed(body))
        .expect("static asset response has valid headers")
}

fn allows_editor_navigation(url: &str) -> bool {
    url.starts_with("iai-editor://localhost/")
        || url.starts_with("http://iai-editor.localhost/")
        || url.starts_with("https://iai-editor.localhost/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PhysicalWebViewRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl PhysicalWebViewRect {
    fn from_egui(rect: egui::Rect, scale_factor: f64) -> Option<Self> {
        if !rect.is_finite() || !scale_factor.is_finite() || scale_factor <= 0.0 {
            return None;
        }
        let x = (f64::from(rect.min.x) * scale_factor).round() as i32;
        let y = (f64::from(rect.min.y) * scale_factor).round() as i32;
        let width = (f64::from(rect.width()) * scale_factor).round().max(1.0) as u32;
        let height = (f64::from(rect.height()) * scale_factor).round().max(1.0) as u32;
        Some(Self {
            x,
            y,
            width,
            height,
        })
    }

    fn into_wry(self) -> Rect {
        Rect {
            position: PhysicalPosition::new(self.x, self.y).into(),
            size: PhysicalSize::new(self.width, self.height).into(),
        }
    }
}

pub(in crate::app) struct DocumentWebView {
    webview: WebView,
    ipc_rx: mpsc::Receiver<String>,
    bounds: Option<PhysicalWebViewRect>,
    visible: bool,
    ready: bool,
    bridge_verified: bool,
    ping_request_id: Option<String>,
    pending_snapshot: Option<PendingSnapshot>,
    deferred_switch: Option<DeferredSwitch>,
    deferred_close: Option<DocumentId>,
    deferred_exit: bool,
    expected_snapshot: Option<serde_json::Value>,
    active_document_id: Option<DocumentId>,
    documents: HashMap<DocumentId, BridgeDocumentState>,
    core_snapshot_updates: Vec<CoreSnapshotUpdate>,
    lifecycle_completion: Option<LifecycleCompletion>,
    pending_error_dialog: Option<String>,
    last_theme: Option<&'static str>,
    last_read_only: Option<bool>,
    bridge_revision: u64,
    next_request_id: u64,
}

impl DocumentWebView {
    fn create(parent: &Arc<Window>) -> Result<Self, String> {
        let (ipc_tx, ipc_rx) = mpsc::channel();
        let redraw_window = Arc::downgrade(parent);
        let initial_bounds = PhysicalWebViewRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let webview = WebViewBuilder::new()
            .with_bounds(initial_bounds.into_wry())
            .with_visible(false)
            .with_clipboard(true)
            .with_hotkeys_zoom(false)
            .with_custom_protocol("iai-editor".to_string(), |_id, request| {
                asset_response(request)
            })
            .with_navigation_handler(|url| allows_editor_navigation(&url))
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_ipc_handler(move |request| {
                let _ = ipc_tx.send(request.body().clone());
                if let Some(window) = redraw_window.upgrade() {
                    window.request_redraw();
                }
            })
            .with_url("iai-editor://localhost/index.html")
            .build_as_child(parent.as_ref())
            .map_err(|error| format!("WebView2 initialization failed: {error}"))?;

        Ok(Self {
            webview,
            ipc_rx,
            bounds: None,
            visible: false,
            ready: false,
            bridge_verified: false,
            ping_request_id: None,
            pending_snapshot: None,
            deferred_switch: None,
            deferred_close: None,
            deferred_exit: false,
            expected_snapshot: None,
            active_document_id: None,
            documents: HashMap::new(),
            core_snapshot_updates: Vec::new(),
            lifecycle_completion: None,
            pending_error_dialog: None,
            last_theme: None,
            last_read_only: None,
            bridge_revision: 0,
            next_request_id: 1,
        })
    }

    fn next_request_id(&mut self) -> String {
        let request_id = format!("rust-{}", self.next_request_id);
        self.next_request_id += 1;
        request_id
    }

    fn send_host_message(&self, message: &serde_json::Value) -> Result<(), String> {
        let encoded = serde_json::to_string(message)
            .map_err(|error| format!("cannot encode host IPC message: {error}"))?;
        let script = format!("window.__iaiReceive?.({encoded});");
        self.webview
            .evaluate_script(&script)
            .map_err(|error| format!("cannot send host IPC message: {error}"))
    }

    fn load_probe_document(&mut self) -> Result<(), String> {
        let request_id = self.next_request_id();
        let document = phase_two_probe_document();
        let message = serde_json::json!({
            "protocol_version": IPC_PROTOCOL_VERSION,
            "request_id": request_id,
            "type": "load_document",
            "revision": 0,
            "document": document
        });
        self.send_host_message(&message)?;
        self.expected_snapshot = message.get("document").cloned();
        self.active_document_id = None;
        self.bridge_revision = 0;
        Ok(())
    }

    fn activate_document(&mut self, document_id: DocumentId) -> Result<(), String> {
        if !self.bridge_verified || self.active_document_id == Some(document_id) {
            return Ok(());
        }
        if self.pending_snapshot.is_some() {
            return Err("Canvas Editor cannot change tabs while a snapshot is pending".to_string());
        }
        let (revision, document) = {
            let state = self
                .documents
                .entry(document_id)
                .or_insert_with(BridgeDocumentState::blank);
            (state.revision, state.document.clone())
        };
        let request_id = self.next_request_id();
        let message = serde_json::json!({
            "protocol_version": IPC_PROTOCOL_VERSION,
            "request_id": request_id,
            "type": "load_document",
            "revision": revision,
            "document": document
        });
        self.send_host_message(&message)?;
        self.bridge_revision = revision;
        self.active_document_id = Some(document_id);
        Ok(())
    }

    fn ensure_document(
        &mut self,
        document_id: DocumentId,
        initial_document: Option<&serde_json::Value>,
    ) {
        self.documents.entry(document_id).or_insert_with(|| {
            initial_document.map_or_else(BridgeDocumentState::blank, |document| {
                BridgeDocumentState::from_document(document.clone())
            })
        });
    }

    fn cache_snapshot(
        &mut self,
        document_id: DocumentId,
        revision: u64,
        document: serde_json::Value,
    ) -> Result<(), String> {
        if self.active_document_id != Some(document_id) {
            return Err("Canvas Editor snapshot belongs to an inactive document".to_string());
        }
        if revision < self.bridge_revision {
            return Err(format!(
                "Canvas Editor returned stale snapshot revision {revision}; expected at least {} — no file was written",
                self.bridge_revision
            ));
        }
        let core_update = validate_core_snapshot_update(document_id, revision, document.clone())?;
        self.documents
            .get_mut(&document_id)
            .ok_or_else(|| "Canvas Editor document state is missing".to_string())?
            .cache_snapshot(revision, document)?;
        self.bridge_revision = revision;
        self.core_snapshot_updates.push(core_update);
        Ok(())
    }

    fn take_core_snapshot_updates(&mut self) -> Vec<CoreSnapshotUpdate> {
        std::mem::take(&mut self.core_snapshot_updates)
    }

    fn mark_document_saved(&mut self, document_id: DocumentId, revision: u64) {
        if let Some(state) = self.documents.get_mut(&document_id) {
            state.mark_saved(revision);
        }
    }

    fn document_is_dirty(&self, document_id: DocumentId) -> bool {
        self.documents
            .get(&document_id)
            .is_some_and(|state| state.dirty)
    }

    fn dirty_document_ids(&self) -> Vec<DocumentId> {
        self.documents
            .iter()
            .filter_map(|(id, state)| state.dirty.then_some(*id))
            .collect()
    }

    fn forget_document(&mut self, document_id: DocumentId) {
        self.documents.remove(&document_id);
        if self.active_document_id == Some(document_id) {
            self.active_document_id = None;
            self.bridge_revision = 0;
        }
    }

    fn send_ping(&mut self) -> Result<(), String> {
        let request_id = self.next_request_id();
        let message = serde_json::json!({
            "protocol_version": IPC_PROTOCOL_VERSION,
            "request_id": request_id,
            "type": "ping"
        });
        self.send_host_message(&message)?;
        self.ping_request_id = message["request_id"].as_str().map(str::to_owned);
        Ok(())
    }

    fn sync_host_state(&mut self, theme: &'static str, read_only: bool) -> Result<(), String> {
        if !self.ready {
            return Ok(());
        }
        if self.last_theme != Some(theme) {
            let request_id = self.next_request_id();
            self.send_host_message(&serde_json::json!({
                "protocol_version": IPC_PROTOCOL_VERSION,
                "request_id": request_id,
                "type": "set_theme",
                "theme": theme,
            }))?;
            self.last_theme = Some(theme);
        }
        if self.last_read_only != Some(read_only) {
            let request_id = self.next_request_id();
            self.send_host_message(&serde_json::json!({
                "protocol_version": IPC_PROTOCOL_VERSION,
                "request_id": request_id,
                "type": "set_read_only",
                "value": read_only,
            }))?;
            self.last_read_only = Some(read_only);
        }
        Ok(())
    }

    fn request_snapshot(&mut self, purpose: SnapshotPurpose) -> Result<(), String> {
        if self.pending_snapshot.is_some() {
            return Err("Canvas Editor snapshot request is already pending".to_string());
        }
        let request_id = self.next_request_id();
        let message = serde_json::json!({
            "protocol_version": IPC_PROTOCOL_VERSION,
            "request_id": request_id,
            "type": "request_snapshot"
        });
        self.send_host_message(&message)?;
        self.pending_snapshot = Some(PendingSnapshot {
            request_id,
            purpose,
            deadline: std::time::Instant::now() + SNAPSHOT_TIMEOUT,
        });
        Ok(())
    }

    /// Preserve the latest tab click while the bridge probe, a document load,
    /// or another snapshot is still in flight. If the in-flight request is
    /// already switching away from the same document, retarget that completion
    /// directly instead of scheduling a second snapshot.
    fn defer_switch(&mut self, document_id: DocumentId, target_id: DocumentId) {
        defer_switch_intent(
            &mut self.pending_snapshot,
            &mut self.deferred_switch,
            document_id,
            target_id,
        );
    }

    /// Start a preserved tab switch as soon as the verified bridge is bound to
    /// the same source document the user clicked away from.
    fn drive_deferred_switch(
        &mut self,
        host_active_document_id: Option<DocumentId>,
    ) -> Option<String> {
        if self.deferred_exit {
            self.deferred_switch = None;
            return None;
        }
        let deferred = self.deferred_switch?;
        if host_active_document_id != Some(deferred.document_id) {
            self.deferred_switch = None;
            return Some("Canceled an obsolete Canvas Editor tab switch".to_string());
        }
        if self.pending_snapshot.is_some()
            || !self.bridge_verified
            || self.active_document_id != Some(deferred.document_id)
        {
            return None;
        }

        self.deferred_switch = None;
        Some(
            match self.request_snapshot(SnapshotPurpose::Switch {
                document_id: deferred.document_id,
                target_id: deferred.target_id,
            }) {
                Ok(()) => {
                    "Canvas Editor ready; capturing snapshot before switching tabs...".to_string()
                }
                Err(error) => {
                    self.deferred_switch = Some(deferred);
                    error
                }
            },
        )
    }

    fn defer_close(&mut self, document_id: DocumentId) {
        // Closing the active tab supersedes navigation away from it. Reuse any
        // same-document snapshot already in flight; otherwise retain the
        // intent across bridge probe/load until a snapshot can be requested.
        defer_close_intent(
            &mut self.pending_snapshot,
            &mut self.deferred_switch,
            &mut self.deferred_close,
            document_id,
        );
    }

    fn drive_deferred_close(
        &mut self,
        host_active_document_id: Option<DocumentId>,
    ) -> Option<String> {
        if self.deferred_exit {
            self.deferred_close = None;
            return None;
        }
        let document_id = self.deferred_close?;
        if host_active_document_id != Some(document_id) {
            self.deferred_close = None;
            return Some("Canceled an obsolete Canvas Editor tab close".to_string());
        }
        if self.pending_snapshot.is_some()
            || !self.bridge_verified
            || self.active_document_id != Some(document_id)
        {
            return None;
        }

        self.deferred_close = None;
        Some(
            match self.request_snapshot(SnapshotPurpose::Close { document_id }) {
                Ok(()) => "Canvas Editor ready; capturing snapshot before closing...".to_string(),
                Err(error) => {
                    self.deferred_close = Some(document_id);
                    error
                }
            },
        )
    }

    fn defer_exit(&mut self, document_id: DocumentId) {
        // Closing the whole application supersedes a tab-navigation intent.
        self.deferred_switch = None;
        self.deferred_close = None;
        // A snapshot already in flight for the same active document contains
        // exactly the payload Exit needs. Promote its completion so a pending
        // Switch cannot move the host to another tab and strand CloseRequested.
        self.deferred_exit =
            !promote_pending_snapshot_to_exit(&mut self.pending_snapshot, document_id);
    }

    fn drive_deferred_exit(
        &mut self,
        host_active_document_id: Option<DocumentId>,
    ) -> Option<String> {
        let document_id = deferred_exit_document_id(
            self.deferred_exit,
            self.pending_snapshot.is_some(),
            self.bridge_verified,
            self.active_document_id,
            host_active_document_id,
        )?;

        self.deferred_exit = false;
        Some(
            match self.request_snapshot(SnapshotPurpose::Exit { document_id }) {
                Ok(()) => "Canvas Editor ready; capturing snapshot before app exit...".to_string(),
                Err(error) => {
                    self.deferred_exit = true;
                    error
                }
            },
        )
    }

    fn abort_pending_work(&mut self) {
        abort_pending_bridge_work(
            &mut self.pending_snapshot,
            &mut self.deferred_switch,
            &mut self.deferred_close,
            &mut self.deferred_exit,
        );
    }

    fn report_error(&mut self, message: String) -> String {
        // Never let a success queued earlier in the same IPC drain close or
        // switch a document after a later bridge failure is observed.
        self.lifecycle_completion = None;
        self.pending_error_dialog = Some(message.clone());
        message
    }

    fn fail_pending_work(&mut self, message: String) -> String {
        self.abort_pending_work();
        self.report_error(message)
    }

    fn poll_bridge(&mut self) -> Option<String> {
        let timed_out = self
            .pending_snapshot
            .as_ref()
            .is_some_and(|pending| snapshot_timed_out(pending.deadline, std::time::Instant::now()));
        let mut status = if timed_out {
            Some(self.fail_pending_work(
                "Canvas Editor snapshot timed out after 2 seconds; the pending action was canceled and no file was written"
                    .to_string(),
            ))
        } else {
            None
        };
        while let Ok(raw) = self.ipc_rx.try_recv() {
            match parse_bridge_message(&raw) {
                Ok(BridgeMessage::Ready { editor_version, .. }) if !self.ready => {
                    self.ready = true;
                    status = Some(format!(
                        "Canvas Editor {editor_version} ready; checking IPC and document round-trip..."
                    ));
                    if let Err(error) = self.load_probe_document().and_then(|()| self.send_ping()) {
                        status = Some(self.fail_pending_work(error));
                    }
                }
                Ok(BridgeMessage::Pong { request_id })
                    if self.ping_request_id.as_deref() == Some(request_id.as_str()) =>
                {
                    self.ping_request_id = None;
                    status = Some(
                        match self.request_snapshot(SnapshotPurpose::Probe) {
                            Ok(()) => format!(
                            "Canvas Editor {CANVAS_EDITOR_VERSION} ready — ping/pong OK; checking snapshot..."
                        ),
                            Err(error) => self.fail_pending_work(error),
                        },
                    );
                }
                Ok(BridgeMessage::DocumentChanged { revision }) => {
                    if revision > self.bridge_revision {
                        self.bridge_revision = revision;
                        if let Some(document_id) = self.active_document_id {
                            if let Some(state) = self.documents.get_mut(&document_id) {
                                state.revision = revision;
                                state.dirty = true;
                            } else {
                                status = Some(self.report_error(
                                    "Canvas Editor changed an untracked document".to_string(),
                                ));
                            }
                        }
                    }
                }
                Ok(BridgeMessage::Snapshot {
                    request_id,
                    revision,
                    document,
                }) if self
                    .pending_snapshot
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request_id) =>
                {
                    let pending = self
                        .pending_snapshot
                        .take()
                        .expect("matching snapshot has pending request");
                    match pending.purpose {
                        SnapshotPurpose::Probe
                            if self.expected_snapshot.as_ref().is_some_and(|expected| {
                                probe_snapshot_matches(expected, &document)
                            }) && revision == self.bridge_revision =>
                        {
                            self.expected_snapshot = None;
                            self.bridge_verified = true;
                            status = Some(format!(
                                "Canvas Editor {CANVAS_EDITOR_VERSION} ready — IPC load/snapshot round-trip OK"
                            ));
                        }
                        SnapshotPurpose::Probe => {
                            status = Some(
                                self.fail_pending_work(
                                    "Canvas Editor snapshot differs from the loaded document"
                                        .to_string(),
                                ),
                            );
                        }
                        SnapshotPurpose::Autosave { document_id } => {
                            let cache_result = self.cache_snapshot(document_id, revision, document);
                            status = Some(match cache_result {
                                Ok(()) => format!(
                                    "Canvas Editor autosave snapshot cached at revision {revision}; disk recovery waits for .iai v12"
                                ),
                                Err(error) => self.fail_pending_work(error),
                            });
                        }
                        SnapshotPurpose::Save {
                            document_id,
                            force_save_as,
                        } => {
                            let cache_result = self.cache_snapshot(document_id, revision, document);
                            status = Some(match cache_result {
                                Ok(()) => {
                                    self.lifecycle_completion = Some(LifecycleCompletion::Save {
                                        document_id,
                                        revision,
                                        force_save_as,
                                    });
                                    format!(
                                        "Canvas Editor snapshot captured at revision {revision}; saving .iai v12..."
                                    )
                                }
                                Err(error) => self.fail_pending_work(error),
                            });
                        }
                        SnapshotPurpose::Switch {
                            document_id,
                            target_id,
                        } => match self.cache_snapshot(document_id, revision, document) {
                            Ok(()) => {
                                self.lifecycle_completion =
                                    Some(LifecycleCompletion::Switch(target_id));
                                status = Some(format!(
                                    "Canvas Editor cached revision {revision} before switching tabs"
                                ));
                            }
                            Err(error) => status = Some(self.fail_pending_work(error)),
                        },
                        SnapshotPurpose::Close { document_id } => {
                            match self.cache_snapshot(document_id, revision, document) {
                                Ok(()) => {
                                    self.lifecycle_completion =
                                        Some(LifecycleCompletion::Close(document_id));
                                    status = Some(format!(
                                        "Canvas Editor cached revision {revision} before closing"
                                    ));
                                }
                                Err(error) => status = Some(self.fail_pending_work(error)),
                            }
                        }
                        SnapshotPurpose::Exit { document_id } => {
                            match self.cache_snapshot(document_id, revision, document) {
                                Ok(()) => {
                                    self.lifecycle_completion = Some(LifecycleCompletion::Exit);
                                    status = Some(format!(
                                        "Canvas Editor cached revision {revision} before app exit"
                                    ));
                                }
                                Err(error) => status = Some(self.fail_pending_work(error)),
                            }
                        }
                    }
                }
                Ok(BridgeMessage::SaveRequested { revision }) => {
                    if revision != self.bridge_revision {
                        status = Some(self.report_error(format!(
                            "Canvas Editor save revision mismatch: got {revision}, expected {}",
                            self.bridge_revision
                        )));
                    } else if let Some(document_id) = self.active_document_id {
                        status = Some(
                            match self.request_snapshot(SnapshotPurpose::Save {
                                document_id,
                                force_save_as: false,
                            }) {
                                Ok(()) => format!(
                                    "Canvas Editor save requested at revision {revision}; capturing snapshot..."
                                ),
                                Err(error) => error,
                            },
                        );
                    } else {
                        status = Some(self.report_error(
                            "Canvas Editor save requested without an active document".to_string(),
                        ));
                    }
                }
                Ok(BridgeMessage::CloseRequested { revision }) => {
                    if revision != self.bridge_revision {
                        status = Some(self.report_error(format!(
                            "Canvas Editor close revision mismatch: got {revision}, expected {}",
                            self.bridge_revision
                        )));
                    } else if let Some(document_id) = self.active_document_id {
                        if self.pending_snapshot.is_some() {
                            self.defer_close(document_id);
                            status = Some(
                                "Using the in-flight Canvas Editor snapshot for Ctrl+W..."
                                    .to_string(),
                            );
                        } else {
                            status = Some(
                                match self.request_snapshot(SnapshotPurpose::Close { document_id })
                                {
                                    Ok(()) => {
                                        "Capturing Canvas Editor snapshot before Ctrl+W close..."
                                            .to_string()
                                    }
                                    Err(error) => error,
                                },
                            );
                        }
                    } else {
                        status = Some(self.report_error(
                            "Canvas Editor Ctrl+W arrived without an active document".to_string(),
                        ));
                    }
                }
                Ok(BridgeMessage::Error {
                    request_id,
                    code,
                    message,
                }) => {
                    if self
                        .pending_snapshot
                        .as_ref()
                        .is_some_and(|pending| pending.request_id == request_id)
                    {
                        self.abort_pending_work();
                    }
                    status =
                        Some(self.report_error(format!("Canvas Editor error {code}: {message}")));
                }
                Ok(_) => {}
                Err(error) => {
                    status =
                        Some(self.fail_pending_work(format!("Canvas Editor IPC error: {error}")))
                }
            }
        }
        status
    }

    fn take_error_dialog(&mut self) -> Option<String> {
        self.pending_error_dialog.take()
    }

    fn take_lifecycle_completion(&mut self) -> Option<LifecycleCompletion> {
        self.lifecycle_completion.take()
    }

    fn snapshot_deadline(&self) -> Option<std::time::Instant> {
        self.pending_snapshot
            .as_ref()
            .map(|pending| pending.deadline)
    }

    fn set_frame(
        &mut self,
        logical_bounds: Option<egui::Rect>,
        scale_factor: f64,
        show: bool,
    ) -> Result<(), String> {
        let physical_bounds =
            logical_bounds.and_then(|rect| PhysicalWebViewRect::from_egui(rect, scale_factor));
        if physical_bounds != self.bounds {
            if let Some(bounds) = physical_bounds {
                self.webview
                    .set_bounds(bounds.into_wry())
                    .map_err(|error| format!("cannot resize document WebView: {error}"))?;
            }
            self.bounds = physical_bounds;
        }
        let visible = show && physical_bounds.is_some();
        if visible != self.visible {
            self.webview
                .set_visible(visible)
                .map_err(|error| format!("cannot change document WebView visibility: {error}"))?;
            self.visible = visible;
        }
        Ok(())
    }

    fn hide(&mut self) {
        if self.visible && self.webview.set_visible(false).is_ok() {
            self.visible = false;
        }
    }
}

impl App {
    /// Capture the active Canvas Editor document before Save/Save As. Until the
    /// v12 writer lands, this deliberately consumes the save command after the
    /// snapshot and never falls through to the legacy FlowText writer.
    pub(in crate::app) fn request_document_webview_save_snapshot(
        &mut self,
        force_save_as: bool,
    ) -> bool {
        let document = &self.docs.documents[self.docs.active_doc_idx];
        if !document.is_flow_text() || self.win.document_webview_failed {
            return false;
        }
        let document_id = document.id;

        let status = match self.win.document_webview.as_mut() {
            Some(webview)
                if webview.ready
                    && webview.bridge_verified
                    && webview.active_document_id == Some(document_id) =>
            {
                if promote_pending_autosave_to_save(
                    &mut webview.pending_snapshot,
                    document_id,
                    force_save_as,
                ) {
                    "Using the in-flight Canvas Editor autosave snapshot for save...".to_string()
                } else {
                    match webview.request_snapshot(SnapshotPurpose::Save {
                        document_id,
                        force_save_as,
                    }) {
                        Ok(()) => "Capturing Canvas Editor snapshot before save...".to_string(),
                        Err(error) => error,
                    }
                }
            }
            Some(_) => "Canvas Editor is not ready; no file was written".to_string(),
            None => "Canvas Editor WebView is not available yet; no file was written".to_string(),
        };
        self.shell.status_msg = status;
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
        true
    }

    pub(in crate::app) fn document_webview_save_is_pending(&self) -> bool {
        self.win.document_webview.as_ref().is_some_and(|webview| {
            matches!(
                webview
                    .pending_snapshot
                    .as_ref()
                    .map(|pending| pending.purpose),
                Some(SnapshotPurpose::Save { .. })
            )
        })
    }

    pub(in crate::app) fn mark_document_webview_saved(
        &mut self,
        document_id: DocumentId,
        revision: u64,
    ) {
        if let Some(webview) = self.win.document_webview.as_mut() {
            webview.mark_document_saved(document_id, revision);
        }
    }

    fn complete_document_webview_save(
        &mut self,
        document_id: DocumentId,
        revision: u64,
        force_save_as: bool,
        parent: &Arc<Window>,
    ) {
        let Some(document) = self.docs.documents.get(self.docs.active_doc_idx) else {
            return;
        };
        if document.id != document_id
            || document
                .flow_text
                .as_ref()
                .is_none_or(|flow| flow.revision() != revision)
        {
            self.shell.status_msg =
                "Canvas Editor save was canceled because the active revision changed".to_string();
            self.shell.ui.document_editor_error = Some(self.shell.status_msg.clone());
            self.finish_document_webview_save_continuation();
            parent.request_redraw();
            return;
        }

        let existing_iai = (!force_save_as)
            .then(|| {
                document
                    .path
                    .clone()
                    .or_else(|| self.docs.current_file.clone())
            })
            .flatten()
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("iai"))
            });
        if let Some(path) = existing_iai {
            self.save_flow_text_doc_to(&path);
            self.finish_document_webview_save_continuation();
        } else {
            self.do_save_project_as();
        }
        parent.request_redraw();
    }

    /// Complete a Save & Close / Save & Exit request only when both the core
    /// backing and the editor revision are clean. A failed write or a newer
    /// WebView edit re-opens the confirmation instead of discarding content.
    pub(in crate::app) fn finish_document_webview_save_continuation(&mut self) {
        let persistence_clean = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|document| {
                !document.is_modified() && !self.document_webview_document_is_dirty(document.id)
            });

        if self.shell.close_requested {
            self.shell.close_requested = false;
            if persistence_clean {
                self.execute_close();
            } else {
                self.shell.ui.show_close_dialog = true;
            }
        }
        if self.shell.exit_save_pending {
            self.shell.exit_save_pending = false;
            if persistence_clean {
                self.docs.pending_exit_docs.pop_front();
                self.present_next_exit_document();
            } else {
                self.shell.ui.show_exit_dialog = true;
            }
        }
    }

    /// Start the FlowText autosave handshake without blocking the UI thread.
    /// Disk recovery is intentionally deferred until the `.iai` v12 writer is
    /// available; this stage guarantees the latest validated per-tab payload.
    pub(in crate::app) fn request_document_webview_autosave_snapshot(&mut self) -> bool {
        let Some(document) = self.docs.documents.get(self.docs.active_doc_idx) else {
            return false;
        };
        if !document.is_flow_text()
            || self.win.document_webview_failed
            || !self.document_webview_active_is_dirty()
        {
            return false;
        }
        let document_id = document.id;
        let Some(webview) = self.win.document_webview.as_mut() else {
            return false;
        };
        if !webview.ready
            || !webview.bridge_verified
            || webview.active_document_id != Some(document_id)
            || webview.pending_snapshot.is_some()
        {
            return false;
        }

        match webview.request_snapshot(SnapshotPurpose::Autosave { document_id }) {
            Ok(()) => {
                self.shell.status_msg = "Capturing Canvas Editor autosave snapshot...".to_string();
                if let Some(window) = &self.win.window {
                    window.request_redraw();
                }
                true
            }
            Err(error) => {
                self.shell.status_msg = format!("Canvas Editor autosave failed: {error}");
                false
            }
        }
    }

    /// Return true when an asynchronous snapshot owns this tab switch.
    pub(in crate::app) fn request_document_webview_switch_snapshot(
        &mut self,
        target_idx: usize,
    ) -> bool {
        let Some(current) = self.docs.documents.get(self.docs.active_doc_idx) else {
            return false;
        };
        let Some(target) = self.docs.documents.get(target_idx) else {
            return false;
        };
        if !current.is_flow_text() || self.win.document_webview_failed {
            return false;
        }
        let (document_id, target_id) = (current.id, target.id);
        let Some(webview) = self.win.document_webview.as_mut() else {
            return false;
        };
        if !webview.bridge_verified
            || webview.active_document_id != Some(document_id)
            || webview.pending_snapshot.is_some()
        {
            webview.defer_switch(document_id, target_id);
            self.shell.status_msg =
                "Canvas Editor tab is not ready yet; tab switch was queued".to_string();
            return true;
        }

        self.shell.status_msg = match webview.request_snapshot(SnapshotPurpose::Switch {
            document_id,
            target_id,
        }) {
            Ok(()) => "Capturing Canvas Editor snapshot before switching tabs...".to_string(),
            Err(error) => error,
        };
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
        true
    }

    /// Return true when Canvas Editor dirty state owns this close request.
    pub(in crate::app) fn request_document_webview_close_snapshot(&mut self, idx: usize) -> bool {
        let Some(document) = self.docs.documents.get(idx) else {
            return false;
        };
        if !document.is_flow_text() || self.win.document_webview_failed {
            return false;
        }
        let document_id = document.id;
        let Some(webview) = self.win.document_webview.as_mut() else {
            return false;
        };

        if webview.active_document_id == Some(document_id) {
            if webview.pending_snapshot.is_some() || !webview.bridge_verified {
                webview.defer_close(document_id);
                self.shell.status_msg =
                    "Canvas Editor tab close was queued until its snapshot is ready".to_string();
            } else {
                self.shell.status_msg =
                    match webview.request_snapshot(SnapshotPurpose::Close { document_id }) {
                        Ok(()) => "Capturing Canvas Editor snapshot before closing...".to_string(),
                        Err(error) => error,
                    };
            }
        } else if webview.document_is_dirty(document_id) {
            self.docs.pending_close_doc_idx = Some(idx);
            self.shell.ui.show_close_dialog = true;
        } else if idx != self.docs.active_doc_idx {
            return false;
        } else {
            self.shell.status_msg =
                "Canvas Editor tab is not ready; close was canceled".to_string();
        }
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
        true
    }

    /// Return true while app exit waits for the active editor's latest payload.
    pub(in crate::app) fn request_document_webview_exit_snapshot(&mut self) -> bool {
        let Some(document) = self.docs.documents.get(self.docs.active_doc_idx) else {
            return false;
        };
        if !document.is_flow_text() || self.win.document_webview_failed {
            return false;
        }
        let document_id = document.id;
        let Some(webview) = self.win.document_webview.as_mut() else {
            return false;
        };
        if !webview.bridge_verified
            || webview.active_document_id != Some(document_id)
            || webview.pending_snapshot.is_some()
        {
            webview.defer_exit(document_id);
            self.shell.status_msg =
                "Canvas Editor app exit was queued until the active snapshot is ready".to_string();
            if let Some(window) = &self.win.window {
                window.request_redraw();
            }
            return true;
        }

        self.shell.status_msg =
            match webview.request_snapshot(SnapshotPurpose::Exit { document_id }) {
                Ok(()) => "Capturing Canvas Editor snapshot before app exit...".to_string(),
                Err(error) => error,
            };
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
        true
    }

    pub(in crate::app) fn document_webview_dirty_document_ids(&self) -> Vec<DocumentId> {
        self.win
            .document_webview
            .as_ref()
            .map_or_else(Vec::new, DocumentWebView::dirty_document_ids)
    }

    pub(in crate::app) fn document_webview_document_is_dirty(
        &self,
        document_id: DocumentId,
    ) -> bool {
        self.win
            .document_webview
            .as_ref()
            .is_some_and(|webview| webview.document_is_dirty(document_id))
    }

    pub(in crate::app) fn document_webview_active_is_dirty(&self) -> bool {
        self.docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|document| {
                document.is_flow_text() && self.document_webview_document_is_dirty(document.id)
            })
    }

    pub(in crate::app) fn forget_document_webview_state(&mut self, document_id: DocumentId) {
        if let Some(webview) = self.win.document_webview.as_mut() {
            webview.forget_document(document_id);
        }
    }

    fn prepare_active_legacy_conversion(
        &self,
    ) -> Result<
        Option<(
            DocumentId,
            u64,
            crate::core::canvas_editor_conversion::LegacyConversion,
        )>,
        String,
    > {
        let Some(flow_text) = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .and_then(|document| document.flow_text.as_ref())
        else {
            return Ok(None);
        };
        let crate::core::document::FlowTextBacking::Legacy(legacy) = flow_text.backing() else {
            return Ok(None);
        };
        let conversion = crate::core::canvas_editor_conversion::convert_legacy_document(legacy)?;
        Ok(Some((
            self.docs.documents[self.docs.active_doc_idx].id,
            flow_text.revision(),
            conversion,
        )))
    }

    pub(in crate::app) fn sync_document_webview(
        &mut self,
        parent: &Arc<Window>,
        bounds: Option<egui::Rect>,
        popup_open: bool,
    ) {
        let active_is_flow_text = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .is_some_and(|document| document.is_flow_text());
        if !active_is_flow_text {
            // Do not keep a hidden child HWND alive over image/vector tabs. A
            // hidden WebView2 can retain keyboard ownership even after SetFocus,
            // especially when a popup hid it before the asynchronous tab switch
            // completed. The outgoing FlowText snapshot was committed to core by
            // the switch gate, so recreating the editor later is lossless.
            let had_webview = self.win.document_webview.take().is_some();
            self.win.document_webview_failed = false;
            if should_focus_parent_after_releasing_webview(
                had_webview,
                self.win.window_occluded,
                super::input::window_is_minimized(parent),
            ) {
                focus_parent_window(parent);
            }
            return;
        }

        let show = bounds.is_some()
            && !popup_open
            && !self.is_modal_open()
            && !self.win.window_occluded
            && !super::input::window_is_minimized(parent);

        if show && self.win.document_webview.is_none() && !self.win.document_webview_failed {
            match DocumentWebView::create(parent) {
                Ok(webview) => self.win.document_webview = Some(webview),
                Err(error) => {
                    self.win.document_webview_failed = true;
                    eprintln!("iai: {error}; using legacy document editor");
                    let message = format!("{error}; using legacy document editor");
                    self.shell.status_msg = message.clone();
                    self.shell.ui.document_editor_error = Some(message);
                    parent.request_redraw();
                }
            }
        }

        let pending_legacy_conversion = if self.win.document_webview.is_some() {
            match self.prepare_active_legacy_conversion() {
                Ok(conversion) => conversion,
                Err(error) => {
                    let message = format!(
                        "Cannot migrate the legacy document to Canvas Editor: {error}; using legacy document editor"
                    );
                    self.win.document_webview = None;
                    self.win.document_webview_failed = true;
                    self.shell.status_msg = message.clone();
                    self.shell.ui.document_editor_error = Some(message);
                    parent.request_redraw();
                    None
                }
            }
        } else {
            None
        };

        let active_flow_document_id = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .filter(|document| document.is_flow_text())
            .map(|document| document.id);
        let active_flow_read_only = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .and_then(|document| document.flow_text.as_ref())
            .is_some_and(|flow_text| flow_text.is_read_only());
        let active_flow_canvas_document = self
            .docs
            .documents
            .get(self.docs.active_doc_idx)
            .and_then(|document| document.flow_text.as_ref())
            .and_then(|flow_text| flow_text.canvas_editor_document())
            .map(|document| document.payload().clone())
            .or_else(|| {
                pending_legacy_conversion
                    .as_ref()
                    .filter(|(document_id, _, _)| Some(*document_id) == active_flow_document_id)
                    .map(|(_, _, conversion)| conversion.document.payload().clone())
            });
        let editor_theme = canvas_editor_theme_name(self.shell.ui.theme_mode);
        let mut snapshot_deadline = None;
        let mut lifecycle_completion = None;
        let mut core_snapshot_updates = Vec::new();
        let mut document_editor_error = None;
        let mut fall_back_to_legacy = false;
        let mut commit_legacy_conversion = false;
        if let Some(webview) = &mut self.win.document_webview {
            if let Some(status) = webview.poll_bridge() {
                eprintln!("iai: {status}");
                self.shell.status_msg = status;
            }
            document_editor_error = webview.take_error_dialog();
            if document_editor_error.is_some() {
                fall_back_to_legacy = !webview.bridge_verified;
                webview.hide();
            } else {
                if let Some(document_id) = active_flow_document_id {
                    webview.ensure_document(document_id, active_flow_canvas_document.as_ref());
                    if let Err(error) = webview.activate_document(document_id) {
                        self.shell.status_msg = error;
                    }
                }
                if let Err(error) = webview.sync_host_state(editor_theme, active_flow_read_only) {
                    self.shell.status_msg = error.clone();
                    document_editor_error = Some(error);
                    webview.hide();
                } else if let Err(error) = webview.set_frame(bounds, parent.scale_factor(), show) {
                    self.shell.status_msg = error.clone();
                    document_editor_error = Some(error);
                    webview.hide();
                }
            }
            commit_legacy_conversion = webview.bridge_verified && document_editor_error.is_none();
            snapshot_deadline = webview.snapshot_deadline();
            core_snapshot_updates = webview.take_core_snapshot_updates();
            lifecycle_completion = webview.take_lifecycle_completion();
            // A completion can change the host's active document below. Drive
            // deferred work only on a stable frame, otherwise Exit could
            // snapshot the source of a just-completed Switch instead of its
            // destination.
            if document_editor_error.is_some() {
                fall_back_to_legacy = !webview.bridge_verified;
            }
            if document_editor_error.is_none() && lifecycle_completion.is_none() {
                if let Some(status) = webview.drive_deferred_exit(active_flow_document_id) {
                    self.shell.status_msg = status;
                } else if let Some(status) = webview.drive_deferred_close(active_flow_document_id) {
                    self.shell.status_msg = status;
                } else if let Some(status) = webview.drive_deferred_switch(active_flow_document_id)
                {
                    self.shell.status_msg = status;
                }
                snapshot_deadline = webview.snapshot_deadline();
            }
        }

        if commit_legacy_conversion {
            if let Some((document_id, revision, conversion)) = pending_legacy_conversion {
                if let Some(flow_text) = self
                    .docs
                    .documents
                    .iter_mut()
                    .find(|document| document.id == document_id)
                    .and_then(|document| document.flow_text.as_mut())
                    .filter(|flow_text| {
                        flow_text.revision() == revision
                            && matches!(
                                flow_text.backing(),
                                crate::core::document::FlowTextBacking::Legacy(_)
                            )
                    })
                {
                    let warning_count = conversion.warnings.len();
                    flow_text.replace_canvas_editor_document(conversion.document, revision);
                    if warning_count > 0 {
                        self.shell.status_msg = format!(
                            "Legacy document migrated to Canvas Editor with {warning_count} compatibility warning(s)"
                        );
                    }
                }
            }
        }

        if let Some(mut error) = document_editor_error {
            if fall_back_to_legacy {
                error.push_str("; using legacy document editor");
                self.win.document_webview = None;
                self.win.document_webview_failed = true;
            }
            self.shell.status_msg = error.clone();
            self.shell.ui.document_editor_error = Some(error);
            lifecycle_completion = None;
            parent.request_redraw();
        }

        if let Some(deadline) = snapshot_deadline {
            self.win.egui_repaint_deadline = Some(
                self.win
                    .egui_repaint_deadline
                    .map_or(deadline, |current| current.min(deadline)),
            );
        }

        for update in core_snapshot_updates {
            if let Some(flow_text) = self
                .docs
                .documents
                .iter_mut()
                .find(|document| document.id == update.document_id)
                .and_then(|document| document.flow_text.as_mut())
            {
                flow_text.replace_canvas_editor_document(update.document, update.revision);
            }
        }

        match lifecycle_completion {
            Some(LifecycleCompletion::Save {
                document_id,
                revision,
                force_save_as,
            }) => {
                self.complete_document_webview_save(document_id, revision, force_save_as, parent);
            }
            Some(LifecycleCompletion::Switch(target_id)) => {
                if let Some(idx) = self
                    .docs
                    .documents
                    .iter()
                    .position(|document| document.id == target_id)
                {
                    self.switch_to_doc_confirmed(idx);
                }
            }
            Some(LifecycleCompletion::Close(document_id)) => {
                if let Some(idx) = self
                    .docs
                    .documents
                    .iter()
                    .position(|document| document.id == document_id)
                {
                    if self.document_webview_document_is_dirty(document_id) {
                        self.docs.pending_close_doc_idx = Some(idx);
                        self.shell.ui.show_close_dialog = true;
                    } else {
                        self.close_doc_confirmed(idx);
                    }
                    parent.request_redraw();
                }
            }
            Some(LifecycleCompletion::Exit) => {
                if self.continue_app_exit_after_webview_snapshot() {
                    self.shell.exit_requested = true;
                    // The snapshot completion runs during redraw, after the
                    // action phase that consumes `exit_requested`. Schedule
                    // one more frame so a clean-document exit actually reaches
                    // the event-loop exit call.
                    parent.request_redraw();
                }
            }
            None => {}
        }
    }

    pub(in crate::app) fn hide_document_webview(&mut self) {
        if let Some(webview) = &mut self.win.document_webview {
            webview.hide();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_and_pong_contract_is_versioned() {
        assert_eq!(
            parse_bridge_message(
                r#"{"protocol_version":1,"request_id":"web-1","type":"ready","editor_version":"1.0.2"}"#
            ),
            Ok(BridgeMessage::Ready {
                request_id: "web-1".to_string(),
                editor_version: "1.0.2".to_string(),
            })
        );
        assert_eq!(
            parse_bridge_message(r#"{"protocol_version":1,"request_id":"rust-1","type":"pong"}"#),
            Ok(BridgeMessage::Pong {
                request_id: "rust-1".to_string()
            })
        );
    }

    #[test]
    fn editor_error_contract_preserves_the_request_id() {
        assert_eq!(
            parse_bridge_message(
                r#"{"protocol_version":1,"request_id":"rust-snapshot","type":"error","code":"snapshot_failed","message":"cannot export document"}"#
            ),
            Ok(BridgeMessage::Error {
                request_id: "rust-snapshot".to_string(),
                code: "snapshot_failed".to_string(),
                message: "cannot export document".to_string(),
            })
        );
    }

    #[test]
    fn app_theme_maps_to_the_canvas_editor_protocol() {
        assert_eq!(
            canvas_editor_theme_name(crate::ui::theme::ThemeMode::Dark),
            "dark"
        );
    }

    #[test]
    fn probe_accepts_canvas_editor_metadata_normalization() {
        let expected = phase_two_probe_document();
        let actual = serde_json::json!({
            "header": [],
            "main": [{
                "value": "Tiếng Việt — Nguyễn Thị Thu — Ắ Ề Ễ Ự",
                "font": "Noto Sans",
                "size": 16,
                "type": "text"
            }],
            "footer": [],
            "graffiti": []
        });

        assert_ne!(expected, actual);
        assert!(probe_snapshot_matches(&expected, &actual));
    }

    #[test]
    fn switch_intent_waits_for_probe_and_retargets_an_inflight_switch() {
        let source = DocumentId(10);
        let first_target = DocumentId(11);
        let latest_target = DocumentId(12);
        let mut pending = Some(PendingSnapshot {
            request_id: "rust-probe".to_string(),
            purpose: SnapshotPurpose::Probe,
            deadline: std::time::Instant::now() + SNAPSHOT_TIMEOUT,
        });
        let mut deferred = None;

        defer_switch_intent(&mut pending, &mut deferred, source, first_target);
        assert_eq!(
            deferred,
            Some(DeferredSwitch {
                document_id: source,
                target_id: first_target
            })
        );

        pending = Some(PendingSnapshot {
            request_id: "rust-switch".to_string(),
            purpose: SnapshotPurpose::Switch {
                document_id: source,
                target_id: first_target,
            },
            deadline: std::time::Instant::now() + SNAPSHOT_TIMEOUT,
        });
        deferred = None;
        defer_switch_intent(&mut pending, &mut deferred, source, latest_target);

        assert!(deferred.is_none());
        assert_eq!(
            pending.map(|request| request.purpose),
            Some(SnapshotPurpose::Switch {
                document_id: source,
                target_id: latest_target
            })
        );
    }

    #[test]
    fn deferred_exit_waits_for_a_stable_verified_active_document() {
        let document_id = DocumentId(20);
        let other_document_id = DocumentId(21);

        assert_eq!(
            deferred_exit_document_id(true, false, true, Some(document_id), Some(document_id)),
            Some(document_id)
        );
        assert_eq!(
            deferred_exit_document_id(false, false, true, Some(document_id), Some(document_id)),
            None
        );
        assert_eq!(
            deferred_exit_document_id(true, true, true, Some(document_id), Some(document_id)),
            None
        );
        assert_eq!(
            deferred_exit_document_id(true, false, false, Some(document_id), Some(document_id)),
            None
        );
        assert_eq!(
            deferred_exit_document_id(
                true,
                false,
                true,
                Some(document_id),
                Some(other_document_id)
            ),
            None
        );
    }

    #[test]
    fn app_exit_promotes_an_inflight_snapshot_for_the_active_document() {
        let document_id = DocumentId(30);
        let target_id = DocumentId(31);
        let request_id = "rust-switch".to_string();
        let deadline = std::time::Instant::now() + SNAPSHOT_TIMEOUT;
        let mut pending = Some(PendingSnapshot {
            request_id: request_id.clone(),
            purpose: SnapshotPurpose::Switch {
                document_id,
                target_id,
            },
            deadline,
        });

        assert!(promote_pending_snapshot_to_exit(&mut pending, document_id));
        let promoted = pending.expect("the in-flight request is preserved");
        assert_eq!(promoted.request_id, request_id);
        assert_eq!(promoted.deadline, deadline);
        assert_eq!(promoted.purpose, SnapshotPurpose::Exit { document_id });

        let mut probe = Some(PendingSnapshot {
            request_id: "rust-probe".to_string(),
            purpose: SnapshotPurpose::Probe,
            deadline,
        });
        assert!(!promote_pending_snapshot_to_exit(&mut probe, document_id));
        assert_eq!(
            probe.map(|request| request.purpose),
            Some(SnapshotPurpose::Probe)
        );
    }

    #[test]
    fn close_promotes_an_inflight_autosave_without_downgrading_exit() {
        let document_id = DocumentId(40);
        let deadline = std::time::Instant::now() + SNAPSHOT_TIMEOUT;
        let mut autosave = Some(PendingSnapshot {
            request_id: "rust-autosave".to_string(),
            purpose: SnapshotPurpose::Autosave { document_id },
            deadline,
        });

        assert!(promote_pending_snapshot_to_close(
            &mut autosave,
            document_id
        ));
        assert_eq!(
            autosave.map(|request| request.purpose),
            Some(SnapshotPurpose::Close { document_id })
        );

        let mut exit = Some(PendingSnapshot {
            request_id: "rust-exit".to_string(),
            purpose: SnapshotPurpose::Exit { document_id },
            deadline,
        });
        assert!(promote_pending_snapshot_to_close(&mut exit, document_id));
        assert_eq!(
            exit.map(|request| request.purpose),
            Some(SnapshotPurpose::Exit { document_id })
        );
    }

    #[test]
    fn explicit_save_reuses_an_inflight_autosave_snapshot() {
        let document_id = DocumentId(50);
        let mut autosave = Some(PendingSnapshot {
            request_id: "rust-autosave".to_string(),
            purpose: SnapshotPurpose::Autosave { document_id },
            deadline: std::time::Instant::now() + SNAPSHOT_TIMEOUT,
        });

        assert!(promote_pending_autosave_to_save(
            &mut autosave,
            document_id,
            true
        ));
        assert_eq!(
            autosave.map(|request| request.purpose),
            Some(SnapshotPurpose::Save {
                document_id,
                force_save_as: true
            })
        );
    }

    #[test]
    fn validated_snapshot_is_ready_for_the_core_backing() {
        let document_id = DocumentId(51);
        let payload = serde_json::json!({
            "main": [{"type": "text", "value": "Xin chào", "future": {"kept": true}}],
            "header": [],
            "footer": [],
            "future_root": 42
        });

        let update = validate_core_snapshot_update(document_id, 9, payload.clone()).unwrap();
        assert_eq!(update.document_id, document_id);
        assert_eq!(update.revision, 9);
        assert_eq!(update.document.payload(), &payload);
    }

    #[test]
    fn saved_marker_never_clears_a_newer_editor_revision() {
        let mut state = BridgeDocumentState::blank();
        state.cache_snapshot(4, phase_two_probe_document()).unwrap();
        assert!(state.dirty);

        state.revision = 5;
        state.mark_saved(4);
        assert!(state.dirty);

        state.snapshot_revision = 5;
        state.mark_saved(5);
        assert!(!state.dirty);
    }

    #[test]
    fn malformed_version_and_oversized_ipc_are_rejected() {
        assert!(parse_bridge_message(
            r#"{"protocol_version":2,"request_id":"web-1","type":"ready","editor_version":"1.0.2"}"#
        )
        .is_err());
        assert!(parse_bridge_message(&"x".repeat(MAX_SNAPSHOT_MESSAGE_BYTES + 1)).is_err());
        assert!(parse_bridge_message(
            &serde_json::json!({
                "protocol_version": 1,
                "request_id": "web-large",
                "type": "document_changed",
                "revision": 1,
                "padding": "x".repeat(MAX_CONTROL_MESSAGE_BYTES)
            })
            .to_string()
        )
        .is_err());
    }

    #[test]
    fn change_and_snapshot_contract_preserve_revision_and_document() {
        assert_eq!(
            parse_bridge_message(
                r#"{"protocol_version":1,"request_id":"web-change","type":"document_changed","revision":7}"#
            ),
            Ok(BridgeMessage::DocumentChanged { revision: 7 })
        );
        let document = phase_two_probe_document();
        let raw = serde_json::json!({
            "protocol_version": 1,
            "request_id": "rust-snapshot",
            "type": "snapshot",
            "revision": 7,
            "document": document
        })
        .to_string();
        assert_eq!(
            parse_bridge_message(&raw),
            Ok(BridgeMessage::Snapshot {
                request_id: "rust-snapshot".to_string(),
                revision: 7,
                document: phase_two_probe_document(),
            })
        );
        assert_eq!(
            parse_bridge_message(
                r#"{"protocol_version":1,"request_id":"web-close","type":"close_requested","revision":7}"#
            ),
            Ok(BridgeMessage::CloseRequested { revision: 7 })
        );
    }

    #[test]
    fn snapshot_requires_canvas_editor_root_zones() {
        let invalid = serde_json::json!({
            "protocol_version": 1,
            "request_id": "rust-snapshot",
            "type": "snapshot",
            "revision": 0,
            "document": { "main": "not-an-array" }
        })
        .to_string();
        assert!(parse_bridge_message(&invalid).is_err());
    }

    #[test]
    fn snapshot_timeout_is_deterministic_at_the_deadline() {
        let started = std::time::Instant::now();
        let deadline = started + SNAPSHOT_TIMEOUT;
        assert!(!snapshot_timed_out(
            deadline,
            deadline - std::time::Duration::from_nanos(1)
        ));
        assert!(snapshot_timed_out(deadline, deadline));
    }

    #[test]
    fn bridge_failure_cancels_pending_lifecycle_intents() {
        let document_id = DocumentId(60);
        let target_id = DocumentId(61);
        let mut pending = Some(PendingSnapshot {
            request_id: "rust-exit".to_string(),
            purpose: SnapshotPurpose::Exit { document_id },
            deadline: std::time::Instant::now() + SNAPSHOT_TIMEOUT,
        });
        let mut deferred_switch = Some(DeferredSwitch {
            document_id,
            target_id,
        });
        let mut deferred_close = Some(document_id);
        let mut deferred_exit = true;

        abort_pending_bridge_work(
            &mut pending,
            &mut deferred_switch,
            &mut deferred_close,
            &mut deferred_exit,
        );

        assert!(pending.is_none());
        assert!(deferred_switch.is_none());
        assert!(deferred_close.is_none());
        assert!(!deferred_exit);
    }

    #[test]
    fn ctrl_w_close_survives_probe_and_supersedes_a_tab_switch() {
        let document_id = DocumentId(70);
        let mut pending = Some(PendingSnapshot {
            request_id: "rust-probe".to_string(),
            purpose: SnapshotPurpose::Probe,
            deadline: std::time::Instant::now() + SNAPSHOT_TIMEOUT,
        });
        let mut deferred_switch = Some(DeferredSwitch {
            document_id,
            target_id: DocumentId(71),
        });
        let mut deferred_close = None;

        defer_close_intent(
            &mut pending,
            &mut deferred_switch,
            &mut deferred_close,
            document_id,
        );

        assert!(deferred_switch.is_none());
        assert_eq!(deferred_close, Some(document_id));
        assert_eq!(
            pending.map(|request| request.purpose),
            Some(SnapshotPurpose::Probe)
        );
    }

    #[test]
    fn per_tab_state_distinguishes_dirty_from_unsnapshotted() {
        let mut state = BridgeDocumentState::blank();
        assert!(!state.dirty);
        assert!(!state.has_unsnapshotted_changes());

        // A lifecycle request can race ahead of Rust polling the queued
        // document_changed event. The snapshot's newer revision is therefore
        // authoritative and must still dirty/cache this tab.
        state
            .cache_snapshot(3, phase_two_probe_document())
            .expect("newer snapshot is accepted");
        assert!(
            state.dirty,
            "snapshotting must not pretend the file was saved"
        );
        assert!(!state.has_unsnapshotted_changes());

        state.revision = 4;
        assert!(state.has_unsnapshotted_changes());
        assert!(state.cache_snapshot(3, blank_editor_document()).is_err());
    }

    #[test]
    fn v12_payload_seeds_a_clean_per_tab_bridge_state() {
        let payload = serde_json::json!({
            "main": [{ "value": "Nội dung đã lưu", "future": true }],
            "unknown_root": { "kept": 1 }
        });
        let state = BridgeDocumentState::from_document(payload.clone());

        assert_eq!(state.document, payload);
        assert_eq!(state.revision, 0);
        assert_eq!(state.snapshot_revision, 0);
        assert!(!state.dirty);
    }

    #[test]
    fn blank_per_tab_document_obeys_the_canvas_editor_schema() {
        assert!(validate_editor_document(&blank_editor_document()).is_ok());
    }

    #[test]
    fn offline_protocol_serves_only_embedded_assets() {
        let index = asset_response(
            Request::builder()
                .uri("/index.html")
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(index.body().as_ref(), INDEX_HTML);

        let missing = asset_response(
            Request::builder()
                .uri("/remote.js")
                .body(Vec::new())
                .unwrap(),
        );
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert!(!String::from_utf8_lossy(INDEX_HTML).contains("https://"));
    }

    #[test]
    fn egui_bounds_scale_to_physical_webview_pixels() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(300.0, 200.0));
        assert_eq!(
            PhysicalWebViewRect::from_egui(rect, 1.25),
            Some(PhysicalWebViewRect {
                x: 13,
                y: 25,
                width: 375,
                height: 250,
            })
        );
    }

    #[test]
    fn releasing_editor_for_an_image_tab_restores_parent_focus_when_safe() {
        assert!(should_focus_parent_after_releasing_webview(
            true, false, false
        ));
        assert!(!should_focus_parent_after_releasing_webview(
            false, false, false
        ));
        assert!(!should_focus_parent_after_releasing_webview(
            true, true, false
        ));
        assert!(!should_focus_parent_after_releasing_webview(
            true, false, true
        ));
    }

    #[test]
    fn navigation_is_locked_to_the_offline_editor_origin() {
        assert!(allows_editor_navigation(
            "http://iai-editor.localhost/editor.js"
        ));
        assert!(!allows_editor_navigation("https://example.com/"));
    }
}
