//! MicroType Express (MTX): the compressed font form inside Embedded OpenType parts that
//! PowerPoint writes when it embeds fonts (`TTEMBED_TTCOMPRESSED`).
//!
//! Implemented from the public W3C Member Submission "MicroType Express (MTX) Font Format"
//! (2008): a 10-byte header, three LZCOMP-compressed blocks ([`lzcomp`]), and the Compact Table
//! Format that turns them back into a TrueType font ([`ctf`]).
//!
//! Every input is treated as hostile: reads are bounds-checked, arithmetic is checked, loops are
//! bounded by the input, and output is capped at [`MAX_OUTPUT`]. Anything malformed returns
//! `None`.

mod ctf;
#[cfg(test)]
pub(crate) mod encode;
mod lzcomp;

/// The largest block or font this decoder produces. PowerPoint embeds subsets; a whole CJK face
/// (tens of megabytes) still fits.
pub(crate) const MAX_OUTPUT: usize = 64 << 20;

/// The header size: version (1 byte), copy limit, offset of block 2, offset of block 3 (3 bytes
/// each, big-endian).
const HEADER: usize = 10;

/// The TrueType font in an MTX blob, or none when the data isn't a well-formed one.
pub(crate) fn decode(data: &[u8]) -> Option<Vec<u8>> {
    let h = data.get(..HEADER)?;
    // Version 3 is MTX 1.0; 2 is an older LZCOMP stream with the same layout (the only
    // difference, a leading run-length flag, is read by both).
    if !matches!(h.first(), Some(2 | 3)) {
        return None;
    }
    let u24 = |i: usize| -> Option<usize> {
        let b = h.get(i..i + 3)?;
        Some((usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2]))
    };
    let off2 = u24(4)?;
    let off3 = u24(7)?;
    if off2 < HEADER || off3 < off2 || off3 > data.len() {
        return None;
    }
    let rest = lzcomp::decompress(data.get(HEADER..off2)?)?;
    let push = lzcomp::decompress(data.get(off2..off3)?)?;
    let code = lzcomp::decompress(data.get(off3..)?)?;
    ctf::rebuild(&rest, &push, &code)
}

#[cfg(test)]
mod tests;
