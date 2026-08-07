//! PSD "descriptor structure" — the recursive key/value tree Photoshop uses for
//! the type tool, descriptor-based adjustment layers (Vibrance, Black & White,
//! …), fills and effects.
//!
//! One shared, bounds-checked parser lives here so every consumer decodes the
//! same well-specified format (Objc/VlLs/TEXT/doub/UntF/enum/long/bool/tdta/…).
//! An unknown OSType has no length we can skip, so the parser bails (`None`)
//! rather than guess and desync — callers then keep their fallback (the flat
//! raster, or skipping the adjustment).

/// A descriptor value. `Other` marks a value we parsed far enough to skip but
/// don't model.
#[derive(Debug, Clone)]
pub(crate) enum Val {
    Desc(Descriptor),
    List(Vec<Val>),
    Double(f64),
    UnitFloat(f64),
    Text(String),
    Enum(String),
    Int(i32),
    Bool(bool),
    Raw(Vec<u8>),
    Other,
}

impl Val {
    /// Numeric value of an `Int`/`Double`/`UnitFloat` (adjustments store slider
    /// amounts as `long`, occasionally as `doub`).
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Int(i) => Some(*i as f64),
            Val::Double(d) | Val::UnitFloat(d) => Some(*d),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Val::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Descriptor {
    pub(crate) items: Vec<(String, Val)>,
}

impl Descriptor {
    pub(crate) fn get(&self, key: &str) -> Option<&Val> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Numeric convenience: `get(key).as_f64()`.
    pub(crate) fn num(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Val::as_f64)
    }
}

/// Parse a version-prefixed descriptor block (`u32` descriptor version, then the
/// descriptor) — the framing used by descriptor-based adjustment layers such as
/// `vibA`/`blwh`. Returns `None` on any short/unexpected field.
pub(crate) fn parse_versioned_descriptor(data: &[u8]) -> Option<Descriptor> {
    let mut c = Cur::new(data);
    let _version = c.u32()?; // = 16
    read_descriptor(&mut c)
}

/// A descriptor: Unicode name, classID, item count, then `count` key/OSType/value
/// triples. The leading `Objc`/`GlbO` OSType (when nested) is consumed by the
/// caller before entering here.
pub(crate) fn read_descriptor(c: &mut Cur) -> Option<Descriptor> {
    let _name = read_unicode(c)?; // "name from classID"
    let _class = read_key(c)?; // classID
    let count = c.u32()? as usize;
    if count > 100_000 {
        return None; // implausible — treat as corrupt
    }
    let mut items = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let key = read_key(c)?;
        let os = c.take(4)?;
        let os = [os[0], os[1], os[2], os[3]];
        let val = read_value(c, &os)?;
        items.push((key, val));
    }
    Some(Descriptor { items })
}

fn read_value(c: &mut Cur, os: &[u8; 4]) -> Option<Val> {
    match os {
        b"Objc" | b"GlbO" => Some(Val::Desc(read_descriptor(c)?)),
        b"VlLs" => {
            let n = c.u32()? as usize;
            if n > 1_000_000 {
                return None;
            }
            let mut v = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                let os2 = c.take(4)?;
                let os2 = [os2[0], os2[1], os2[2], os2[3]];
                v.push(read_value(c, &os2)?);
            }
            Some(Val::List(v))
        }
        b"doub" => Some(Val::Double(c.f64()?)),
        b"UntF" => {
            let _unit = c.take(4)?;
            Some(Val::UnitFloat(c.f64()?))
        }
        b"TEXT" => Some(Val::Text(read_unicode(c)?)),
        b"enum" => {
            let _type = read_key(c)?;
            Some(Val::Enum(read_key(c)?))
        }
        b"long" => Some(Val::Int(c.u32()? as i32)),
        b"comp" => {
            c.take(8)?;
            Some(Val::Other)
        }
        b"bool" => Some(Val::Bool(c.u8()? != 0)),
        b"type" | b"GlbC" => {
            let _name = read_unicode(c)?;
            let _class = read_key(c)?;
            Some(Val::Other)
        }
        b"tdta" => {
            let n = c.u32()? as usize;
            Some(Val::Raw(c.take(n)?.to_vec()))
        }
        b"alis" => {
            let n = c.u32()? as usize;
            c.take(n)?;
            Some(Val::Other)
        }
        // 'obj ' (Reference) and any unknown OSType have no length we can skip
        // safely — bail so the caller falls back to its default behaviour.
        _ => None,
    }
}

/// Descriptor Unicode string: a `u32` length in UTF-16 code units, then that many
/// UTF-16BE units (a trailing NUL terminator is stripped).
fn read_unicode(c: &mut Cur) -> Option<String> {
    let n = c.u32()? as usize;
    if n > 10_000_000 {
        return None;
    }
    let mut units = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        units.push(c.u16()?);
    }
    if units.last() == Some(&0) {
        units.pop();
    }
    Some(String::from_utf16_lossy(&units))
}

/// Descriptor key/classID: a `u32` length; if zero, a 4-byte key, else that many
/// ASCII bytes.
fn read_key(c: &mut Cur) -> Option<String> {
    let n = c.u32()? as usize;
    let bytes = if n == 0 { c.take(4)? } else { c.take(n)? };
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Bounds-checked big-endian cursor.
pub(crate) struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    pub(crate) fn new(d: &'a [u8]) -> Self {
        Self { d, p: 0 }
    }
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub(crate) fn f64(&mut self) -> Option<f64> {
        self.take(8)
            .map(|b| f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn push_key(v: &mut Vec<u8>, key: &str) {
        push_u32(v, key.len() as u32);
        v.extend_from_slice(key.as_bytes());
    }
    fn push_unicode(v: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().collect();
        push_u32(v, units.len() as u32 + 1);
        for u in units {
            v.extend_from_slice(&u.to_be_bytes());
        }
        v.extend_from_slice(&0u16.to_be_bytes());
    }

    /// Build a versioned descriptor with the given `long` items.
    fn versioned_long_desc(class: &str, items: &[(&str, i32)]) -> Vec<u8> {
        let mut d = Vec::new();
        push_u32(&mut d, 16); // descriptor version
        push_unicode(&mut d, ""); // name
        push_key(&mut d, class); // classID
        push_u32(&mut d, items.len() as u32);
        for (k, v) in items {
            push_key(&mut d, k);
            d.extend_from_slice(b"long");
            push_u32(&mut d, *v as u32);
        }
        d
    }

    #[test]
    fn parses_versioned_long_descriptor() {
        let block = versioned_long_desc("vibrance", &[("vibrance", 30), ("Strt", -15)]);
        let desc = parse_versioned_descriptor(&block).expect("descriptor");
        assert_eq!(desc.num("vibrance"), Some(30.0));
        assert_eq!(desc.num("Strt"), Some(-15.0));
        assert_eq!(desc.num("missing"), None);
    }

    #[test]
    fn nested_and_typed_values() {
        // { flag: bool true, inner: Objc { amt: long 5 } }
        let mut d = Vec::new();
        push_u32(&mut d, 16);
        push_unicode(&mut d, "");
        push_key(&mut d, "root");
        push_u32(&mut d, 2);
        push_key(&mut d, "flag");
        d.extend_from_slice(b"bool");
        d.push(1);
        push_key(&mut d, "inner");
        d.extend_from_slice(b"Objc");
        push_unicode(&mut d, "");
        push_key(&mut d, "cls");
        push_u32(&mut d, 1);
        push_key(&mut d, "amt");
        d.extend_from_slice(b"long");
        push_u32(&mut d, 5);

        let desc = parse_versioned_descriptor(&d).expect("descriptor");
        assert_eq!(desc.get("flag").and_then(Val::as_bool), Some(true));
        let Some(Val::Desc(inner)) = desc.get("inner") else {
            panic!("expected nested descriptor");
        };
        assert_eq!(inner.num("amt"), Some(5.0));
    }

    #[test]
    fn unknown_ostype_bails() {
        let mut d = Vec::new();
        push_u32(&mut d, 16);
        push_unicode(&mut d, "");
        push_key(&mut d, "x");
        push_u32(&mut d, 1);
        push_key(&mut d, "ref");
        d.extend_from_slice(b"obj "); // Reference — unhandled
        assert!(parse_versioned_descriptor(&d).is_none());
    }

    #[test]
    fn short_block_is_none_not_panic() {
        assert!(parse_versioned_descriptor(&[0, 0, 0]).is_none());
        assert!(parse_versioned_descriptor(&[]).is_none());
    }
}
