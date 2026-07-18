#![allow(dead_code)]
use crate::core::document::DocumentId;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub enum Event {
    StrokeBegin {
        doc_id: DocumentId,
        x: f32,
        y: f32,
        pressure: f32,
    },
    StrokeMove {
        doc_id: DocumentId,
        x: f32,
        y: f32,
        pressure: f32,
    },
    StrokeEnd {
        doc_id: DocumentId,
    },
    ToolChanged {
        tool_id: String,
    },
    BrushSizeChanged {
        size: f32,
    },
    BrushHardnessChanged {
        hardness: f32,
    },
    ColorChanged {
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    },
    PixelsUpdated {
        doc_id: DocumentId,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    CanvasResized {
        doc_id: DocumentId,
        width: u32,
        height: u32,
    },
    ViewZoomChanged {
        doc_id: DocumentId,
        zoom: f32,
        pivot_x: f32,
        pivot_y: f32,
    },
    ViewPanChanged {
        doc_id: DocumentId,
        offset_x: f32,
        offset_y: f32,
    },
    ViewFitToScreen {
        doc_id: DocumentId,
    },
    UndoRequested {
        doc_id: DocumentId,
    },
    RedoRequested {
        doc_id: DocumentId,
    },
    HistoryChanged {
        doc_id: DocumentId,
        undo_count: usize,
        redo_count: usize,
    },
    FileOpenRequested {
        path: String,
    },
    FileSaveRequested {
        doc_id: DocumentId,
        path: Option<String>,
    },
    FileLoaded {
        doc_id: DocumentId,
        width: u32,
        height: u32,
        path: String,
    },
    FileSaved {
        doc_id: DocumentId,
        path: String,
    },
    ShowNewCanvasDialog,
    NewCanvasConfirmed {
        width: u32,
        height: u32,
    },
    StatusMessage {
        message: String,
    },
}

pub type EventHandler = Arc<dyn Fn(&Event) + Send + Sync>;

pub struct EventBus {
    handlers: Mutex<Vec<EventHandler>>,
    queue: Mutex<Vec<Event>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            handlers: Mutex::new(Vec::new()),
            queue: Mutex::new(Vec::new()),
        }
    }

    pub fn subscribe(&self, handler: EventHandler) {
        self.handlers
            .lock()
            .expect("event bus handlers mutex poisoned")
            .push(handler);
    }

    pub fn emit(&self, event: Event) {
        let handlers: Vec<_> = self
            .handlers
            .lock()
            .expect("event bus handlers mutex poisoned")
            .clone();
        for handler in handlers {
            handler(&event);
        }
    }

    pub fn post(&self, event: Event) {
        self.queue
            .lock()
            .expect("event bus queue mutex poisoned")
            .push(event);
    }

    pub fn drain(&self) -> Vec<Event> {
        let mut queue = self.queue.lock().expect("event bus queue mutex poisoned");
        std::mem::take(&mut *queue)
    }
}

pub type SharedBus = Arc<EventBus>;

pub fn create_bus() -> SharedBus {
    Arc::new(EventBus::new())
}
