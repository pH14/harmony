// SPDX-License-Identifier: AGPL-3.0-or-later
//! Output — a PPM (P6) frame writer and a contact-sheet compositor, with **zero
//! new dependencies** (task 87 §4). Both are trivially hand-written and
//! byte-exact (rule 4: no floats; goldens are exact).
//!
//! Video encoding stays **outside the repo** — the crate README documents the
//! ffmpeg one-liner over a PPM sequence. Rendered game frames are **never
//! committed** (they are the game publisher's imagery, same hygiene as the ROM);
//! the committed artifact is a [`blake3_hex`] digest, and the contact sheet is
//! attached to the report.

use thiserror::Error;

use crate::render::{Frame, RenderError};

/// Serialize a [`Frame`] as binary PPM (P6): the ASCII header `P6\n<w>
/// <h>\n255\n` followed by the raw `width * height * 3` RGB bytes. Byte-exact and
/// deterministic — the format the golden tests pin and the ffmpeg one-liner
/// consumes.
pub fn write_ppm(frame: &Frame) -> Vec<u8> {
    let header = format!("P6\n{} {}\n255\n", frame.width(), frame.height());
    let mut out = Vec::with_capacity(header.len() + frame.rgb().len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(frame.rgb());
    out
}

/// Compose frames into one contact-sheet image: `cols` columns, row-major, every
/// cell the same size, unused trailing cells filled with `background`. The
/// caller has already thinned the clip by stride (task 87 §1), so this simply
/// tiles what it is given.
///
/// Every frame must share the first frame's dimensions
/// ([`OutputError::DimensionMismatch`] otherwise), `cols ≥ 1`, and the sheet
/// dimensions must not overflow.
pub fn contact_sheet(
    frames: &[Frame],
    cols: u32,
    background: [u8; 3],
) -> Result<Frame, OutputError> {
    if frames.is_empty() {
        return Err(OutputError::Empty);
    }
    if cols == 0 {
        return Err(OutputError::ZeroCols);
    }
    let cell_w = frames[0].width();
    let cell_h = frames[0].height();
    for (i, f) in frames.iter().enumerate() {
        if f.width() != cell_w || f.height() != cell_h {
            return Err(OutputError::DimensionMismatch {
                index: i,
                got: (f.width(), f.height()),
                expected: (cell_w, cell_h),
            });
        }
    }
    let n = frames.len() as u32;
    // rows = ceil(n / cols); n ≥ 1 and cols ≥ 1 so this is ≥ 1.
    let rows = n.div_ceil(cols);
    let sheet_w = cols.checked_mul(cell_w).ok_or(OutputError::SheetOverflow)?;
    let sheet_h = rows.checked_mul(cell_h).ok_or(OutputError::SheetOverflow)?;
    // Start from a solid background, then blit each cell over it.
    let mut sheet = Frame::solid(sheet_w, sheet_h, background)?;
    let mut rgb = sheet.rgb().to_vec();
    let row_stride = sheet_w as usize * 3;
    for (idx, f) in frames.iter().enumerate() {
        let cell_col = idx as u32 % cols;
        let cell_row = idx as u32 / cols;
        let x0 = (cell_col * cell_w) as usize;
        let y0 = (cell_row * cell_h) as usize;
        for y in 0..cell_h as usize {
            let dst_row = (y0 + y) * row_stride + x0 * 3;
            let src_row = y * cell_w as usize * 3;
            rgb[dst_row..dst_row + cell_w as usize * 3]
                .copy_from_slice(&f.rgb()[src_row..src_row + cell_w as usize * 3]);
        }
    }
    // Rebuild through the checked constructor so the length invariant is proven.
    sheet = Frame::from_rgb(sheet_w, sheet_h, rgb)?;
    Ok(sheet)
}

/// The lower-case hex blake3 digest of `bytes` — the committed artifact for a
/// rendered frame or contact sheet (the image itself is never committed).
pub fn blake3_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Why an output step failed (rule 4: distinct, panic-free).
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum OutputError {
    /// A contact sheet was asked for with no frames.
    #[error("cannot build a contact sheet from zero frames")]
    Empty,
    /// A contact sheet was asked for with zero columns.
    #[error("contact sheet needs at least one column")]
    ZeroCols,
    /// A frame's dimensions differed from the first frame's.
    #[error("frame {index} is {got:?}, expected {expected:?} to match the sheet cell")]
    DimensionMismatch {
        /// The offending frame's index.
        index: usize,
        /// Its dimensions.
        got: (u32, u32),
        /// The cell dimensions (the first frame's).
        expected: (u32, u32),
    },
    /// The composed sheet dimensions overflowed `u32`.
    #[error("contact sheet dimensions overflow")]
    SheetOverflow,
    /// A frame constructor rejected the composed pixels (a shape invariant).
    #[error("frame construction failed: {0}")]
    Frame(#[from] RenderError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(color: [u8; 3]) -> Frame {
        Frame::solid(1, 1, color).unwrap()
    }

    #[test]
    fn ppm_is_byte_exact() {
        let f = Frame::from_rgb(2, 2, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]).unwrap();
        let ppm = write_ppm(&f);
        let mut expected = b"P6\n2 2\n255\n".to_vec();
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(ppm, expected);
    }

    #[test]
    fn contact_sheet_tiles_row_major() {
        // Two 1x1 cells, 2 columns → a 2x1 sheet, [A, B].
        let sheet =
            contact_sheet(&[frame([10, 20, 30]), frame([40, 50, 60])], 2, [0, 0, 0]).unwrap();
        assert_eq!(sheet.width(), 2);
        assert_eq!(sheet.height(), 1);
        assert_eq!(sheet.rgb(), &[10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn contact_sheet_fills_trailing_cells_with_background() {
        // Three 1x1 cells, 2 columns → a 2x2 sheet; the fourth cell is
        // background.
        let sheet = contact_sheet(
            &[frame([1, 1, 1]), frame([2, 2, 2]), frame([3, 3, 3])],
            2,
            [9, 9, 9],
        )
        .unwrap();
        assert_eq!((sheet.width(), sheet.height()), (2, 2));
        // row 0: [1, 2]; row 1: [3, background]
        assert_eq!(
            sheet.rgb(),
            &[1, 1, 1, 2, 2, 2, /* row1 */ 3, 3, 3, 9, 9, 9]
        );
    }

    #[test]
    fn contact_sheet_rejects_mismatched_dimensions() {
        let big = Frame::solid(2, 2, [0, 0, 0]).unwrap();
        assert!(matches!(
            contact_sheet(&[frame([0, 0, 0]), big], 2, [0, 0, 0]),
            Err(OutputError::DimensionMismatch { index: 1, .. })
        ));
        assert_eq!(contact_sheet(&[], 2, [0, 0, 0]), Err(OutputError::Empty));
        assert_eq!(
            contact_sheet(&[frame([0, 0, 0])], 0, [0, 0, 0]),
            Err(OutputError::ZeroCols)
        );
    }

    // blake3 uses SIMD/FFI its pure-Rust callers cannot reach under Miri
    // (unsupported intrinsic / foreign call), so this digest test is skipped
    // there; the rest of the crate's logic stays Miri-covered.
    #[cfg_attr(miri, ignore = "blake3 SIMD/FFI is not Miri-interpretable")]
    #[test]
    fn blake3_hex_is_stable_and_hex() {
        let h = blake3_hex(b"the same bytes");
        assert_eq!(h, blake3_hex(b"the same bytes"));
        assert_eq!(h.len(), 64);
        assert!(
            h.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }
}
