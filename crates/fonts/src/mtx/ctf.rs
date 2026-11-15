//! Compact Table Format (CTF) back to TrueType (W3C MTX submission, sections 4 to 7).
//!
//! Block 1 is an sfnt whose `glyf` and `cvt ` tables are in CTF form and whose `loca` is empty;
//! block 2 holds every glyph's initial push values and block 3 every glyph's remaining
//! instructions. `hdmx` and `VDMX` (device metrics caches, also re-encoded) are left out: nothing
//! here draws with them.

use super::MAX_OUTPUT;

/// A bounds-checked big-endian cursor.
pub(super) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub(super) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    pub(super) fn u8(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    pub(super) fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }

    /// `255USHORT` (section 6.1.1).
    fn u255(&mut self) -> Option<u16> {
        Some(match self.u8()? {
            253 => self.u16()?,
            255 => 253 + u16::from(self.u8()?),
            254 => 506 + u16::from(self.u8()?),
            c => u16::from(c),
        })
    }

    /// `255SHORT` (section 6.1.2).
    fn s255(&mut self) -> Option<i16> {
        let mut code = self.u8()?;
        if code == 253 {
            return self.i16();
        }
        let neg = code == 250;
        if neg {
            code = self.u8()?;
        }
        let v = match code {
            255 => 250 + i16::from(self.u8()?),
            254 => 500 + i16::from(self.u8()?),
            c => i16::from(c),
        };
        Some(if neg { -v } else { v })
    }
}

/// One entry of the point triplet table (section 5.11): bytes after the flag, bits of dx and dy,
/// amounts added to them, and whether each is positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Triplet {
    pub bytes: u8,
    pub x_bits: u8,
    pub y_bits: u8,
    pub dx: u16,
    pub dy: u16,
    pub x_pos: bool,
    pub y_pos: bool,
}

/// The triplet for flag index `i` (0..128), derived from the table's regular structure.
pub(super) fn triplet(i: u8) -> Triplet {
    let i = i & 0x7F;
    let (bytes, x_bits, y_bits, dx, dy, x_pos, y_pos) = match i {
        0..=9 => (1, 0, 8, 0, 256 * u16::from(i / 2), true, i % 2 == 1),
        10..=19 => (1, 8, 0, 256 * u16::from((i - 10) / 2), 0, i % 2 == 1, true),
        20..=83 => {
            let j = i - 20;
            (1, 4, 4, 1 + 16 * u16::from(j / 16), 1 + 16 * u16::from((j % 16) / 4), j % 2 == 1, j % 4 >= 2)
        }
        84..=119 => {
            let j = i - 84;
            (2, 8, 8, 1 + 256 * u16::from(j / 12), 1 + 256 * u16::from((j % 12) / 4), j % 2 == 1, j % 4 >= 2)
        }
        120..=123 => (3, 12, 12, 0, 0, i % 2 == 1, i % 4 >= 2),
        _ => (4, 16, 16, 0, 0, i % 2 == 1, i % 4 >= 2),
    };
    Triplet { bytes, x_bits, y_bits, dx, dy, x_pos, y_pos }
}

/// The TrueType font from the three decompressed blocks.
pub(super) fn rebuild(rest: &[u8], push: &[u8], code: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(rest);
    let version = r.take(4)?;
    let num_tables = usize::from(r.u16()?);
    if num_tables > 1024 {
        return None;
    }
    let dir = rest.get(12..12usize.checked_add(num_tables.checked_mul(16)?)?)?;
    let tables: Vec<([u8; 4], &[u8])> = dir
        .as_chunks::<16>()
        .0
        .iter()
        .map(|e| {
            let tag = [e[0], e[1], e[2], e[3]];
            let off = u32::from_be_bytes([e[8], e[9], e[10], e[11]]) as usize;
            let len = u32::from_be_bytes([e[12], e[13], e[14], e[15]]) as usize;
            Some((tag, if len == 0 { &[][..] } else { rest.get(off..off.checked_add(len)?)? }))
        })
        .collect::<Option<_>>()?;
    let find = |t: &[u8; 4]| tables.iter().find(|(tag, _)| tag == t).map(|(_, d)| *d);
    let num_glyphs = usize::from(Reader { data: find(b"maxp")?, pos: 4 }.u16()?);

    let mut out: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(tables.len());
    let mut push = Reader::new(push);
    let mut code = Reader::new(code);
    let mut copied = 0usize;
    for (tag, data) in &tables {
        // A directory can name the same bytes many times; cap what the copies add up to.
        copied = copied.checked_add(data.len())?;
        if copied > MAX_OUTPUT {
            return None;
        }
        match tag {
            b"loca" | b"hdmx" | b"VDMX" => {}
            b"glyf" => {
                let (glyf, loca) = glyphs(data, num_glyphs, &mut push, &mut code)?;
                out.push((*b"glyf", glyf));
                out.push((*b"loca", loca));
            }
            b"cvt " => out.push((*tag, cvt(data)?)),
            b"head" => {
                let mut head = data.to_vec();
                // The rebuilt loca uses 32-bit offsets.
                head.get_mut(50..52)?.copy_from_slice(&1u16.to_be_bytes());
                out.push((*tag, head));
            }
            _ => out.push((*tag, data.to_vec())),
        }
    }
    sfnt(version, out)
}

/// The decoded `cvt `: a count, then deltas from the previous value (section 5.2).
fn cvt(data: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(data);
    let n = usize::from(r.u16()?);
    let mut out = Vec::with_capacity(2 * n.min(r.remaining()));
    let mut prev = 0i16;
    for _ in 0..n {
        let delta = match r.u8()? {
            c @ 0..=237 => i16::from(c),
            238 => r.i16()?,
            c @ 239..=247 => -(238 * i16::from(c - 239) + i16::from(r.u8()?)),
            c => 238 * i16::from(c - 247) + i16::from(r.u8()?),
        };
        prev = prev.wrapping_add(delta);
        out.extend_from_slice(&prev.to_be_bytes());
    }
    Some(out)
}

/// The TrueType `glyf` and long `loca` for `n` CTF glyphs.
fn glyphs(data: &[u8], n: usize, push: &mut Reader, code: &mut Reader) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut r = Reader::new(data);
    let mut glyf = Vec::with_capacity(data.len().saturating_mul(2).min(MAX_OUTPUT));
    let mut loca = Vec::with_capacity(4 * (n + 1));
    for _ in 0..n {
        loca.extend_from_slice(&u32::try_from(glyf.len()).ok()?.to_be_bytes());
        glyph(&mut r, push, code, &mut glyf)?;
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
        if glyf.len() > MAX_OUTPUT {
            return None;
        }
    }
    loca.extend_from_slice(&u32::try_from(glyf.len()).ok()?.to_be_bytes());
    Some((glyf, loca))
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn glyph(r: &mut Reader, push: &mut Reader, code: &mut Reader, out: &mut Vec<u8>) -> Option<()> {
    let contours = r.i16()?;
    match contours {
        // An empty glyph: nothing in TrueType either.
        0 => Some(()),
        -1 => composite(r, push, code, out),
        1.. => simple(contours, r, push, code, out),
        _ => None,
    }
}

fn composite(r: &mut Reader, push: &mut Reader, code: &mut Reader, out: &mut Vec<u8>) -> Option<()> {
    put16(out, 0xFFFF);
    out.extend_from_slice(r.take(8)?);
    const ARGS_ARE_WORDS: u16 = 0x0001;
    const HAVE_SCALE: u16 = 0x0008;
    const MORE: u16 = 0x0020;
    const XY_SCALE: u16 = 0x0040;
    const TWO_BY_TWO: u16 = 0x0080;
    const HAVE_INSTRUCTIONS: u16 = 0x0100;
    let mut flags;
    // Each component takes at least six bytes, so the input bounds the loop.
    loop {
        flags = r.u16()?;
        put16(out, flags);
        let mut len = 2 + if flags & ARGS_ARE_WORDS != 0 { 4 } else { 2 };
        if flags & HAVE_SCALE != 0 {
            len += 2;
        } else if flags & XY_SCALE != 0 {
            len += 4;
        } else if flags & TWO_BY_TWO != 0 {
            len += 8;
        }
        out.extend_from_slice(r.take(len)?);
        if flags & MORE == 0 {
            break;
        }
    }
    if flags & HAVE_INSTRUCTIONS != 0 {
        let instructions = instructions(r, push, code)?;
        put16(out, u16::try_from(instructions.len()).ok()?);
        out.extend_from_slice(&instructions);
    }
    Some(())
}

fn simple(contours: i16, r: &mut Reader, push: &mut Reader, code: &mut Reader, out: &mut Vec<u8>) -> Option<()> {
    let (contours, bbox) = if contours == 0x7FFF {
        let n = r.i16()?;
        let b = r.take(8)?;
        (n, Some(b))
    } else {
        (contours, None)
    };
    let contours = usize::try_from(contours).ok().filter(|&n| n > 0 && n <= r.remaining())?;
    let mut ends = Vec::with_capacity(contours);
    let mut last = 0u32;
    for i in 0..contours {
        let v = u32::from(r.u255()?);
        last = if i == 0 { v } else { last.checked_add(v)? };
        ends.push(u16::try_from(last).ok()?);
    }
    let points = usize::try_from(last).ok()?.checked_add(1)?;
    // A point is at least a flag byte and one coordinate byte.
    if points > r.remaining() / 2 {
        return None;
    }
    let flags = r.take(points)?;
    let mut deltas = Vec::with_capacity(points);
    let (mut x, mut y) = (0i32, 0i32);
    let (mut x_min, mut y_min, mut x_max, mut y_max) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for &f in flags {
        let t = triplet(f);
        let mut data = 0u64;
        for &b in r.take(usize::from(t.bytes))? {
            data = (data << 8) | u64::from(b);
        }
        let total = 8 * u32::from(t.bytes);
        let field = |shift: u32, bits: u8| -> u64 { if bits == 0 { 0 } else { (data >> shift) & ((1u64 << bits) - 1) } };
        let xv = field(total - u32::from(t.x_bits), t.x_bits) as i32 + i32::from(t.dx);
        let yv = field(total - u32::from(t.x_bits) - u32::from(t.y_bits), t.y_bits) as i32 + i32::from(t.dy);
        let dx = if t.x_pos { xv } else { -xv };
        let dy = if t.y_pos { yv } else { -yv };
        x = x.checked_add(dx)?;
        y = y.checked_add(dy)?;
        (x_min, y_min, x_max, y_max) = (x_min.min(x), y_min.min(y), x_max.max(x), y_max.max(y));
        deltas.push((i16::try_from(dx).ok()?, i16::try_from(dy).ok()?, f & 0x80 == 0));
    }
    let instructions = instructions(r, push, code)?;

    put16(out, contours as u16);
    match bbox {
        Some(b) => out.extend_from_slice(b),
        None => {
            for v in [x_min, y_min, x_max, y_max] {
                put16(out, i16::try_from(v).ok()? as u16);
            }
        }
    }
    for e in ends {
        put16(out, e);
    }
    put16(out, u16::try_from(instructions.len()).ok()?);
    out.extend_from_slice(&instructions);
    // Plain flags (on-curve bit only) and two-byte deltas: valid TrueType, simplest to write.
    out.extend(deltas.iter().map(|&(_, _, on)| u8::from(on)));
    for &(dx, _, _) in &deltas {
        put16(out, dx as u16);
    }
    for &(_, dy, _) in &deltas {
        put16(out, dy as u16);
    }
    Some(())
}

/// A glyph's instructions: its push values (block 2) as new push instructions, then its
/// remaining code (block 3).
fn instructions(r: &mut Reader, push: &mut Reader, code: &mut Reader) -> Option<Vec<u8>> {
    let push_count = usize::from(r.u255()?);
    let code_size = usize::from(r.u255()?);
    let values = push_values(push, push_count)?;
    let mut out = push_instructions(&values);
    out.extend_from_slice(code.take(code_size)?);
    Some(out)
}

const HOP3: u8 = 251;
const HOP4: u8 = 252;

/// `count` push values with hop codes expanded (section 6.2.4).
fn push_values(r: &mut Reader, count: usize) -> Option<Vec<i16>> {
    // A value takes at least one byte, and a hop code at most five values per byte.
    let mut v: Vec<i16> = Vec::with_capacity(count.min(r.remaining().saturating_mul(5)));
    while v.len() < count {
        let hop = r.peek()?;
        if hop == HOP3 || hop == HOP4 {
            r.u8()?;
            let a = *v.get(v.len().checked_sub(2)?)?;
            v.push(a);
            v.push(r.s255()?);
            v.push(a);
            if hop == HOP4 {
                v.push(r.s255()?);
                v.push(a);
            }
        } else {
            v.push(r.s255()?);
        }
    }
    (v.len() == count).then_some(v)
}

/// TrueType instructions that push `values` in order: byte pushes for runs that fit a byte,
/// word pushes otherwise, at most 255 values per instruction.
fn push_instructions(values: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 2 + 4);
    let fits = |v: &i16| (0..=255).contains(v);
    let mut rest = values;
    while let Some(first) = rest.first() {
        let bytes = fits(first);
        let run = rest.iter().take(255).take_while(|v| fits(v) == bytes).count();
        let (chunk, tail) = rest.split_at(run);
        rest = tail;
        let n = chunk.len() as u8;
        match (bytes, n) {
            (true, 1..=8) => out.push(0xB0 + n - 1),
            (false, 1..=8) => out.push(0xB8 + n - 1),
            (true, _) => out.extend_from_slice(&[0x40, n]),
            (false, _) => out.extend_from_slice(&[0x41, n]),
        }
        for &v in chunk {
            if bytes {
                out.push(v as u8);
            } else {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
    }
    out
}

/// An sfnt with `tables` (sorted by tag, padded to four bytes, with checksums).
fn sfnt(version: &[u8], mut tables: Vec<([u8; 4], Vec<u8>)>) -> Option<Vec<u8>> {
    tables.sort_by_key(|t| t.0);
    let n = u16::try_from(tables.len()).ok().filter(|&n| n > 0 && n < 4096)?;
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow * 2 <= n {
        pow *= 2;
        log += 1;
    }
    let total: usize = tables.iter().map(|(_, d)| (d.len() + 3) & !3).sum::<usize>() + 12 + 16 * tables.len();
    if total > MAX_OUTPUT {
        return None;
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(version);
    for v in [n, pow * 16, log, n * 16 - pow * 16] {
        put16(&mut out, v);
    }
    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        let sum = data.chunks(4).fold(0u32, |s, c| {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            s.wrapping_add(u32::from_be_bytes(w))
        });
        out.extend_from_slice(tag);
        out.extend_from_slice(&sum.to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).ok()?.to_be_bytes());
        out.extend_from_slice(&u32::try_from(data.len()).ok()?.to_be_bytes());
        offset += (data.len() + 3) & !3;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    Some(out)
}
