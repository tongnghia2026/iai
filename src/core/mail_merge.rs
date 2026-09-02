//! Mail merge: fill a flowing-text template with rows from a CSV/Excel file and
//! produce one merged [`TextDocument`] per row (phase 6 of the word-processor
//! plan). This module is deliberately **pure data**: it reads a data source into
//! a [`MergeTable`], substitutes `{{field}}` placeholders in a template
//! [`TextDocument`] while preserving per-run character styling, and builds safe
//! output file names. It knows nothing about egui, the job queue or PDF export;
//! the app wires those around it. Keeping it engine-agnostic lets the risky
//! parsing/substitution logic be unit-tested in isolation.
//!
//! Placeholder syntax is `{{Field name}}` — the name is matched against the data
//! headers case-insensitively and after trimming outer spaces, so a header
//! `Họ tên` matches `{{Họ tên}}` or `{{ ho ten }}`... (only case/space differ;
//! the letters must match). A known field with an empty cell becomes empty text;
//! an *unknown* field is left as the literal `{{...}}` so a typo is visible in
//! the output rather than silently blanked.

#![allow(dead_code)]

use crate::core::text_document::{Paragraph, Run, TextDocument};
use std::collections::HashMap;
use std::path::Path;

/// A parsed data source: a header row plus zero or more data rows. Cells are
/// already stringified (Excel numbers/dates formatted for Vietnamese output).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergeTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// One data row resolved to a header-keyed lookup, ready for substitution.
pub struct MergeRow {
    /// Normalised header -> cell value. See [`normalize_key`].
    map: HashMap<String, String>,
}

impl MergeRow {
    /// Look a placeholder name up against this row (trim + case-insensitive).
    /// `Some("")` means the field exists but is blank; `None` means no such
    /// column.
    pub fn get(&self, field: &str) -> Option<&str> {
        self.map.get(&normalize_key(field)).map(|s| s.as_str())
    }
}

impl MergeTable {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// True when `field` (trimmed, case-insensitive) is one of the headers.
    pub fn has_field(&self, field: &str) -> bool {
        let key = normalize_key(field);
        self.headers.iter().any(|h| normalize_key(h) == key)
    }

    /// Resolve one data row into a header-keyed lookup. Rows shorter than the
    /// header stretch read the missing trailing cells as empty.
    pub fn row(&self, index: usize) -> Option<MergeRow> {
        let cells = self.rows.get(index)?;
        let mut map = HashMap::with_capacity(self.headers.len());
        for (i, header) in self.headers.iter().enumerate() {
            let value = cells.get(i).cloned().unwrap_or_default();
            map.insert(normalize_key(header), value);
        }
        Some(MergeRow { map })
    }
}

/// Fold a placeholder or header name to its match key: outer whitespace trimmed
/// and lower-cased. Vietnamese diacritics are preserved (only case is folded).
pub fn normalize_key(s: &str) -> String {
    s.trim().to_lowercase()
}

// ---------------------------------------------------------------------------
// Reading data sources
// ---------------------------------------------------------------------------

/// Read a CSV or spreadsheet file into a [`MergeTable`], dispatching on the
/// file extension. `.csv`/`.tsv`/`.txt` go through the CSV reader; everything
/// else (`.xlsx`, `.xls`, `.xlsm`, `.ods`, ...) through calamine.
pub fn read_data_file(path: &Path) -> Result<MergeTable, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "csv" | "tsv" | "txt" => {
            let bytes = std::fs::read(path).map_err(|e| format!("Không đọc được tệp: {e}"))?;
            read_csv_bytes(&bytes)
        }
        _ => read_spreadsheet(path),
    }
}

/// Parse CSV bytes: strip a UTF-8 BOM, sniff the delimiter (`,`, `;` or tab)
/// from the first line, and read header + rows.
pub fn read_csv_bytes(bytes: &[u8]) -> Result<MergeTable, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = String::from_utf8_lossy(bytes);
    let delimiter = sniff_delimiter(&text);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(text.as_bytes());
    let rows = reader
        .records()
        .map(|record| {
            record
                .map(|r| r.iter().map(|c| c.to_string()).collect::<Vec<_>>())
                .map_err(|e| format!("Lỗi đọc CSV: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    table_from_rows(rows.into_iter())
}

/// Pick the delimiter with the most occurrences on the first non-empty line.
fn sniff_delimiter(text: &str) -> u8 {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let comma = first.matches(',').count();
    let semi = first.matches(';').count();
    let tab = first.matches('\t').count();
    if semi > comma && semi >= tab {
        b';'
    } else if tab > comma && tab > semi {
        b'\t'
    } else {
        b','
    }
}

/// Read the first worksheet of an Excel/ODS workbook into a [`MergeTable`].
fn read_spreadsheet(path: &Path) -> Result<MergeTable, String> {
    use calamine::{open_workbook_auto, Reader};
    let mut workbook =
        open_workbook_auto(path).map_err(|e| format!("Không mở được tệp bảng tính: {e}"))?;
    let range = workbook
        .worksheet_range_at(0)
        .ok_or_else(|| "Tệp không có trang tính nào".to_string())?
        .map_err(|e| format!("Không đọc được trang tính: {e}"))?;
    let rows = range
        .rows()
        .map(|row| row.iter().map(cell_to_string).collect::<Vec<_>>());
    table_from_rows(rows)
}

/// Stringify one spreadsheet cell for merge output. Numbers drop a redundant
/// `.0`; dates format as Vietnamese `dd/mm/yyyy` (with `HH:MM` only when a time
/// is present) so contract fields read naturally.
fn cell_to_string(cell: &calamine::Data) -> String {
    use calamine::Data;
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => format_number(*f),
        Data::DateTime(dt) => format_excel_datetime(dt),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("{e:?}"),
    }
}

fn format_number(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn format_excel_datetime(dt: &calamine::ExcelDateTime) -> String {
    match dt.as_datetime() {
        Some(ndt) if ndt.time() == chrono::NaiveTime::MIN => ndt.format("%d/%m/%Y").to_string(),
        Some(ndt) => ndt.format("%d/%m/%Y %H:%M").to_string(),
        None => format_number(dt.as_f64()),
    }
}

/// Build a table from raw string rows: the first row is the header (trailing
/// empty columns trimmed), the rest are data rows (fully blank rows skipped).
fn table_from_rows(mut rows: impl Iterator<Item = Vec<String>>) -> Result<MergeTable, String> {
    let mut headers = loop {
        match rows.next() {
            Some(row) if row.iter().any(|c| !c.trim().is_empty()) => break row,
            Some(_) => continue,
            None => return Err("Tệp dữ liệu trống — không có tiêu đề cột".to_string()),
        }
    };
    while headers.last().is_some_and(|c| c.trim().is_empty()) {
        headers.pop();
    }
    let width = headers.len();
    if width == 0 {
        return Err("Không tìm thấy tiêu đề cột nào".to_string());
    }
    let mut data = Vec::new();
    for mut row in rows {
        if row.iter().take(width).all(|c| c.trim().is_empty()) {
            continue;
        }
        row.truncate(width);
        while row.len() < width {
            row.push(String::new());
        }
        data.push(row);
    }
    Ok(MergeTable {
        headers,
        rows: data,
    })
}

// ---------------------------------------------------------------------------
// Placeholder scanning
// ---------------------------------------------------------------------------

/// One `{{field}}` occurrence in a string, as char indices into the source.
struct Placeholder {
    start: usize,
    end: usize,
    name: String,
}

/// Scan a `char` slice for `{{field}}` placeholders. A placeholder ends at the
/// first `}}`; a stray `{` or newline inside cancels it (so unbalanced braces in
/// prose are left alone).
fn scan_placeholders(chars: &[char]) -> Vec<Placeholder> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '{' && chars[i + 1] == '{' {
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < chars.len() {
                if chars[j] == '}' && chars[j + 1] == '}' {
                    close = Some(j);
                    break;
                }
                if chars[j] == '{' || chars[j] == '\n' {
                    break;
                }
                j += 1;
            }
            if let Some(c) = close {
                let name: String = chars[i + 2..c].iter().collect();
                if !name.trim().is_empty() {
                    out.push(Placeholder {
                        start: i,
                        end: c + 2,
                        name: name.trim().to_string(),
                    });
                    i = c + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Distinct merge field names used by `doc`, in first-appearance order.
pub fn find_fields(doc: &TextDocument) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for para in &doc.paragraphs {
        if para.is_image() {
            continue;
        }
        let chars: Vec<char> = para.text().chars().collect();
        for ph in scan_placeholders(&chars) {
            if !seen
                .iter()
                .any(|s| normalize_key(s) == normalize_key(&ph.name))
            {
                seen.push(ph.name);
            }
        }
    }
    seen
}

/// How a template's fields line up with a data table: which are present as a
/// column and which are missing. Used by the pre-export dialog.
pub struct MergeAnalysis {
    pub matched: Vec<String>,
    pub missing: Vec<String>,
}

pub fn analyze(doc: &TextDocument, table: &MergeTable) -> MergeAnalysis {
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for field in find_fields(doc) {
        if table.has_field(&field) {
            matched.push(field);
        } else {
            missing.push(field);
        }
    }
    MergeAnalysis { matched, missing }
}

// ---------------------------------------------------------------------------
// Substitution
// ---------------------------------------------------------------------------

/// Produce a merged copy of `template` with every `{{field}}` replaced by the
/// row's value. Per-run character styling is preserved: a replacement inherits
/// the style of the run where its `{{` begins. Unknown fields are left as the
/// literal placeholder.
pub fn merge_document(template: &TextDocument, row: &MergeRow) -> TextDocument {
    let mut doc = template.clone();
    for para in &mut doc.paragraphs {
        if para.is_image() {
            continue;
        }
        substitute_paragraph(para, &|field| row.get(field).map(|v| v.to_string()));
    }
    doc.normalize();
    doc
}

/// Rewrite one text paragraph's runs, substituting placeholders while keeping
/// per-character style. `lookup` returns `None` for unknown fields (left raw).
fn substitute_paragraph(para: &mut Paragraph, lookup: &dyn Fn(&str) -> Option<String>) {
    // Flatten to per-char (glyph, style) so the replacement can inherit the
    // style at the placeholder's opening brace even across run boundaries.
    let mut chars: Vec<(char, usize)> = Vec::new();
    let mut styles = Vec::with_capacity(para.runs.len());
    for (run_idx, run) in para.runs.iter().enumerate() {
        styles.push(run.style.clone());
        for ch in run.text.chars() {
            chars.push((ch, run_idx));
        }
    }
    let glyphs: Vec<char> = chars.iter().map(|(c, _)| *c).collect();
    let placeholders = scan_placeholders(&glyphs);
    if placeholders.is_empty() {
        return;
    }

    let mut out: Vec<(char, usize)> = Vec::with_capacity(chars.len());
    let mut cursor = 0;
    for ph in &placeholders {
        out.extend_from_slice(&chars[cursor..ph.start]);
        let style_run = chars[ph.start].1;
        match lookup(&ph.name) {
            Some(value) => {
                for ch in value.chars() {
                    // Collapse hard line breaks in a cell into spaces so a
                    // multi-line address does not break the paragraph model.
                    let ch = if ch == '\n' || ch == '\r' { ' ' } else { ch };
                    out.push((ch, style_run));
                }
            }
            None => out.extend_from_slice(&chars[ph.start..ph.end]),
        }
        cursor = ph.end;
    }
    out.extend_from_slice(&chars[cursor..]);

    // Coalesce adjacent characters that share a run's style back into runs.
    let mut runs: Vec<Run> = Vec::new();
    for (ch, run_idx) in out {
        match runs.last_mut() {
            Some(last) if last.style == styles[run_idx] => last.text.push(ch),
            _ => runs.push(Run::new(ch.to_string(), styles[run_idx].clone())),
        }
    }
    para.runs = runs;
}

// ---------------------------------------------------------------------------
// Output file names
// ---------------------------------------------------------------------------

/// Expand a file-name pattern (same `{{field}}` syntax; unknown fields become
/// empty) and sanitise it for the filesystem. Returns an empty string when the
/// pattern resolves to nothing usable — the caller supplies a fallback.
pub fn expand_filename(pattern: &str, row: &MergeRow) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let placeholders = scan_placeholders(&chars);
    let mut out = String::new();
    let mut cursor = 0;
    for ph in &placeholders {
        out.extend(&chars[cursor..ph.start]);
        if let Some(value) = row.get(&ph.name) {
            out.push_str(value);
        }
        cursor = ph.end;
    }
    out.extend(&chars[cursor..]);
    sanitize_filename(&out)
}

/// Make `name` safe as a single path component on Windows and Unix: replace
/// reserved characters, drop control chars, trim surrounding dots/spaces and
/// cap the length.
pub fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => ' ',
            c => c,
        })
        .collect();
    s = s.trim().trim_matches('.').trim().to_string();
    // Collapse runs of whitespace introduced by the mapping above.
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 120 {
        collapsed.chars().take(120).collect()
    } else {
        collapsed
    }
}

/// Make `base` unique against names already produced in this batch by appending
/// ` (2)`, ` (3)`, ... The comparison is case-insensitive to match Windows.
pub fn unique_stem(base: &str, used: &mut std::collections::HashSet<String>) -> String {
    let base = if base.is_empty() { "tai-lieu" } else { base };
    let mut candidate = base.to_string();
    let mut n = 2;
    while !used.insert(candidate.to_lowercase()) {
        candidate = format!("{base} ({n})");
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::text_document::{CharStyle, ParagraphStyle};

    fn table() -> MergeTable {
        MergeTable {
            headers: vec!["Họ tên".into(), "Số tiền".into(), "Ngày".into()],
            rows: vec![
                vec!["Nguyễn Văn A".into(), "1500000".into(), "01/09/2026".into()],
                vec!["Trần Thị B".into(), "".into(), "".into()],
            ],
        }
    }

    #[test]
    fn csv_parses_headers_and_rows() {
        let csv = "Name,Amount\nAlice,100\nBob,200\n";
        let t = read_csv_bytes(csv.as_bytes()).unwrap();
        assert_eq!(t.headers, vec!["Name", "Amount"]);
        assert_eq!(t.row_count(), 2);
        assert_eq!(t.rows[1], vec!["Bob", "200"]);
    }

    #[test]
    fn csv_strips_bom_and_sniffs_semicolon() {
        let csv = "\u{FEFF}Ten;Tien\nA;1\nB;2\n";
        let t = read_csv_bytes(csv.as_bytes()).unwrap();
        assert_eq!(t.headers, vec!["Ten", "Tien"]);
        assert_eq!(t.rows[0], vec!["A", "1"]);
    }

    #[test]
    fn table_skips_blank_rows_and_pads_short_rows() {
        let rows = vec![
            vec!["A".to_string(), "B".to_string()],
            vec!["".to_string(), "".to_string()],
            vec!["x".to_string()],
        ];
        let t = table_from_rows(rows.into_iter()).unwrap();
        assert_eq!(t.row_count(), 1);
        assert_eq!(t.rows[0], vec!["x", ""]);
    }

    #[test]
    fn table_trims_trailing_empty_headers() {
        let rows = vec![
            vec!["A".into(), "B".into(), "".into(), "".into()],
            vec!["1".into(), "2".into(), "".into(), "".into()],
        ];
        let t = table_from_rows(rows.into_iter()).unwrap();
        assert_eq!(t.headers, vec!["A", "B"]);
        assert_eq!(t.rows[0], vec!["1", "2"]);
    }

    #[test]
    fn empty_source_is_an_error() {
        assert!(table_from_rows(std::iter::empty()).is_err());
    }

    #[test]
    fn find_fields_is_ordered_and_deduplicated() {
        let doc = TextDocument::from_plain_text(
            "Kính gửi {{Họ tên}},\nSố tiền {{Số tiền}} — {{Họ tên}} ký ngày {{Ngày}}.",
        );
        assert_eq!(find_fields(&doc), vec!["Họ tên", "Số tiền", "Ngày"]);
    }

    #[test]
    fn row_lookup_is_trim_and_case_insensitive() {
        let t = table();
        let row = t.row(0).unwrap();
        // Trim + case fold only (diacritics must still match exactly).
        assert_eq!(row.get("  HỌ TÊN  "), Some("Nguyễn Văn A"));
        assert_eq!(row.get("SỐ TIỀN"), Some("1500000"));
        assert_eq!(row.get("ho ten"), None);
        assert_eq!(row.get("khong-co"), None);
    }

    #[test]
    fn merge_replaces_known_fields_and_keeps_unknown_literal() {
        let doc = TextDocument::from_plain_text("{{Họ tên}} nợ {{Số tiền}}đ, mã {{Ma}}.");
        let merged = merge_document(&doc, &table().row(0).unwrap());
        assert_eq!(merged.plain_text(), "Nguyễn Văn A nợ 1500000đ, mã {{Ma}}.");
    }

    #[test]
    fn merge_blank_cell_becomes_empty() {
        let doc = TextDocument::from_plain_text("{{Họ tên}}: {{Số tiền}}");
        let merged = merge_document(&doc, &table().row(1).unwrap());
        assert_eq!(merged.plain_text(), "Trần Thị B: ");
    }

    #[test]
    fn merge_preserves_run_style_of_placeholder_start() {
        // Two runs: a bold "{{Họ tên}}" then a plain " ký.".
        let bold = CharStyle {
            bold: true,
            ..CharStyle::default()
        };
        let para = Paragraph {
            runs: vec![
                Run::new("{{Họ tên}}", bold.clone()),
                Run::new(" ký.", CharStyle::default()),
            ],
            style: ParagraphStyle::default(),
            image: None,
        };
        let doc = TextDocument {
            paragraphs: vec![para],
            ..TextDocument::default()
        };
        let merged = merge_document(&doc, &table().row(0).unwrap());
        let runs = &merged.paragraphs[0].runs;
        // The substituted name keeps the bold style; the trailing text stays plain.
        assert_eq!(runs[0].text, "Nguyễn Văn A");
        assert!(runs[0].style.bold);
        assert_eq!(runs[1].text, " ký.");
        assert!(!runs[1].style.bold);
    }

    #[test]
    fn merge_across_run_boundary_uses_opening_run_style() {
        // Placeholder split across runs: "{{Ho" (bold) + "_ten}}" (plain).
        let bold = CharStyle {
            bold: true,
            ..CharStyle::default()
        };
        let para = Paragraph {
            runs: vec![
                Run::new("{{Họ", bold),
                Run::new(" tên}}!", CharStyle::default()),
            ],
            style: ParagraphStyle::default(),
            image: None,
        };
        let doc = TextDocument {
            paragraphs: vec![para],
            ..TextDocument::default()
        };
        let merged = merge_document(&doc, &table().row(0).unwrap());
        assert_eq!(merged.plain_text(), "Nguyễn Văn A!");
        assert!(merged.paragraphs[0].runs[0].style.bold);
    }

    #[test]
    fn analyze_splits_matched_and_missing() {
        let doc = TextDocument::from_plain_text("{{Họ tên}} {{Địa chỉ}}");
        let a = analyze(&doc, &table());
        assert_eq!(a.matched, vec!["Họ tên"]);
        assert_eq!(a.missing, vec!["Địa chỉ"]);
    }

    #[test]
    fn filename_pattern_expands_and_sanitises() {
        let t = table();
        assert_eq!(
            expand_filename("HD {{Họ tên}} {{Ngày}}", &t.row(0).unwrap()),
            "HD Nguyễn Văn A 01_09_2026"
        );
        // Unknown field -> empty.
        assert_eq!(expand_filename("{{Ma}}-x", &t.row(0).unwrap()), "-x");
    }

    #[test]
    fn sanitize_strips_reserved_chars() {
        assert_eq!(sanitize_filename("a/b:c*?d"), "a_b_c__d");
        assert_eq!(sanitize_filename("  ..name..  "), "name");
    }

    #[test]
    fn unique_stem_disambiguates_case_insensitively() {
        let mut used = std::collections::HashSet::new();
        assert_eq!(unique_stem("An", &mut used), "An");
        assert_eq!(unique_stem("an", &mut used), "an (2)");
        assert_eq!(unique_stem("AN", &mut used), "AN (3)");
        assert_eq!(unique_stem("", &mut used), "tai-lieu");
    }

    #[test]
    fn merge_to_pdf_writes_one_valid_pdf_per_row() {
        // Exercises the batch worker's pipeline (minus the native dialogs):
        // fixture -> merge each row -> layout -> selectable-text PDF on disk.
        use crate::core::text_layout::DocumentLayout;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/core/testdata/mail_merge_sample.xlsx");
        let table = read_data_file(&path).unwrap();
        let template = TextDocument::from_plain_text(
            "HỢP ĐỒNG\nKhách hàng: {{Họ tên}}\nSố tiền: {{Số tiền}} đồng\nNgày: {{Ngày ký}}",
        );
        let mut fs = cosmic_text::FontSystem::new();
        let dir = std::env::temp_dir().join(format!("iai_mm_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut used = std::collections::HashSet::new();
        for i in 0..table.row_count() {
            let row = table.row(i).unwrap();
            let merged = merge_document(&template, &row);
            let stem = unique_stem(&expand_filename("{{Họ tên}}", &row), &mut used);
            let out = dir.join(format!("{stem}.pdf"));
            let layout = DocumentLayout::build(&merged, 96.0, &mut fs);
            layout.write_text_pdf(&mut fs, &out).unwrap();
            let bytes = std::fs::read(&out).unwrap();
            assert!(bytes.starts_with(b"%PDF"), "output is not a PDF");
            assert!(bytes.len() > 1000, "PDF suspiciously small");
        }
        let count = std::fs::read_dir(&dir).unwrap().count();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(count, 2, "expected one PDF per data row");
    }

    #[test]
    fn reads_xlsx_fixture_with_date_and_number() {
        // Verifies the calamine path end to end: headers, an integer amount
        // (no trailing .0) and an Excel serial date formatted dd/mm/yyyy.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/core/testdata/mail_merge_sample.xlsx");
        let t = read_data_file(&path).unwrap();
        assert_eq!(t.headers, vec!["Họ tên", "Số tiền", "Ngày ký"]);
        assert_eq!(t.row_count(), 2);
        let row0 = t.row(0).unwrap();
        assert_eq!(row0.get("Họ tên"), Some("Nguyễn Văn A"));
        assert_eq!(row0.get("Số tiền"), Some("1500000"));
        assert_eq!(row0.get("Ngày ký"), Some("01/09/2026"));
        assert_eq!(t.row(1).unwrap().get("Ngày ký"), Some("25/12/2026"));
    }

    #[test]
    fn newline_in_cell_collapses_to_space() {
        let t = MergeTable {
            headers: vec!["Địa chỉ".into()],
            rows: vec![vec!["12 Lê Lợi\nQuận 1".into()]],
        };
        let doc = TextDocument::from_plain_text("Tại {{Địa chỉ}}.");
        let merged = merge_document(&doc, &t.row(0).unwrap());
        assert_eq!(merged.plain_text(), "Tại 12 Lê Lợi Quận 1.");
    }
}
