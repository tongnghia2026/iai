//! The commands of the free core. Each module is one area; `register_core`
//! puts them all in a registry. Pro commands are registered by the app.

pub mod ai;
pub mod analyze;
pub mod common;
pub mod doc;
pub mod edit;
pub mod export;
pub mod file;
pub mod image;
pub mod layer;
pub mod page;
pub mod select;
pub mod text;
pub mod vector;

use crate::Registry;

/// Every command of the free core.
pub fn register_core(r: &mut Registry) {
    r.register(doc::ListDocs);
    r.register(doc::Inspect);
    r.register(doc::Show);
    r.register(doc::RenderPreview);
    r.register(file::NewDocument);
    r.register(file::OpenFile);
    r.register(file::SaveFile);
    r.register(file::ExportImage);
    r.register(page::AddPage);
    r.register(page::SelectPage);
    r.register(layer::NewLayer);
    r.register(layer::DeleteLayer);
    r.register(layer::MoveLayer);
    r.register(layer::SetLayer);
    r.register(select::SelectAll);
    r.register(select::SelectNone);
    r.register(select::SelectInvert);
    r.register(select::SelectRect);
    r.register(select::SelectSubject);
    r.register(image::Crop);
    r.register(image::Resize);
    r.register(image::Rotate);
    r.register(image::Flip);
    r.register(ai::RemoveBackground);
    r.register(ai::Inpaint);
    r.register(ai::ModelDownload);
    r.register(export::ExportPdf);
    r.register(vector::AddShape);
    r.register(vector::AddPath);
    r.register(text::AddText);
    r.register(edit::Undo);
    r.register(edit::Redo);
    r.register(analyze::AnalyzeRegion);
}

/// A registry of the free core's commands.
pub fn core_registry() -> Registry {
    let mut r = Registry::new();
    register_core(&mut r);
    r
}

#[cfg(test)]
mod tests;
