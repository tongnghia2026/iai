//! What an ONNX file takes in and gives out, read from the file without
//! loading the model: just enough protobuf to walk `ModelProto` and its
//! `GraphProto`, seeking past the weights (hundreds of megabytes in the file,
//! or a 1.6 GB file of their own beside it) instead of reading them.
//!
//! The fields read (onnx.proto3): `ModelProto` ir_version 1, producer_name 2,
//! graph 7, opset_import 8, metadata_props 14; `GraphProto` initializer 5,
//! input 11, output 12; `TensorProto` name 8, external_data 13; `ValueInfoProto`
//! name 1, type 2; `TypeProto` tensor_type 1; `TypeProto.Tensor` elem_type 1,
//! shape 2; `TensorShapeProto` dim 1; `Dimension` dim_value 1, dim_param 2.

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

/// `TensorProto.DataType` FLOAT.
pub const FLOAT: i32 = 1;
/// No name or key read is longer than this; a longer one is cut.
const MAX_TEXT: u64 = 4096;
/// No list read is longer than this: a file past it is not a model.
const MAX_ITEMS: usize = 100_000;

/// One side of a tensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dim {
    Fixed(i64),
    /// A size chosen when the model is run ("height", "batch").
    Named(String),
    Unknown,
}

impl Dim {
    /// `size` fits this side: it is that size, or any.
    pub fn fits(&self, size: i64) -> bool {
        match self {
            Dim::Fixed(n) => *n == size,
            Dim::Named(_) | Dim::Unknown => true,
        }
    }

    pub fn is_fixed(&self) -> bool {
        matches!(self, Dim::Fixed(_))
    }
}

/// An input or an output of the graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tensor {
    pub name: String,
    /// `TensorProto.DataType` of its values (0 when it is not a tensor).
    pub elem_type: i32,
    /// `None` when the file gives no shape.
    pub dims: Option<Vec<Dim>>,
}

impl Tensor {
    pub fn rank(&self) -> Option<usize> {
        self.dims.as_ref().map(Vec::len)
    }

    /// Side `i`, `Unknown` past the shape.
    pub fn dim(&self, i: usize) -> Dim {
        self.dims
            .as_ref()
            .and_then(|d| d.get(i).cloned())
            .unwrap_or(Dim::Unknown)
    }

    /// How many values, when every side is fixed.
    pub fn elements(&self) -> Option<i64> {
        self.dims.as_ref()?.iter().try_fold(1i64, |n, d| match d {
            Dim::Fixed(v) => n.checked_mul(*v),
            _ => None,
        })
    }

    /// For people: `name [1, 3, height, width]`.
    pub fn describe(&self) -> String {
        let dims = match &self.dims {
            None => "?".to_string(),
            Some(dims) => dims
                .iter()
                .map(|d| match d {
                    Dim::Fixed(n) => n.to_string(),
                    Dim::Named(s) => s.clone(),
                    Dim::Unknown => "?".to_string(),
                })
                .collect::<Vec<_>>()
                .join(", "),
        };
        format!("{} [{dims}]", self.name)
    }
}

/// What the file says of itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub ir_version: i64,
    pub producer: String,
    /// (domain, version); "" is the default domain.
    pub opsets: Vec<(String, i64)>,
    /// The graph's inputs, the weights left out (an old file lists them as
    /// inputs too).
    pub inputs: Vec<Tensor>,
    pub outputs: Vec<Tensor>,
    /// Keys of the model's metadata (PP-OCR keeps its characters there).
    pub metadata_keys: Vec<String>,
    /// Files beside the model its weights are in, as the model names them.
    pub external_files: Vec<String>,
}

impl Header {
    pub fn input(&self, name: &str) -> Option<&Tensor> {
        self.inputs.iter().find(|t| t.name == name)
    }

    pub fn output(&self, name: &str) -> Option<&Tensor> {
        self.outputs.iter().find(|t| t.name == name)
    }

    pub fn has_metadata(&self, key: &str) -> bool {
        self.metadata_keys.iter().any(|k| k == key)
    }

    /// For people: what goes in and what comes out.
    pub fn describe(&self) -> String {
        let list = |tensors: &[Tensor]| {
            tensors
                .iter()
                .take(4)
                .map(Tensor::describe)
                .collect::<Vec<_>>()
                .join("; ")
                + if tensors.len() > 4 { "; …" } else { "" }
        };
        format!("vào: {} — ra: {}", list(&self.inputs), list(&self.outputs))
    }
}

/// Read the header of the ONNX file at `path`. An error says, for people,
/// why it is not one.
pub fn read_header(path: &Path) -> Result<Header, String> {
    let file = File::open(path).map_err(|e| format!("không mở được file: {e}"))?;
    let len = file
        .metadata()
        .map_err(|e| format!("không đọc được file: {e}"))?
        .len();
    let mut reader = Reader {
        inner: BufReader::with_capacity(64 * 1024, file),
        pos: 0,
    };
    parse_model(&mut reader, len).map_err(|e| format!("không phải file ONNX đọc được ({e})"))
}

/// Read the header of an ONNX file held in memory.
pub fn parse_header(bytes: &[u8]) -> Result<Header, String> {
    let mut reader = Reader {
        inner: std::io::Cursor::new(bytes),
        pos: 0,
    };
    parse_model(&mut reader, bytes.len() as u64)
        .map_err(|e| format!("không phải file ONNX đọc được ({e})"))
}

/// A protobuf stream, with where it is.
struct Reader<R> {
    inner: R,
    pos: u64,
}

type Res<T> = Result<T, &'static str>;

impl<R: Read + Seek> Reader<R> {
    fn byte(&mut self) -> Res<u8> {
        let mut b = [0u8; 1];
        self.inner.read_exact(&mut b).map_err(|_| "hết file")?;
        self.pos += 1;
        Ok(b[0])
    }

    fn varint(&mut self) -> Res<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("số hỏng")
    }

    fn skip(&mut self, n: u64, end: u64) -> Res<()> {
        let to = self.pos.checked_add(n).ok_or("độ dài hỏng")?;
        if to > end {
            return Err("độ dài vượt quá file");
        }
        let n = i64::try_from(n).map_err(|_| "độ dài hỏng")?;
        self.inner.seek_relative(n).map_err(|_| "hết file")?;
        self.pos = to;
        Ok(())
    }

    /// The bytes of a length-delimited field, no more than `MAX_TEXT` kept.
    fn text(&mut self, len: u64, end: u64) -> Res<String> {
        if self.pos.saturating_add(len) > end {
            return Err("độ dài vượt quá file");
        }
        let kept = len.min(MAX_TEXT);
        let mut buf = vec![0u8; kept as usize];
        self.inner.read_exact(&mut buf).map_err(|_| "hết file")?;
        self.pos += kept;
        self.skip(len - kept, end)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// The next field's number and wire type, `None` at `end`.
    fn tag(&mut self, end: u64) -> Res<Option<(u64, u8)>> {
        if self.pos >= end {
            return Ok(None);
        }
        let tag = self.varint()?;
        let field = tag >> 3;
        if field == 0 {
            return Err("trường số 0");
        }
        Ok(Some((field, (tag & 7) as u8)))
    }

    /// Past the value of a field of wire type `wire`.
    fn skip_value(&mut self, wire: u8, end: u64) -> Res<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.skip(8, end),
            2 => {
                let len = self.varint()?;
                self.skip(len, end)
            }
            5 => self.skip(4, end),
            _ => Err("kiểu trường lạ"),
        }
    }

    /// The end of a length-delimited field that starts here.
    fn sub(&mut self, wire: u8, end: u64) -> Res<u64> {
        if wire != 2 {
            return Err("kiểu trường sai");
        }
        let len = self.varint()?;
        let sub_end = self.pos.checked_add(len).ok_or("độ dài hỏng")?;
        if sub_end > end {
            return Err("độ dài vượt quá file");
        }
        Ok(sub_end)
    }

    fn string(&mut self, wire: u8, end: u64) -> Res<String> {
        let sub_end = self.sub(wire, end)?;
        let len = sub_end - self.pos;
        self.text(len, sub_end)
    }

    fn int(&mut self, wire: u8) -> Res<i64> {
        if wire != 0 {
            return Err("kiểu trường sai");
        }
        Ok(self.varint()? as i64)
    }
}

fn parse_model<R: Read + Seek>(r: &mut Reader<R>, end: u64) -> Res<Header> {
    let mut header = Header::default();
    let mut graph = None;
    let mut initializers = std::collections::HashSet::new();
    while let Some((field, wire)) = r.tag(end)? {
        match field {
            1 => header.ir_version = r.int(wire)?,
            2 => header.producer = r.string(wire, end)?,
            7 => {
                let sub_end = r.sub(wire, end)?;
                graph = Some(parse_graph(r, sub_end, &mut header, &mut initializers)?);
            }
            8 => {
                let sub_end = r.sub(wire, end)?;
                let (mut domain, mut version) = (String::new(), 0);
                while let Some((f, w)) = r.tag(sub_end)? {
                    match f {
                        1 => domain = r.string(w, sub_end)?,
                        2 => version = r.int(w)?,
                        _ => r.skip_value(w, sub_end)?,
                    }
                }
                push(&mut header.opsets, (domain, version))?;
            }
            14 => {
                let sub_end = r.sub(wire, end)?;
                while let Some((f, w)) = r.tag(sub_end)? {
                    match f {
                        1 => {
                            let key = r.string(w, sub_end)?;
                            push(&mut header.metadata_keys, key)?;
                        }
                        _ => r.skip_value(w, sub_end)?,
                    }
                }
            }
            _ => r.skip_value(wire, end)?,
        }
    }
    let Some(inputs) = graph else {
        return Err("không có đồ thị");
    };
    header.inputs = inputs
        .into_iter()
        .filter(|t: &Tensor| !initializers.contains(&t.name))
        .collect();
    if header.inputs.is_empty() || header.outputs.is_empty() {
        return Err("đồ thị không có đầu vào hoặc đầu ra");
    }
    Ok(header)
}

fn push<T>(list: &mut Vec<T>, item: T) -> Res<()> {
    if list.len() >= MAX_ITEMS {
        return Err("danh sách quá dài");
    }
    list.push(item);
    Ok(())
}

/// The graph's inputs (weights not yet left out); its outputs, the
/// weights' names and their outside files go into `header`.
fn parse_graph<R: Read + Seek>(
    r: &mut Reader<R>,
    end: u64,
    header: &mut Header,
    initializers: &mut std::collections::HashSet<String>,
) -> Res<Vec<Tensor>> {
    let mut inputs = Vec::new();
    while let Some((field, wire)) = r.tag(end)? {
        match field {
            5 => {
                let sub_end = r.sub(wire, end)?;
                let (name, location) = parse_initializer(r, sub_end)?;
                if initializers.len() >= MAX_ITEMS {
                    return Err("danh sách quá dài");
                }
                initializers.insert(name);
                if let Some(location) = location
                    && !header.external_files.contains(&location)
                {
                    push(&mut header.external_files, location)?;
                }
            }
            11 => {
                let sub_end = r.sub(wire, end)?;
                push(&mut inputs, parse_value_info(r, sub_end)?)?;
            }
            12 => {
                let sub_end = r.sub(wire, end)?;
                let output = parse_value_info(r, sub_end)?;
                push(&mut header.outputs, output)?;
            }
            _ => r.skip_value(wire, end)?,
        }
    }
    Ok(inputs)
}

/// A weight's name, and the file beside the model its values are in.
fn parse_initializer<R: Read + Seek>(r: &mut Reader<R>, end: u64) -> Res<(String, Option<String>)> {
    let (mut name, mut location) = (String::new(), None);
    while let Some((field, wire)) = r.tag(end)? {
        match field {
            8 => name = r.string(wire, end)?,
            13 => {
                let sub_end = r.sub(wire, end)?;
                let (mut key, mut value) = (String::new(), String::new());
                while let Some((f, w)) = r.tag(sub_end)? {
                    match f {
                        1 => key = r.string(w, sub_end)?,
                        2 => value = r.string(w, sub_end)?,
                        _ => r.skip_value(w, sub_end)?,
                    }
                }
                if key == "location" && !value.is_empty() {
                    location = Some(value);
                }
            }
            _ => r.skip_value(wire, end)?,
        }
    }
    Ok((name, location))
}

fn parse_value_info<R: Read + Seek>(r: &mut Reader<R>, end: u64) -> Res<Tensor> {
    let mut tensor = Tensor::default();
    while let Some((field, wire)) = r.tag(end)? {
        match field {
            1 => tensor.name = r.string(wire, end)?,
            2 => {
                let type_end = r.sub(wire, end)?;
                while let Some((f, w)) = r.tag(type_end)? {
                    match f {
                        1 => {
                            let tensor_end = r.sub(w, type_end)?;
                            parse_tensor_type(r, tensor_end, &mut tensor)?;
                        }
                        _ => r.skip_value(w, type_end)?,
                    }
                }
            }
            _ => r.skip_value(wire, end)?,
        }
    }
    Ok(tensor)
}

fn parse_tensor_type<R: Read + Seek>(r: &mut Reader<R>, end: u64, tensor: &mut Tensor) -> Res<()> {
    while let Some((field, wire)) = r.tag(end)? {
        match field {
            1 => tensor.elem_type = r.int(wire)? as i32,
            2 => {
                let shape_end = r.sub(wire, end)?;
                let mut dims = Vec::new();
                while let Some((f, w)) = r.tag(shape_end)? {
                    match f {
                        1 => {
                            let dim_end = r.sub(w, shape_end)?;
                            let mut dim = Dim::Unknown;
                            while let Some((df, dw)) = r.tag(dim_end)? {
                                match df {
                                    1 => dim = Dim::Fixed(r.int(dw)?),
                                    2 => dim = Dim::Named(r.string(dw, dim_end)?),
                                    _ => r.skip_value(dw, dim_end)?,
                                }
                            }
                            push(&mut dims, dim)?;
                        }
                        _ => r.skip_value(w, shape_end)?,
                    }
                }
                tensor.dims = Some(dims);
            }
            _ => r.skip_value(wire, end)?,
        }
    }
    Ok(())
}

/// Writing the few messages the reader reads, for the tests of this crate:
/// a real model's header without its weights, or a small fake model.
#[cfg(test)]
pub(crate) mod write {
    pub fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    pub fn field_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
        varint(out, field << 3 | 2);
        varint(out, bytes.len() as u64);
        out.extend_from_slice(bytes);
    }

    pub fn field_int(out: &mut Vec<u8>, field: u64, v: i64) {
        varint(out, field << 3);
        varint(out, v as u64);
    }

    /// `ValueInfoProto` of a float tensor; a negative side is named.
    pub fn value_info(name: &str, dims: &[i64]) -> Vec<u8> {
        let mut shape = Vec::new();
        for &d in dims {
            let mut dim = Vec::new();
            if d < 0 {
                field_bytes(&mut dim, 2, format!("d{}", -d).as_bytes());
            } else {
                field_int(&mut dim, 1, d);
            }
            field_bytes(&mut shape, 1, &dim);
        }
        let mut tensor = Vec::new();
        field_int(&mut tensor, 1, super::FLOAT as i64);
        field_bytes(&mut tensor, 2, &shape);
        let mut ty = Vec::new();
        field_bytes(&mut ty, 1, &tensor);
        let mut info = Vec::new();
        field_bytes(&mut info, 1, name.as_bytes());
        field_bytes(&mut info, 2, &ty);
        info
    }

    /// What a fake model is made of.
    #[derive(Default)]
    pub struct Model<'a> {
        pub inputs: Vec<(&'a str, Vec<i64>)>,
        pub outputs: Vec<(&'a str, Vec<i64>)>,
        pub metadata: Vec<(&'a str, &'a str)>,
        /// Weights: name, raw bytes, and the outside file they are in.
        pub weights: Vec<(&'a str, Vec<u8>, Option<&'a str>)>,
        /// Weights also listed as inputs, as an old file does.
        pub weights_as_inputs: bool,
    }

    impl Model<'_> {
        pub fn bytes(&self) -> Vec<u8> {
            let mut graph = Vec::new();
            // A node first, as exporters write them: skipped over.
            let mut node = Vec::new();
            field_bytes(&mut node, 4, b"Conv");
            field_bytes(&mut graph, 1, &node);
            field_bytes(&mut graph, 2, b"graph");
            for (name, raw, location) in &self.weights {
                let mut init = Vec::new();
                field_int(&mut init, 1, raw.len() as i64);
                field_int(&mut init, 2, 2);
                field_bytes(&mut init, 8, name.as_bytes());
                field_bytes(&mut init, 9, raw);
                if let Some(location) = location {
                    let mut entry = Vec::new();
                    field_bytes(&mut entry, 1, b"location");
                    field_bytes(&mut entry, 2, location.as_bytes());
                    field_bytes(&mut init, 13, &entry);
                    field_int(&mut init, 14, 1);
                }
                field_bytes(&mut graph, 5, &init);
            }
            for (name, dims) in &self.inputs {
                field_bytes(&mut graph, 11, &value_info(name, dims));
            }
            if self.weights_as_inputs {
                for (name, raw, _) in &self.weights {
                    field_bytes(&mut graph, 11, &value_info(name, &[raw.len() as i64]));
                }
            }
            for (name, dims) in &self.outputs {
                field_bytes(&mut graph, 12, &value_info(name, dims));
            }
            let mut model = Vec::new();
            field_int(&mut model, 1, 8);
            field_bytes(&mut model, 2, b"iai-test");
            field_bytes(&mut model, 7, &graph);
            let mut opset = Vec::new();
            field_int(&mut opset, 2, 17);
            field_bytes(&mut model, 8, &opset);
            for (key, value) in &self.metadata {
                let mut entry = Vec::new();
                field_bytes(&mut entry, 1, key.as_bytes());
                field_bytes(&mut entry, 2, value.as_bytes());
                field_bytes(&mut model, 14, &entry);
            }
            model
        }
    }
}

#[cfg(test)]
mod tests {
    use super::write::Model;
    use super::*;

    #[test]
    fn inputs_outputs_metadata_and_outside_files_are_read_past_the_weights() {
        let model = Model {
            inputs: vec![("x", vec![-1, 3, 48, -2])],
            outputs: vec![("softmax", vec![-1, -2, 6625])],
            metadata: vec![("character", "a\nb\nc")],
            weights: vec![
                ("w0", vec![7u8; 300_000], None),
                ("w1", vec![], Some("model.onnx.data")),
                ("w2", vec![], Some("model.onnx.data")),
            ],
            weights_as_inputs: false,
        };
        let header = parse_header(&model.bytes()).unwrap();
        assert_eq!(header.ir_version, 8);
        assert_eq!(header.producer, "iai-test");
        assert_eq!(header.opsets, [(String::new(), 17)]);
        assert_eq!(header.inputs.len(), 1);
        let x = &header.inputs[0];
        assert_eq!(x.name, "x");
        assert_eq!(x.elem_type, FLOAT);
        assert_eq!(
            x.dims,
            Some(vec![
                Dim::Named("d1".into()),
                Dim::Fixed(3),
                Dim::Fixed(48),
                Dim::Named("d2".into())
            ])
        );
        assert_eq!(header.outputs[0].rank(), Some(3));
        assert!(header.has_metadata("character"));
        assert_eq!(header.external_files, ["model.onnx.data"]);
        assert_eq!(x.describe(), "x [d1, 3, 48, d2]");
    }

    #[test]
    fn weights_an_old_file_lists_as_inputs_are_not_inputs() {
        let model = Model {
            inputs: vec![("input", vec![1, 3, 512, 512])],
            outputs: vec![("output", vec![1, 3, 512, 512])],
            weights: vec![("conv.weight", vec![0u8; 64], None)],
            weights_as_inputs: true,
            ..Model::default()
        };
        let header = parse_header(&model.bytes()).unwrap();
        assert_eq!(header.inputs.len(), 1);
        assert_eq!(header.inputs[0].name, "input");
        assert_eq!(header.inputs[0].elements(), Some(3 * 512 * 512));
    }

    #[test]
    fn a_large_model_is_read_without_reading_its_weights() {
        // A file whose weights are mostly not there: past the first bytes
        // of the weight, the file is cut, and its header still reads...
        let weight = vec![1u8; 4 * 1024 * 1024];
        let model = Model {
            inputs: vec![("image", vec![1, 3, 512, 512])],
            outputs: vec![("out", vec![1, 3, 512, 512])],
            weights: vec![("big", weight, None)],
            ..Model::default()
        };
        let bytes = model.bytes();
        let dir = std::env::temp_dir().join(format!("iai_onnx_header_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.onnx");
        std::fs::write(&path, &bytes).unwrap();
        let header = read_header(&path).unwrap();
        assert_eq!(header.inputs[0].name, "image");
        // ...and a file cut short is said to be so, not read past its end.
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let error = read_header(&path).unwrap_err();
        assert!(error.contains("không phải file ONNX"), "{error}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_is_not_onnx_is_said_to_be_so() {
        for bytes in [
            &b""[..],
            b"PK\x03\x04 a zip, not a model",
            b"<!DOCTYPE html><html>Google Drive - Virus scan warning</html>",
            &[0xff; 64],
        ] {
            let error = parse_header(bytes).unwrap_err();
            assert!(error.contains("không phải file ONNX"), "{error}");
        }
    }

    /// Opt-in: IAI_ONNX_HEADER_PROBE is a folder of real models; prints
    /// what each takes in and gives out.
    #[test]
    #[ignore]
    fn probe_real_models() {
        let Ok(dir) = std::env::var("IAI_ONNX_HEADER_PROBE") else {
            return;
        };
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "onnx") {
                let started = std::time::Instant::now();
                match read_header(&path) {
                    Ok(h) => println!(
                        "{}: {} | meta {:?} | outside {:?} | {} µs",
                        path.display(),
                        h.describe(),
                        h.metadata_keys,
                        h.external_files,
                        started.elapsed().as_micros()
                    ),
                    Err(e) => println!("{}: {e}", path.display()),
                }
            }
        }
    }
}
