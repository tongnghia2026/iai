//! The commands of the free core. Each module is one area; `register_core`
//! puts them all in a registry. Pro commands are registered by the app.

pub mod adjust;
pub mod ai;
pub mod analyze;
pub mod arrange;
pub mod basic;
pub mod channel;
pub mod common;
pub mod contact;
pub mod doc;
pub mod edit;
pub mod export;
pub mod file;
pub mod free_transform;
pub mod image;
pub mod layer;
pub mod mode;
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
    r.register(file::ListFiles);
    r.register(contact::ContactSheet);
    r.register(file::OpenFile);
    r.register(file::SaveFile);
    r.register(file::ExportImage);
    r.register(page::AddPage);
    r.register(page::SelectPage);
    r.register(page::DeletePage);
    r.register(page::MovePage);
    r.register(page::RenamePage);
    r.register(page::Master);
    r.register(page::Setup);
    r.register(layer::NewLayer);
    r.register(layer::DeleteLayer);
    r.register(layer::MoveLayer);
    r.register(layer::SetLayer);
    r.register(layer::PlaceImage);
    r.register(layer::LayerViaCopy);
    r.register(layer::LayerMask);
    r.register(layer::TransformLayer);
    r.register(free_transform::FreeTransform);
    r.register(arrange::AlignLayers);
    r.register(arrange::DistributeLayers);
    r.register(arrange::StepCopy);
    r.register(arrange::GroupLayers);
    r.register(arrange::Ungroup);
    r.register(arrange::Merge);
    r.register(arrange::FromBackground);
    r.register(arrange::Invert);
    r.register(arrange::Rasterize);
    r.register(arrange::Clip);
    r.register(arrange::SelectFromLayer);
    r.register(channel::EditChannel);
    r.register(channel::LoadChannel);
    r.register(select::SelectAll);
    r.register(select::SelectNone);
    r.register(select::SelectInvert);
    r.register(select::SelectRect);
    r.register(select::SelectEllipse);
    r.register(select::SelectPolygon);
    r.register(select::SelectSubject);
    r.register(image::Crop);
    r.register(image::PerspectiveCrop);
    r.register(adjust::Adjust);
    r.register(adjust::AutoLevels);
    r.register(adjust::ApplyFilter);
    r.register(adjust::ScanCleanup);
    r.register(basic::Fill);
    r.register(basic::Clear);
    r.register(basic::ModifySelection);
    r.register(basic::DuplicateLayer);
    r.register(basic::MergeDown);
    r.register(basic::Flatten);
    r.register(basic::Stroke);
    r.register(basic::CloseDocument);
    r.register(basic::ColorRange);
    r.register(basic::Wand);
    r.register(basic::Grayscale);
    r.register(image::Resize);
    r.register(image::CanvasSize);
    r.register(mode::Mode);
    r.register(mode::ColorProfile);
    r.register(image::Rotate);
    r.register(image::Flip);
    r.register(ai::RemoveBackground);
    r.register(ai::Inpaint);
    r.register(ai::ModelDownload);
    r.register(export::ExportPdf);
    r.register(vector::AddShape);
    r.register(vector::AddPath);
    r.register(vector::ToCurves);
    r.register(vector::Shaping);
    r.register(vector::PowerClip);
    r.register(text::AddText);
    r.register(text::SetText);
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
