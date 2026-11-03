//! The models iAi runs on this machine (ONNX Runtime, DirectML with a CPU
//! fallback): where they are and what each file is (`model_store`), fetching
//! and checking them, Select Subject (BiRefNet), LaMa inpainting and Smart
//! Fill, and with the `pro` feature the portrait models and the online edits.

pub mod ai;
pub mod lama;
pub mod model_store;
pub mod select_subject;
pub mod smart_fill;
