//! Where a layer was cut from: a photo and the four corners of the piece
//! laid flat (an ID card on a print sheet), so the piece can be cut again
//! from the photo with other corners, or turned another way. A print of an
//! ID photo sheet (`kind` "id-photo") keeps its person's portrait instead,
//! its corners where the photo lies in the print's layer (its cutting marks
//! round it) and its turn how it lies on the sheet.

/// A piece of a photo laid flat into a layer.
#[derive(Clone, Debug, PartialEq)]
pub struct CutSource {
    /// The photo, as a path on this machine.
    pub path: String,
    /// In the photo's pixels: top-left, top-right, bottom-right, bottom-left
    /// of the piece as it reads.
    pub corners: [(f32, f32); 4],
    /// Turned clockwise after laying it flat: 0, 90, 180 or 270.
    pub turn: u32,
    /// What the piece was taken for (`front-2021`…); empty when not known.
    pub kind: String,
    /// Worth a look: laid flat without being sure of it.
    pub doubtful: bool,
}
