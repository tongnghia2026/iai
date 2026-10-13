// The models that run on this machine. What every build has is here: how a
// model's session is built (`ort_ep`) and how its file is fetched and checked
// (`download`), which Select Subject and Smart Fill run on. The rest is the
// Pro part: the models of the portrait retouch, and the edits sent to Gemini /
// ChatGPT (`edit`, `studio`).

#[cfg(feature = "pro")]
pub mod body_parts;
pub mod download;
#[cfg(feature = "pro")]
pub mod edit;
#[cfg(feature = "pro")]
pub mod face_mesh;
pub mod ort_ep;
#[cfg(feature = "pro")]
pub mod pose;
#[cfg(feature = "pro")]
pub(crate) mod retouch;
#[cfg(feature = "pro")]
pub mod settings;
#[cfg(feature = "pro")]
mod studio;

#[cfg(feature = "pro")]
pub use settings::AiProvider;
#[cfg(feature = "pro")]
pub use studio::{guarded_edit_prompt, AiPanelState, AiSource};

/// Tests that run the ONNX models take turns: started together they starve
/// one another past their own deadlines. The turn is held until the calling
/// test's thread ends, or until that thread asks for another.
#[cfg(all(test, feature = "pro"))]
pub(crate) fn take_model_test_turn() {
    use std::cell::RefCell;
    use std::sync::{Mutex, MutexGuard};

    static TURN: Mutex<()> = Mutex::new(());
    thread_local! {
        static HELD: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
    }
    HELD.with(|held| {
        let mut held = held.borrow_mut();
        // `--test-threads=1` runs every test on the one thread.
        *held = None;
        *held = Some(TURN.lock().unwrap_or_else(|e| e.into_inner()));
    });
}

/// How long a test waits on a model before calling the work hung. Generous on
/// purpose: these tests run beside two thousand others, and a worker a test
/// walked away from keeps running under the next one. A real hang still
/// fails, only later.
#[cfg(all(test, feature = "pro"))]
pub(crate) const MODEL_TEST_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);
