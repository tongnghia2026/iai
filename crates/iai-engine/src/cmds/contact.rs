//! A folder's photos at a glance: one picture of small numbered copies, to
//! tell what each file is without opening them one by one.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, ErrorCode, Job, Plan, Session, Validate,
};

/// Endings read as photos.
const PHOTOS: [&str; 7] = ["jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp"];
/// The most photos on one sheet.
const MAX_ON_SHEET: usize = 30;

static CONTACT_SHEET: CommandMeta = CommandMeta {
    id: "file.contact_sheet",
    label_vi: "Xem nhanh ảnh trong thư mục",
    label_en: "Contact Sheet",
    doc: "One picture of the photos of a folder (or of the files given), small and numbered, \
          with the list of what each number is: to sort a customer's photos (ID card fronts \
          and backs, phone screenshots, portraits…) in one look instead of opening each. \
          Changes nothing.",
    effects: &[Effect::ReadFiles],
    featured: true,
};

pub struct ContactSheet;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContactSheetParams {
    /// The folder; its photos by name.
    #[schemars(length(min = 1, max = 4096))]
    pub path: Option<String>,
    /// Or these files, in this order.
    #[serde(default)]
    #[schemars(length(max = 30))]
    pub files: Vec<FileRef>,
    /// Skip this many of the folder's photos (for the next sheet).
    #[serde(default)]
    pub start: usize,
    /// The side of each small copy, in pixels.
    #[serde(default = "cell_256")]
    #[schemars(range(min = 96, max = 512))]
    pub cell_px: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
}

fn cell_256() -> u32 {
    256
}

impl Validate for ContactSheetParams {
    fn validate(&self) -> Result<(), CmdError> {
        match (&self.path, self.files.is_empty()) {
            (Some(_), true) | (None, false) => Ok(()),
            _ => Err(CmdError::bad_param(
                "path",
                "a folder, or `files`, not both",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SheetEntry {
    /// The number on its copy.
    pub n: usize,
    pub name: String,
    pub path: String,
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Sheet {
    pub photos: Vec<SheetEntry>,
    /// Files that could not be read as photos.
    pub unread: Vec<String>,
    /// Photos of the folder after these (ask again with `start`).
    pub more: usize,
    /// The picture, PNG.
    pub png_base64: String,
}

/// The photos of `dir`, by name.
fn folder_photos(dir: &Path) -> Result<Vec<PathBuf>, CmdError> {
    let read = std::fs::read_dir(dir).map_err(|e| {
        CmdError::new(
            ErrorCode::NotFound,
            format!("no folder {}: {e}", dir.display()),
        )
        .vi(format!("Không có thư mục {}", dir.display()))
    })?;
    let mut photos: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .is_some_and(|e| PHOTOS.contains(&e.as_str()))
        })
        .collect();
    photos.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    Ok(photos)
}

/// A photo as it is meant to be seen (turned as the camera says), made to
/// fit `edge` × `edge`.
fn thumbnail(path: &Path, edge: u32) -> Option<(image::RgbaImage, u32, u32)> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let orientation = decoder.orientation().ok();
    let mut photo = image::DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        photo.apply_orientation(o);
    }
    let (w, h) = (photo.width(), photo.height());
    Some((photo.thumbnail(edge, edge).to_rgba8(), w, h))
}

/// The digits 0 to 9, five pixels wide and seven high, a row per byte.
const DIGITS: [[u8; 7]; 10] = [
    [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
    [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
    [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
    [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
    [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
    [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
    [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
    [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
    [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
];

/// A `w` × `h` block of `colour` at `(x, y)`, cut to the sheet.
fn block(sheet: &mut image::RgbaImage, (x, y): (u32, u32), (w, h): (u32, u32), colour: [u8; 4]) {
    let (right, bottom) = (
        x.saturating_add(w).min(sheet.width()),
        y.saturating_add(h).min(sheet.height()),
    );
    for ty in y..bottom {
        for tx in x..right {
            sheet.put_pixel(tx, ty, image::Rgba(colour));
        }
    }
}

/// `n` written in white on a black tag at `(x, y)`, `scale` pixels a dot.
fn label(sheet: &mut image::RgbaImage, n: usize, x: u32, y: u32, scale: u32) {
    let text = n.to_string();
    let digits = u32::try_from(text.len()).unwrap_or(1);
    let size = (
        digits
            .saturating_mul(6)
            .saturating_add(1)
            .saturating_mul(scale),
        scale.saturating_mul(9),
    );
    block(sheet, (x, y), size, [0, 0, 0, 255]);
    let mut left_of_digit = x.saturating_add(scale);
    for c in text.chars() {
        let Some(rows) = c.to_digit(10).and_then(|d| DIGITS.get(d as usize)) else {
            continue;
        };
        let mut top = y.saturating_add(scale);
        for bits in rows {
            let mut left = left_of_digit;
            for col in 0..5u32 {
                if bits & (0x10 >> col) != 0 {
                    block(sheet, (left, top), (scale, scale), [255, 255, 255, 255]);
                }
                left = left.saturating_add(scale);
            }
            top = top.saturating_add(scale);
        }
        left_of_digit = left_of_digit.saturating_add(scale.saturating_mul(6));
    }
}

/// Space around each copy.
const GAP: u32 = 8;

/// Columns and rows for `count` copies: one row up to four, five across
/// past nine.
fn grid(count: usize) -> (u32, u32) {
    let count = count.max(1) as u32;
    let columns = match count {
        1..=4 => count,
        5..=9 => 3.max(count.div_ceil(2)).min(5),
        _ => 5,
    };
    (columns, count.div_ceil(columns))
}

/// The sheet of `paths`, numbered from `first`.
fn make(
    paths: &[PathBuf],
    first: usize,
    cell: u32,
) -> Result<(Vec<SheetEntry>, Vec<String>, String), CmdError> {
    // A picture of 2048 a side at most: what an agent is shown anyway.
    let (columns, rows) = grid(paths.len());
    let cell = cell.min(
        2048u32
            .saturating_sub(GAP)
            .checked_div(columns.max(rows))
            .unwrap_or(0)
            .saturating_sub(GAP)
            .max(64),
    );
    let small: Vec<Option<(image::RgbaImage, u32, u32)>> =
        paths.par_iter().map(|p| thumbnail(p, cell)).collect();
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut entries = Vec::new();
    let mut unread = Vec::new();
    let mut shown = Vec::new();
    for (path, thumb) in paths.iter().zip(small) {
        match thumb {
            Some((copy, w, h)) => {
                entries.push(SheetEntry {
                    n: first.saturating_add(shown.len()).saturating_add(1),
                    name: name(path),
                    path: path.display().to_string(),
                    width_px: w,
                    height_px: h,
                });
                shown.push(copy);
            }
            None => unread.push(name(path)),
        }
    }
    let (columns, rows) = grid(shown.len());
    let pitch = cell.saturating_add(GAP);
    let (sw, sh) = (
        columns.saturating_mul(pitch).saturating_add(GAP),
        rows.saturating_mul(pitch).saturating_add(GAP),
    );
    let mut sheet = image::RgbaImage::from_pixel(sw, sh, image::Rgba([235, 235, 235, 255]));
    let scale = (cell / 64).max(2);
    for (i, copy) in shown.iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        let (c, r) = (
            i.checked_rem(columns).unwrap_or(0),
            i.checked_div(columns).unwrap_or(0),
        );
        let x0 = GAP.saturating_add(c.saturating_mul(pitch));
        let y0 = GAP.saturating_add(r.saturating_mul(pitch));
        let x = x0.saturating_add(cell.saturating_sub(copy.width()) / 2);
        let y = y0.saturating_add(cell.saturating_sub(copy.height()) / 2);
        image::imageops::overlay(&mut sheet, copy, i64::from(x), i64::from(y));
        let n = first.saturating_add(i as usize).saturating_add(1);
        label(&mut sheet, n, x0, y0, scale);
    }
    let mut png = Vec::new();
    sheet
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| CmdError::internal(format!("PNG: {e}")))?;
    use base64::Engine as _;
    Ok((
        entries,
        unread,
        base64::engine::general_purpose::STANDARD.encode(&png),
    ))
}

impl Command for ContactSheet {
    type Params = ContactSheetParams;
    type Output = Sheet;

    fn meta(&self) -> &'static CommandMeta {
        &CONTACT_SHEET
    }

    fn plan(&self, _: &mut Session<'_>, p: ContactSheetParams) -> Result<Plan<Sheet>, CmdError> {
        let (paths, first, more) = match &p.path {
            Some(dir) => {
                let all = folder_photos(Path::new(dir))?;
                let start = p.start.min(all.len());
                let end = start.saturating_add(MAX_ON_SHEET).min(all.len());
                let more = all.len().saturating_sub(end);
                let shown = all
                    .get(start..end)
                    .map(<[PathBuf]>::to_vec)
                    .unwrap_or_default();
                (shown, start, more)
            }
            None => (
                p.files.iter().map(|f| PathBuf::from(&f.path)).collect(),
                0,
                0,
            ),
        };
        if paths.is_empty() {
            return Err(
                CmdError::new(ErrorCode::NotFound, "no photos there").vi("Không có ảnh nào")
            );
        }
        let cell = p.cell_px.clamp(96, 512);
        Ok(Plan::Job(Job {
            doc: None,
            label_vi: "Đang xem nhanh ảnh".into(),
            work: Box::new(move |cx| {
                cx.progress(0.1, "Đang đọc ảnh…");
                let (photos, unread, png_base64) = make(&paths, first, cell)?;
                Ok(Box::new(move |_: &mut Session<'_>| {
                    Ok(Sheet {
                        photos,
                        unread,
                        more,
                        png_base64,
                    })
                }) as Apply<Sheet>)
            }),
        }))
    }
}
