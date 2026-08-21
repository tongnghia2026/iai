// Browser-extension bridge: a tiny localhost WebSocket server the IAI extension
// connects to. iai sends `edit` requests (image + prompt); the extension drives
// the user's real Gemini/ChatGPT tab in their own browser and sends the result
// image back. This replaces the embedded-webview "fake web" (which only worked on
// Windows/macOS) — the extension runs in the user's real Chromium browser
// (Chrome/Edge/Brave) so it behaves the same on Linux too.
//
// Sync `tungstenite` on a worker thread (matches the app's thread+mpsc pattern,
// e.g. `core/ai/edit.rs`); inbound events are drained once per frame by the app.

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use base64::Engine;
use tungstenite::Message;

/// Localhost-only. The extension connects to ws://127.0.0.1:PORT.
pub const PORT: u16 = 47821;
/// Longest edge uploaded to the browser (it forwards to Gemini/ChatGPT, which cap
/// around 1MP anyway). The result is kept at whatever resolution the model
/// returns — it is NOT upscaled back to the source canvas, which used to soften
/// (blur) results whenever the model returned an image smaller than the canvas.
const MAX_UPLOAD_EDGE: u32 = 1536;
const FIRST_PROGRESS_TIMEOUT: Duration = Duration::from_secs(18);

/// Events coming FROM the extension, drained by `App::poll_ext_bridge`.
pub enum ExtInbound {
    Connected,
    Disconnected,
    /// A human-readable progress line ("đính ảnh", "đang tạo"…).
    Status(String),
    /// The extension could not complete request `id`.
    Failed {
        id: u64,
        message: String,
        origin: Option<EditOrigin>,
    },
    /// A finished result image (base64 PNG) for request `id`.
    Result {
        id: u64,
        image_b64: String,
        origin: Option<EditOrigin>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOrigin {
    pub doc_id: u32,
    pub width: u32,
    pub height: u32,
    pub output_new_file: bool,
}

pub struct QueuedEdit {
    png: Vec<u8>,
    pub doc_id: u32,
    pub width: u32,
    pub height: u32,
    pub site: String,
    prompt: String,
    output_new_file: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Sent,
    Queued(usize),
}

/// Commands sent TO the extension.
enum ExtOutbound {
    Edit {
        id: u64,
        site: String,
        prompt: String,
        image_b64: String,
        /// True when iai already placed this image on the OS clipboard (native
        /// write — far more reliable than the extension's offscreen copy), so
        /// the extension can go straight to Ctrl+V.
        clipboard: bool,
    },
    Cancel {
        id: u64,
    },
}

pub struct ExtBridge {
    inbound: Receiver<ExtInbound>,
    outbound: Sender<ExtOutbound>,
    /// Shared secret the extension must echo in its `hello` — stops a random web
    /// page from connecting to the localhost port and stealing the canvas.
    pub token: String,
    pub connected: bool,
    pub status: String,
    /// Rolling log of recent status/error lines (newest last). The panel shows it
    /// so a message that flashed by in `status` can still be read.
    pub log: VecDeque<String>,
    /// Content hash of the image `send_edit` last wrote to the OS clipboard;
    /// `do_ext_edit` moves it into `App::os_clipboard_written` so paste keeps
    /// recognising app-written clipboard content.
    pub last_clipboard_write: Option<u64>,
    /// True between sending an edit and receiving its result.
    pub awaiting: bool,
    awaiting_id: Option<u64>,
    awaiting_started: Option<Instant>,
    awaiting_progress: bool,
    pub awaiting_site: Option<String>,
    next_id: u64,
    pub origin: Option<EditOrigin>,
    queue: VecDeque<QueuedEdit>,
}

impl Default for ExtBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtBridge {
    pub fn new() -> Self {
        let token = load_or_create_token();
        let (in_tx, in_rx) = mpsc::channel();
        let (out_tx, out_rx) = mpsc::channel();
        let server_token = token.clone();
        std::thread::spawn(move || server_loop(in_tx, out_rx, server_token));
        Self {
            inbound: in_rx,
            outbound: out_tx,
            token,
            connected: false,
            status: String::new(),
            log: VecDeque::new(),
            last_clipboard_write: None,
            awaiting: false,
            awaiting_id: None,
            awaiting_started: None,
            awaiting_progress: false,
            awaiting_site: None,
            next_id: 1,
            origin: None,
            queue: VecDeque::new(),
        }
    }

    /// Append a line to the rolling status log (deduping consecutive repeats).
    pub fn push_log(&mut self, line: &str) {
        if line.is_empty() || self.log.back().is_some_and(|last| last == line) {
            return;
        }
        self.log.push_back(line.to_string());
        while self.log.len() > 12 {
            self.log.pop_front();
        }
    }

    /// Encode once at enqueue time. Clipboard ownership is deliberately deferred
    /// until this job actually reaches the single browser slot.
    pub fn enqueue_edit(
        &mut self,
        rgba: Vec<u8>,
        w: u32,
        h: u32,
        site: &str,
        prompt: String,
        doc_id: u32,
        output_new_file: bool,
    ) -> Result<EnqueueOutcome, String> {
        if !self.connected {
            return Err("Extension chưa kết nối".to_string());
        }
        let img = image::RgbaImage::from_raw(w, h, rgba)
            .ok_or_else(|| "Không mã hoá được ảnh đầu vào".to_string())?;
        let dynimg = downscale(image::DynamicImage::ImageRgba8(img), MAX_UPLOAD_EDGE);
        let mut png = Vec::new();
        dynimg
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| format!("Không mã hoá được PNG: {e}"))?;
        let job = QueuedEdit {
            png,
            doc_id,
            width: w,
            height: h,
            site: site.to_string(),
            prompt,
            output_new_file,
        };

        if self.awaiting {
            self.queue.push_back(job);
            let pos = self.queue.len();
            let line = format!("Đã thêm vào hàng chờ (vị trí {pos})…");
            self.status = line.clone();
            self.push_log(&line);
            Ok(EnqueueOutcome::Queued(pos))
        } else {
            self.dispatch(job)?;
            Ok(EnqueueOutcome::Sent)
        }
    }

    fn dispatch(&mut self, job: QueuedEdit) -> Result<(), String> {
        let small = image::load_from_memory(&job.png)
            .map_err(|e| format!("Không đọc được PNG trong hàng chờ: {e}"))?
            .to_rgba8();
        #[cfg(not(test))]
        {
            self.last_clipboard_write = crate::app::os_clipboard::write_image(
                small.width(),
                small.height(),
                small.as_raw(),
            )
            .ok();
        }
        #[cfg(test)]
        {
            let _ = small;
            self.last_clipboard_write = None;
        }
        let clipboard = self.last_clipboard_write.is_some();
        let image_b64 = base64::engine::general_purpose::STANDARD.encode(&job.png);
        let id = self.next_id;
        self.next_id += 1;
        self.outbound
            .send(ExtOutbound::Edit {
                id,
                site: job.site.clone(),
                prompt: job.prompt,
                image_b64,
                clipboard,
            })
            .map_err(|_| "Bridge extension đã dừng".to_string())?;
        self.awaiting = true;
        self.awaiting_id = Some(id);
        self.awaiting_started = Some(Instant::now());
        self.awaiting_progress = false;
        self.awaiting_site = Some(job.site);
        self.origin = Some(EditOrigin {
            doc_id: job.doc_id,
            width: job.width,
            height: job.height,
            output_new_file: job.output_new_file,
        });
        Ok(())
    }

    fn clear_awaiting(&mut self) -> Option<EditOrigin> {
        self.awaiting = false;
        self.awaiting_id = None;
        self.awaiting_started = None;
        self.awaiting_progress = false;
        self.awaiting_site = None;
        self.origin.take()
    }

    pub fn busy(&self) -> bool {
        self.awaiting || !self.queue.is_empty()
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    pub fn doc_busy(&self, doc_id: u32) -> bool {
        self.origin.is_some_and(|origin| origin.doc_id == doc_id)
            || self.queue.iter().any(|job| job.doc_id == doc_id)
    }

    pub fn queued_pos(&self, doc_id: u32) -> Option<usize> {
        self.queue
            .iter()
            .position(|job| job.doc_id == doc_id)
            .map(|index| index + 1)
    }

    pub fn queued_for_doc(&self, doc_id: u32) -> Option<&QueuedEdit> {
        self.queue.iter().find(|job| job.doc_id == doc_id)
    }

    pub fn awaiting_started(&self) -> Option<Instant> {
        self.awaiting_started
    }

    pub fn cancel_for_doc(&mut self, doc_id: u32) -> bool {
        if self.origin.is_some_and(|origin| origin.doc_id == doc_id) {
            if let Some(id) = self.awaiting_id {
                let _ = self.outbound.send(ExtOutbound::Cancel { id });
            }
            self.clear_awaiting();
            self.status = "Đã hủy lệnh đang gửi sang extension".to_string();
            let line = self.status.clone();
            self.push_log(&line);
            return true;
        }
        if let Some(index) = self.queue.iter().position(|job| job.doc_id == doc_id) {
            self.queue.remove(index);
            self.status = "Đã xóa lệnh khỏi hàng chờ".to_string();
            let line = self.status.clone();
            self.push_log(&line);
            return true;
        }
        false
    }

    pub fn remove_doc_jobs(&mut self, doc_id: u32) -> bool {
        let mut removed = self.cancel_for_doc(doc_id);
        let before = self.queue.len();
        self.queue.retain(|job| job.doc_id != doc_id);
        removed |= self.queue.len() != before;
        if removed {
            self.status = "Đã hủy lệnh của tài liệu đã đóng".to_string();
            let line = self.status.clone();
            self.push_log(&line);
        }
        removed
    }

    /// Drain all pending events from the extension (called once per frame).
    pub fn drain(&mut self) -> Vec<ExtInbound> {
        let mut out = Vec::new();
        loop {
            match self.inbound.try_recv() {
                Ok(ev) => match ev {
                    ExtInbound::Connected => {
                        self.connected = true;
                        out.push(ExtInbound::Connected);
                    }
                    ExtInbound::Disconnected => {
                        self.connected = false;
                        self.push_log("Extension ngắt kết nối");
                        out.push(ExtInbound::Disconnected);
                        if self.awaiting {
                            let id = self.awaiting_id.unwrap_or(0);
                            let origin = self.clear_awaiting();
                            let message = "Extension ngắt kết nối khi đang xử lý".to_string();
                            self.status = message.clone();
                            self.push_log(&message);
                            out.push(ExtInbound::Failed {
                                id,
                                message,
                                origin,
                            });
                        }
                    }
                    ExtInbound::Status(s) => {
                        if self.awaiting {
                            self.awaiting_progress = true;
                        }
                        self.status = s.clone();
                        self.push_log(&s);
                        out.push(ExtInbound::Status(s));
                    }
                    ExtInbound::Failed { id, message, .. } => {
                        if id == 0 || self.awaiting_id.is_some_and(|cur| cur == id) {
                            let origin = self.clear_awaiting();
                            self.status = message.clone();
                            let line = format!("Lỗi: {message}");
                            self.push_log(&line);
                            out.push(ExtInbound::Failed {
                                id,
                                message,
                                origin,
                            });
                        }
                    }
                    ExtInbound::Result { id, image_b64, .. } => {
                        if self.awaiting_id.is_some_and(|cur| cur == id || id == 0) {
                            let origin = self.clear_awaiting();
                            out.push(ExtInbound::Result {
                                id,
                                image_b64,
                                origin,
                            });
                        }
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if self.awaiting
            && !self.awaiting_progress
            && self
                .awaiting_started
                .is_some_and(|t| t.elapsed() >= FIRST_PROGRESS_TIMEOUT)
        {
            let id = self.awaiting_id.take().unwrap_or(0);
            let origin = self.clear_awaiting();
            let message =
                "Không thấy phản hồi từ extension sau khi gửi — thử reload extension/tab Gemini"
                    .to_string();
            self.status = message.clone();
            self.push_log(&message);
            out.push(ExtInbound::Failed {
                id,
                message,
                origin,
            });
        }

        // This is the only automatic queue-advance point. Every path that opens
        // the browser slot (result, failure, timeout, cancel, reconnect) converges
        // here on the next drain.
        if self.connected && !self.awaiting {
            if let Some(job) = self.queue.pop_front() {
                let site = job.site.clone();
                let failed_origin = EditOrigin {
                    doc_id: job.doc_id,
                    width: job.width,
                    height: job.height,
                    output_new_file: job.output_new_file,
                };
                match self.dispatch(job) {
                    Ok(()) => {
                        let line = format!("Gửi lệnh trong hàng chờ sang {site}…");
                        self.status = line.clone();
                        self.push_log(&line);
                    }
                    Err(message) => {
                        self.status = message.clone();
                        self.push_log(&message);
                        out.push(ExtInbound::Failed {
                            id: 0,
                            message,
                            origin: Some(failed_origin),
                        });
                    }
                }
            }
        }
        out
    }
}

/// Accept loop: one client at a time. Reconnects after a drop.
fn server_loop(inbound: Sender<ExtInbound>, outbound: Receiver<ExtOutbound>, token: String) {
    let listener = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => l,
        Err(e) => {
            let _ = inbound.send(ExtInbound::Status(format!(
                "Khong mo duoc extension bridge port {PORT}: {e}"
            )));
            return;
        }
    };
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // Do the WS handshake with a BLOCKING stream (a read timeout here can abort
        // the upgrade); only after it succeeds set a short per-message read timeout
        // so the single thread can poll reads and writes in turn.
        let mut ws = match tungstenite::accept(stream) {
            Ok(w) => w,
            Err(_) => continue,
        };
        ws.get_ref()
            .set_read_timeout(Some(Duration::from_millis(120)))
            .ok();

        let mut authed = false;

        loop {
            // Flush queued outbound commands (only once the client has authed).
            let mut send_err = false;
            while let Ok(cmd) = outbound.try_recv() {
                if !authed {
                    continue; // drop pre-auth commands
                }
                let json = match cmd {
                    ExtOutbound::Edit {
                        id,
                        site,
                        prompt,
                        image_b64,
                        clipboard,
                    } => serde_json::json!({
                        "type": "edit", "id": id, "site": site,
                        "prompt": prompt, "image": image_b64,
                        "clipboard": clipboard
                    })
                    .to_string(),
                    ExtOutbound::Cancel { id } => serde_json::json!({
                        "type": "cancel", "id": id
                    })
                    .to_string(),
                };
                if ws.send(Message::Text(json.into())).is_err() {
                    send_err = true;
                    break;
                }
            }
            if send_err {
                break;
            }

            match ws.read() {
                Ok(Message::Text(t)) => {
                    if handle_text(&inbound, Some(&mut ws), t.as_str(), &token, &mut authed)
                        .is_break()
                    {
                        break;
                    }
                }
                Ok(Message::Close(_)) => break,
                Ok(Message::Ping(p)) => {
                    let _ = ws.send(Message::Pong(p));
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                {
                    // No message within the read timeout — loop to flush outbound.
                }
                Err(_) => break,
            }
        }
        let _ = inbound.send(ExtInbound::Disconnected);
    }
}

use std::ops::ControlFlow;

fn handle_text(
    inbound: &Sender<ExtInbound>,
    mut ws: Option<&mut tungstenite::WebSocket<std::net::TcpStream>>,
    text: &str,
    token: &str,
    authed: &mut bool,
) -> ControlFlow<()> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return ControlFlow::Continue(());
    };
    match v["type"].as_str().unwrap_or("") {
        "hello" => {
            if v["token"].as_str() == Some(token) {
                *authed = true;
                let site = v["site"].as_str().unwrap_or("?").to_string();
                let _ = inbound.send(ExtInbound::Connected);
                let _ = inbound.send(ExtInbound::Status(format!("Extension đã kết nối ({site})")));
            } else {
                let message = "Sai token extension - copy lai token tu iAi vao popup".to_string();
                if let Some(ws) = ws.as_mut() {
                    let _ = ws.send(Message::Text(
                        serde_json::json!({ "type": "error", "id": 0, "message": message })
                            .to_string()
                            .into(),
                    ));
                }
                let _ = inbound.send(ExtInbound::Status(message));
                return ControlFlow::Break(());
            }
        }
        "status" if *authed => {
            if let Some(msg) = v["message"].as_str() {
                let _ = inbound.send(ExtInbound::Status(msg.to_string()));
            }
        }
        "error" if *authed => {
            let id = v["id"].as_u64().unwrap_or(0);
            let message = v["message"]
                .as_str()
                .unwrap_or("Extension khong hoan tat duoc request")
                .to_string();
            let _ = inbound.send(ExtInbound::Failed {
                id,
                message,
                origin: None,
            });
        }
        "result" if *authed => {
            if let Some(img) = v["image"].as_str() {
                let id = v["id"].as_u64().unwrap_or(0);
                let _ = inbound.send(ExtInbound::Result {
                    id,
                    image_b64: img.to_string(),
                    origin: None,
                });
            }
        }
        _ => {}
    }
    ControlFlow::Continue(())
}

fn downscale(img: image::DynamicImage, max_edge: u32) -> image::DynamicImage {
    let longest = img.width().max(img.height());
    if longest <= max_edge {
        return img;
    }
    let scale = max_edge as f32 / longest as f32;
    let nw = ((img.width() as f32 * scale).round() as u32).max(1);
    let nh = ((img.height() as f32 * scale).round() as u32).max(1);
    img.resize(nw, nh, image::imageops::FilterType::Lanczos3)
}

/// Decode a base64 PNG result into RGBA at exactly `w×h` (so it lines up as a
/// layer over the Background). Used by the app when a `Result` arrives.
/// Decode an extension result image at its NATIVE resolution — returns the RGBA
/// bytes plus the real pixel dimensions. Earlier this force-resized to the source
/// canvas size, which upscaled (and softened) any model output smaller than the
/// canvas; the result now keeps exactly the resolution the browser showed.
pub fn decode_result(image_b64: &str) -> Result<(Vec<u8>, u32, u32), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image_b64.as_bytes())
        .map_err(|e| format!("decode base64: {e}"))?;
    let out = image::load_from_memory(&bytes).map_err(|e| format!("load result: {e}"))?;
    let rgba = out.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Ok((rgba.into_raw(), w, h))
}

/// Non-cryptographic per-session token: enough to stop a casual web page from
/// connecting to the localhost port. (Not a security boundary against a
/// determined local attacker — localhost only, personal use.)
fn gen_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mix = nanos ^ ((std::process::id() as u128) << 64) ^ (&nanos as *const _ as u128);
    format!("{mix:032x}")
}

/// Persist the token next to ai.json so it is STABLE across launches — the user
/// pastes it into the extension once, not every time iai restarts.
fn load_or_create_token() -> String {
    let path = token_path();
    if let Ok(s) = std::fs::read_to_string(&path) {
        let t = s.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let t = gen_token();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, &t);
    t
}

fn token_path() -> std::path::PathBuf {
    let dir = if let Ok(appdata) = std::env::var("APPDATA") {
        std::path::PathBuf::from(appdata).join("IAI")
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("iai")
    } else {
        std::path::PathBuf::from(".")
    };
    dir.join("ext_token.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_bridge() -> (ExtBridge, Sender<ExtInbound>, Receiver<ExtOutbound>) {
        let (in_tx, in_rx) = mpsc::channel();
        let (out_tx, out_rx) = mpsc::channel();
        (
            ExtBridge {
                inbound: in_rx,
                outbound: out_tx,
                token: "good".to_string(),
                connected: true,
                status: String::new(),
                log: VecDeque::new(),
                last_clipboard_write: None,
                awaiting: false,
                awaiting_id: None,
                awaiting_started: None,
                awaiting_progress: false,
                awaiting_site: None,
                next_id: 1,
                origin: None,
                queue: VecDeque::new(),
            },
            in_tx,
            out_rx,
        )
    }

    #[test]
    fn decode_result_keeps_native_resolution() {
        // A 7x3 image must decode back as 7x3 — NOT resized to any canvas size.
        let img = image::RgbaImage::from_pixel(7, 3, image::Rgba([10, 20, 30, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode png");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let (rgba, w, h) = decode_result(&b64).expect("decode");
        assert_eq!((w, h), (7, 3), "native dimensions preserved");
        assert_eq!(rgba.len(), 7 * 3 * 4);
        assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
    }

    fn enqueue(bridge: &mut ExtBridge, doc_id: u32, site: &str) -> EnqueueOutcome {
        bridge
            .enqueue_edit(
                vec![10, 20, 30, 255],
                1,
                1,
                site,
                "edit".to_string(),
                doc_id,
                false,
            )
            .unwrap()
    }

    #[test]
    fn auth_gate_and_result_routing() {
        let (tx, rx) = mpsc::channel();

        // Wrong token → connection broken, not authed.
        let mut authed = false;
        let r = handle_text(
            &tx,
            None,
            r#"{"type":"hello","token":"bad","site":"gemini"}"#,
            "good",
            &mut authed,
        );
        assert!(r.is_break());
        assert!(!authed);

        // Right token → authed, continue.
        let mut authed = false;
        let r = handle_text(
            &tx,
            None,
            r#"{"type":"hello","token":"good","site":"gemini"}"#,
            "good",
            &mut authed,
        );
        assert!(r.is_continue());
        assert!(authed);

        // A result after auth is routed with its id.
        let _ = handle_text(
            &tx,
            None,
            r#"{"type":"result","id":7,"image":"QUJD"}"#,
            "good",
            &mut authed,
        );

        let mut got = false;
        while let Ok(ev) = rx.try_recv() {
            if let ExtInbound::Result { id, image_b64, .. } = ev {
                assert_eq!(id, 7);
                assert_eq!(image_b64, "QUJD");
                got = true;
            }
        }
        assert!(got, "result event should have been emitted");
    }

    #[test]
    fn server_completes_websocket_handshake() {
        use std::io::{Read, Write};
        if std::net::TcpStream::connect(("127.0.0.1", PORT)).is_ok() {
            eprintln!("port {PORT} already in use; skipping fixed-port handshake test");
            return;
        }
        let _bridge = ExtBridge::new();
        std::thread::sleep(Duration::from_millis(250)); // let the server bind
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", PORT)).expect("tcp connect");
        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
        let req = "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n\
                   Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                   Sec-WebSocket-Version: 13\r\n\r\n";
        stream.write_all(req.as_bytes()).expect("write upgrade");
        let mut buf = [0u8; 256];
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                eprintln!(
                    "fixed-port handshake test timed out; port may be held by another process"
                );
                return;
            }
            Err(e) => panic!("read handshake response: {e}"),
        };
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(
            resp.contains("101"),
            "server must answer 101 Switching Protocols, got: {resp}"
        );
    }

    #[test]
    fn result_before_auth_is_ignored() {
        let (tx, rx) = mpsc::channel();
        let mut authed = false;
        let _ = handle_text(
            &tx,
            None,
            r#"{"type":"result","id":1,"image":"x"}"#,
            "good",
            &mut authed,
        );
        assert!(rx.try_recv().is_err(), "pre-auth result must be dropped");
    }

    #[test]
    fn error_event_unlocks_awaiting_request() {
        let (mut bridge, tx, _out_rx) = test_bridge();
        bridge.awaiting = true;
        bridge.awaiting_id = Some(42);
        bridge.awaiting_started = Some(Instant::now());

        let mut authed = true;
        let _ = handle_text(
            &tx,
            None,
            r#"{"type":"error","id":42,"message":"timeout"}"#,
            "good",
            &mut authed,
        );
        bridge.drain();

        assert!(!bridge.awaiting);
        assert_eq!(bridge.awaiting_id, None);
        assert_eq!(bridge.status, "timeout");
    }

    #[test]
    fn cancel_for_doc_unlocks_local_request() {
        let (mut bridge, _in_tx, out_rx) = test_bridge();
        bridge.awaiting = true;
        bridge.awaiting_id = Some(9);
        bridge.awaiting_started = Some(Instant::now());
        bridge.origin = Some(EditOrigin {
            doc_id: 1,
            width: 2,
            height: 3,
            output_new_file: false,
        });

        assert!(bridge.cancel_for_doc(1));

        assert!(!bridge.awaiting);
        assert_eq!(bridge.awaiting_id, None);
        assert_eq!(bridge.origin, None);
        assert!(matches!(
            out_rx.try_recv(),
            Ok(ExtOutbound::Cancel { id: 9 })
        ));
    }

    #[test]
    fn queued_edit_waits_then_dispatches_after_result() {
        let (mut bridge, in_tx, out_rx) = test_bridge();
        assert_eq!(enqueue(&mut bridge, 1, "gemini"), EnqueueOutcome::Sent);
        assert!(matches!(
            out_rx.try_recv(),
            Ok(ExtOutbound::Edit { id: 1, .. })
        ));
        assert_eq!(
            enqueue(&mut bridge, 2, "chatgpt"),
            EnqueueOutcome::Queued(1)
        );
        assert!(
            out_rx.try_recv().is_err(),
            "queued edit must not hit socket"
        );

        in_tx
            .send(ExtInbound::Result {
                id: 1,
                image_b64: "result".to_string(),
                origin: None,
            })
            .unwrap();
        let events = bridge.drain();
        assert!(matches!(
            events.as_slice(),
            [ExtInbound::Result { id: 1, .. }]
        ));
        assert!(matches!(
            out_rx.try_recv(),
            Ok(ExtOutbound::Edit { id: 2, .. })
        ));
        assert_eq!(bridge.origin.unwrap().doc_id, 2);
    }

    #[test]
    fn cancel_for_doc_handles_queue_and_in_flight() {
        let (mut bridge, _in_tx, out_rx) = test_bridge();
        enqueue(&mut bridge, 1, "gemini");
        let _ = out_rx.try_recv();
        enqueue(&mut bridge, 2, "chatgpt");
        assert!(bridge.cancel_for_doc(2));
        assert_eq!(bridge.queue_len(), 0);
        assert!(bridge.cancel_for_doc(1));
        assert!(!bridge.awaiting);
        assert!(matches!(
            out_rx.try_recv(),
            Ok(ExtOutbound::Cancel { id: 1 })
        ));
    }

    #[test]
    fn timeout_fails_current_and_dispatches_queue_head() {
        let (mut bridge, _in_tx, out_rx) = test_bridge();
        enqueue(&mut bridge, 1, "gemini");
        let _ = out_rx.try_recv();
        enqueue(&mut bridge, 2, "chatgpt");
        bridge.awaiting_started = Some(Instant::now() - FIRST_PROGRESS_TIMEOUT);

        let events = bridge.drain();
        assert!(matches!(
            events.as_slice(),
            [ExtInbound::Failed { id: 1, .. }]
        ));
        assert!(matches!(
            out_rx.try_recv(),
            Ok(ExtOutbound::Edit { id: 2, .. })
        ));
    }

    #[test]
    fn disconnect_fails_in_flight_but_keeps_queue_for_reconnect() {
        let (mut bridge, in_tx, out_rx) = test_bridge();
        enqueue(&mut bridge, 1, "gemini");
        let _ = out_rx.try_recv();
        enqueue(&mut bridge, 2, "chatgpt");

        in_tx.send(ExtInbound::Disconnected).unwrap();
        let events = bridge.drain();
        assert!(matches!(
            events.as_slice(),
            [ExtInbound::Disconnected, ExtInbound::Failed { id: 1, .. }]
        ));
        assert_eq!(bridge.queue_len(), 1);
        assert!(out_rx.try_recv().is_err());

        in_tx.send(ExtInbound::Connected).unwrap();
        bridge.drain();
        assert!(matches!(
            out_rx.try_recv(),
            Ok(ExtOutbound::Edit { id: 2, .. })
        ));
    }

    #[test]
    fn remove_doc_jobs_culls_queued_job() {
        let (mut bridge, _in_tx, out_rx) = test_bridge();
        enqueue(&mut bridge, 1, "gemini");
        let _ = out_rx.try_recv();
        enqueue(&mut bridge, 2, "chatgpt");
        assert!(bridge.remove_doc_jobs(2));
        assert!(!bridge.doc_busy(2));
        assert_eq!(bridge.queue_len(), 0);
    }
}
