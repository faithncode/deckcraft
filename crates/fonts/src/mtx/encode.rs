//! A test-only MicroType Express encoder, so the decoder is tested on fonts we compress
//! ourselves (never on files another product wrote). It follows the same W3C submission: CTF
//! glyphs with triplet points, split push data with hop codes, `cvt ` deltas, and LZCOMP with
//! literals, one-byte copies, copy items and the optional run-length layer. Its choices are
//! simple (greedy matches) but every construct the decoder reads is produced.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::ctf::{Reader, triplet};
use super::lzcomp::{AHuff, DIST_WIDTH, LEN_WIDTH, MAX_2BYTE_DIST, PRELOAD, dist_ranges, preload};
use std::collections::HashMap;

/// Options for [`compress_font`].
#[derive(Clone, Copy)]
pub(crate) struct Options {
    /// Use the run-length layer under LZCOMP.
    pub run_length: bool,
    /// Find copy items (otherwise literals and one-byte copies only).
    pub copies: bool,
}

/// An MTX blob for a TrueType font.
pub(crate) fn compress_font(ttf: &[u8], opt: Options) -> Vec<u8> {
    let (rest, push, code) = to_ctf(ttf);
    let blocks = [compress(&rest, opt), compress(&push, opt), compress(&code, opt)];
    let off2 = 10 + blocks[0].len();
    let off3 = off2 + blocks[1].len();
    let mut out = vec![3];
    for v in [0x015E00usize, off2, off3] {
        out.extend_from_slice(&(v as u32).to_be_bytes()[1..]);
    }
    for b in &blocks {
        out.extend_from_slice(b);
    }
    out
}

/// An Embedded OpenType (version 0x00020002) around `font_data` with `flags`, family name
/// `family`.
pub(crate) fn eot(font_data: &[u8], flags: u32, family: &str) -> Vec<u8> {
    let mut h = vec![0u8; 82];
    h[8..12].copy_from_slice(&0x0002_0002u32.to_le_bytes());
    h[12..16].copy_from_slice(&flags.to_le_bytes());
    h[34..36].copy_from_slice(&0x504Cu16.to_le_bytes());
    let name = |h: &mut Vec<u8>, s: &str| {
        let u: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
        h.extend_from_slice(&(u.len() as u16).to_le_bytes());
        h.extend_from_slice(&u);
        h.extend_from_slice(&[0, 0]); // padding
    };
    for s in [family, "Regular", "Version 1", family, ""] {
        name(&mut h, s);
    }
    h.extend_from_slice(&[0; 4]); // RootStringCheckSum
    h.extend_from_slice(&[0; 4]); // EUDCCodePage
    h.extend_from_slice(&[0, 0]); // Padding6
    h.extend_from_slice(&[0, 0]); // SignatureSize
    h.extend_from_slice(&[0; 4]); // EUDCFlags
    h.extend_from_slice(&[0; 4]); // EUDCFontSize
    let data: Vec<u8> = if flags & 0x1000_0000 != 0 { font_data.iter().map(|b| b ^ 0x50).collect() } else { font_data.to_vec() };
    h.extend_from_slice(&data);
    let total = h.len() as u32;
    h[0..4].copy_from_slice(&total.to_le_bytes());
    h[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
    h
}

// ---- LZCOMP ----

struct BitOut {
    bits: Vec<bool>,
}

impl BitOut {
    fn value(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.bits.push((v >> i) & 1 == 1);
        }
    }

    fn bytes(&self) -> Vec<u8> {
        self.bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |b, (i, &x)| b | (u8::from(x) << (7 - i)))).collect()
    }
}

fn bits_used(x: usize) -> u32 {
    (usize::BITS - x.leading_zeros()).max(1)
}

fn run_length(data: &[u8]) -> Vec<u8> {
    let mut counts = [0usize; 256];
    for &b in data {
        counts[usize::from(b)] += 1;
    }
    let escape = (0..=255u8).min_by_key(|&b| counts[usize::from(b)]).unwrap();
    let mut out = vec![escape];
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        let run = data[i..].iter().take(255).take_while(|&&x| x == b).count();
        if run > 3 {
            out.extend_from_slice(&[escape, run as u8, b]);
            i += run;
        } else {
            if b == escape {
                out.extend_from_slice(&[escape, 0]);
            } else {
                out.push(b);
            }
            i += 1;
        }
    }
    out
}

/// One LZCOMP block.
pub(crate) fn compress(input: &[u8], opt: Options) -> Vec<u8> {
    let packed;
    let data = if opt.run_length {
        packed = run_length(input);
        &packed[..]
    } else {
        input
    };
    let mut out = BitOut { bits: vec![opt.run_length] };
    let mut dist_tree = AHuff::new(1 << DIST_WIDTH).unwrap();
    let mut len_tree = AHuff::new(1 << LEN_WIDTH).unwrap();
    out.value(data.len() as u32, 24);
    let ranges = dist_ranges(data.len());
    let dist_max = 1usize << (DIST_WIDTH * ranges);
    let dup2 = 256 + (1usize << LEN_WIDTH) * ranges as usize;
    let mut sym_tree = AHuff::new(dup2 + 3).unwrap();

    let mut hist = preload();
    hist.extend_from_slice(data);
    let mut chains: HashMap<(u8, u8), Vec<usize>> = HashMap::new();
    let add = |chains: &mut HashMap<(u8, u8), Vec<usize>>, hist: &[u8], i: usize| {
        if i + 1 < hist.len() {
            chains.entry((hist[i], hist[i + 1])).or_default().push(i);
        }
    };
    for i in 0..PRELOAD {
        add(&mut chains, &hist, i);
    }
    let mut i = PRELOAD;
    while i < hist.len() {
        let mut best = (0usize, 0usize); // (length, distance to tail)
        if let Some(c) = (opt.copies && i + 1 < hist.len()).then(|| chains.get(&(hist[i], hist[i + 1]))).flatten() {
            {
                for &s in c.iter().rev().take(48) {
                    let max = (i - s).min(hist.len() - i);
                    let len = (0..max).take_while(|&k| hist[s + k] == hist[i + k]).count();
                    if len < 2 {
                        continue;
                    }
                    let dist = i - s - len + 1;
                    if dist > dist_max || (dist >= MAX_2BYTE_DIST && len < 3) {
                        continue;
                    }
                    if len > best.0 {
                        best = (len, dist);
                    }
                }
            }
        }
        let (len, dist) = best;
        if len >= 3 || (len == 2 && dist < MAX_2BYTE_DIST) {
            let coded_len = len - if dist >= MAX_2BYTE_DIST { 3 } else { 2 };
            let d = dist - 1;
            let n_dist = bits_used(d).div_ceil(DIST_WIDTH) as usize;
            let groups = bits_used(coded_len).div_ceil(2) as usize;
            for g in 0..groups {
                let two = (coded_len >> (2 * (groups - 1 - g))) & 3;
                let sym = (usize::from(g + 1 < groups) << 2) | two;
                if g == 0 {
                    sym_tree.write(256 + sym + (n_dist - 1) * (1 << LEN_WIDTH), &mut out.bits).unwrap();
                } else {
                    len_tree.write(sym, &mut out.bits).unwrap();
                }
            }
            for g in (0..n_dist).rev() {
                dist_tree.write((d >> (DIST_WIDTH as usize * g)) & 7, &mut out.bits).unwrap();
            }
            for k in 0..len {
                add(&mut chains, &hist, i + k);
            }
            i += len;
        } else {
            let c = hist[i];
            let sym = [2, 4, 6].iter().position(|&b| hist[i - b] == c).map_or(usize::from(c), |p| dup2 + p);
            sym_tree.write(sym, &mut out.bits).unwrap();
            add(&mut chains, &hist, i);
            i += 1;
        }
    }
    out.bytes()
}

// ---- CTF ----

/// The three CTF blocks (font tables, push data, instructions) for a TrueType font.
fn to_ctf(ttf: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let n = u16::from_be_bytes([ttf[4], ttf[5]]) as usize;
    let mut tables: Vec<([u8; 4], &[u8])> = (0..n)
        .map(|i| {
            let e = &ttf[12 + 16 * i..28 + 16 * i];
            let off = u32::from_be_bytes(e[8..12].try_into().unwrap()) as usize;
            let len = u32::from_be_bytes(e[12..16].try_into().unwrap()) as usize;
            (e[0..4].try_into().unwrap(), &ttf[off..off + len])
        })
        .collect();
    let find = |t: &[u8; 4]| tables.iter().find(|(tag, _)| tag == t).unwrap().1;
    let head = find(b"head");
    let long_loca = head[51] == 1;
    let num_glyphs = u16::from_be_bytes([find(b"maxp")[4], find(b"maxp")[5]]) as usize;
    let loca = find(b"loca");
    let glyf = find(b"glyf");
    let offset = |g: usize| {
        if long_loca {
            u32::from_be_bytes(loca[4 * g..4 * g + 4].try_into().unwrap()) as usize
        } else {
            2 * u16::from_be_bytes([loca[2 * g], loca[2 * g + 1]]) as usize
        }
    };
    let (mut ctf_glyf, mut push, mut code) = (Vec::new(), Vec::new(), Vec::new());
    for g in 0..num_glyphs {
        glyph(&glyf[offset(g)..offset(g + 1)], &mut ctf_glyf, &mut push, &mut code);
    }
    let ctf_cvt = tables.iter().find(|(t, _)| t == b"cvt ").map(|(_, d)| cvt(d));
    tables.retain(|(t, _)| t != b"hdmx" && t != b"VDMX");
    let owned: Vec<([u8; 4], Vec<u8>)> = tables
        .iter()
        .map(|(t, d)| {
            let data = match t {
                b"glyf" => ctf_glyf.clone(),
                b"loca" => Vec::new(),
                b"cvt " => ctf_cvt.clone().unwrap(),
                _ => d.to_vec(),
            };
            (*t, data)
        })
        .collect();
    let mut rest = ttf[0..4].to_vec();
    rest.extend_from_slice(&(owned.len() as u16).to_be_bytes());
    rest.extend_from_slice(&ttf[6..12]);
    let mut off = 12 + 16 * owned.len();
    for (t, d) in &owned {
        rest.extend_from_slice(t);
        rest.extend_from_slice(&[0; 4]);
        let o = if d.is_empty() { 0 } else { off };
        rest.extend_from_slice(&(o as u32).to_be_bytes());
        rest.extend_from_slice(&(d.len() as u32).to_be_bytes());
        off += (d.len() + 3) & !3;
    }
    for (_, d) in &owned {
        rest.extend_from_slice(d);
        while !rest.len().is_multiple_of(4) {
            rest.push(0);
        }
    }
    (rest, push, code)
}

fn cvt(data: &[u8]) -> Vec<u8> {
    let n = data.len() / 2;
    let mut out = (n as u16).to_be_bytes().to_vec();
    let mut prev = 0i16;
    for c in data.as_chunks::<2>().0 {
        let v = i16::from_be_bytes([c[0], c[1]]);
        let d = v.wrapping_sub(prev);
        prev = v;
        let m = i32::from(d).abs();
        if d == i16::MIN || m >= 238 * 9 {
            out.push(238);
            out.extend_from_slice(&d.to_be_bytes());
        } else if d < 0 {
            out.push(239 + (m / 238) as u8);
            out.push((m % 238) as u8);
        } else if m < 238 {
            out.push(m as u8);
        } else {
            out.push(247 + (m / 238) as u8);
            out.push((m % 238) as u8);
        }
    }
    out
}

fn u255(out: &mut Vec<u8>, v: usize) {
    match v {
        0..=252 => out.push(v as u8),
        253..=505 => out.extend_from_slice(&[255, (v - 253) as u8]),
        506..=761 => out.extend_from_slice(&[254, (v - 506) as u8]),
        _ => {
            out.push(253);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        }
    }
}

fn s255(out: &mut Vec<u8>, v: i16) {
    let m = i32::from(v).abs();
    if m > 755 || v == i16::MIN {
        out.push(253);
        out.extend_from_slice(&v.to_be_bytes());
        return;
    }
    if v < 0 {
        out.push(250);
    }
    match m {
        0..=249 => out.push(m as u8),
        250..=499 => out.extend_from_slice(&[255, (m - 250) as u8]),
        _ => out.extend_from_slice(&[254, (m - 500) as u8]),
    }
}

/// Splits instructions into their leading push values and the rest; writes the counts to the
/// glyph, the values (with hop codes) to `push` and the rest to `code`.
fn split_instructions(ins: &[u8], glyph: &mut Vec<u8>, push: &mut Vec<u8>, code: &mut Vec<u8>) {
    let mut values: Vec<i16> = Vec::new();
    let mut p = 0;
    while p < ins.len() {
        let op = ins[p];
        let (count, words, start) = match op {
            0x40 => (usize::from(ins[p + 1]), false, p + 2),
            0x41 => (usize::from(ins[p + 1]), true, p + 2),
            0xB0..=0xB7 => (usize::from(op - 0xAF), false, p + 1),
            0xB8..=0xBF => (usize::from(op - 0xB7), true, p + 1),
            _ => break,
        };
        for k in 0..count {
            values.push(if words { i16::from_be_bytes([ins[start + 2 * k], ins[start + 2 * k + 1]]) } else { i16::from(ins[start + k]) });
        }
        p = start + count * if words { 2 } else { 1 };
    }
    u255(glyph, values.len());
    u255(glyph, ins.len() - p);
    code.extend_from_slice(&ins[p..]);
    let mut i = 0;
    while i < values.len() {
        let a = if i >= 2 { Some(values[i - 2]) } else { None };
        let at = |k: usize| values.get(k).copied();
        // A, X1, [A, X2, A, X3, A] → Hop4 X2 X3 ; [A, X2, A] → Hop3 X2.
        if a.is_some() && at(i) == a && at(i + 2) == a && at(i + 4) == a && i + 4 < values.len() {
            push.push(252);
            s255(push, values[i + 1]);
            s255(push, values[i + 3]);
            i += 5;
        } else if a.is_some() && at(i) == a && at(i + 2) == a {
            push.push(251);
            s255(push, values[i + 1]);
            i += 3;
        } else {
            s255(push, values[i]);
            i += 1;
        }
    }
}

fn triplet_for(dx: i32, dy: i32) -> (u8, Vec<u8>) {
    for i in 0..128u8 {
        let t = triplet(i);
        let fit = |d: i32, bits: u8, add: u16, pos: bool| -> Option<u64> {
            if bits == 0 {
                return (d == 0).then_some(0);
            }
            if d != 0 && (d > 0) != pos {
                return None;
            }
            let m = d.unsigned_abs().checked_sub(u32::from(add))?;
            (u64::from(m) < (1u64 << bits)).then_some(u64::from(m))
        };
        let (Some(x), Some(y)) = (fit(dx, t.x_bits, t.dx, t.x_pos), fit(dy, t.y_bits, t.dy, t.y_pos)) else { continue };
        let total = 8 * u32::from(t.bytes);
        let data = (x << (total - u32::from(t.x_bits))) | (y << (total - u32::from(t.x_bits) - u32::from(t.y_bits)));
        let bytes = (0..t.bytes).map(|k| (data >> (8 * (t.bytes - 1 - k))) as u8).collect();
        return (i, bytes);
    }
    panic!("no triplet for ({dx}, {dy})");
}

fn glyph(g: &[u8], out: &mut Vec<u8>, push: &mut Vec<u8>, code: &mut Vec<u8>) {
    if g.is_empty() {
        out.extend_from_slice(&0i16.to_be_bytes());
        return;
    }
    let mut r = Reader::new(g);
    let contours = r.u16().unwrap() as i16;
    let bbox = r.take(8).unwrap();
    if contours < 0 {
        out.extend_from_slice(&(-1i16).to_be_bytes());
        out.extend_from_slice(bbox);
        let mut flags;
        loop {
            flags = r.u16().unwrap();
            out.extend_from_slice(&flags.to_be_bytes());
            let mut len = 2 + if flags & 1 != 0 { 4 } else { 2 };
            len += if flags & 8 != 0 {
                2
            } else if flags & 0x40 != 0 {
                4
            } else if flags & 0x80 != 0 {
                8
            } else {
                0
            };
            out.extend_from_slice(r.take(len).unwrap());
            if flags & 0x20 == 0 {
                break;
            }
        }
        if flags & 0x100 != 0 {
            let n = usize::from(r.u16().unwrap());
            split_instructions(r.take(n).unwrap(), out, push, code);
        }
        return;
    }
    let ends: Vec<usize> = (0..contours).map(|_| usize::from(r.u16().unwrap())).collect();
    let points = ends.last().map_or(0, |e| e + 1);
    let ins_len = usize::from(r.u16().unwrap());
    let ins = r.take(ins_len).unwrap();
    let mut flags = Vec::with_capacity(points);
    while flags.len() < points {
        let f = r.u8().unwrap();
        flags.push(f);
        if f & 8 != 0 {
            for _ in 0..r.u8().unwrap() {
                flags.push(f);
            }
        }
    }
    let mut coord = |short: u8, same: u8| -> Vec<i32> {
        flags
            .iter()
            .map(|&f| {
                if f & short != 0 {
                    let v = i32::from(r.u8().unwrap());
                    if f & same != 0 { v } else { -v }
                } else if f & same != 0 {
                    0
                } else {
                    i32::from(r.u16().unwrap() as i16)
                }
            })
            .collect()
    };
    let xs = coord(2, 0x10);
    let ys = coord(4, 0x20);
    // Store the bounding box only when it isn't the points' own.
    let (mut x, mut y) = (0, 0);
    let (mut b, mut first) = ([0i32; 4], true);
    for k in 0..points {
        x += xs[k];
        y += ys[k];
        if first {
            b = [x, y, x, y];
            first = false;
        }
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    let stored: Vec<i32> = bbox.chunks(2).map(|c| i32::from(i16::from_be_bytes([c[0], c[1]]))).collect();
    if stored == b {
        out.extend_from_slice(&contours.to_be_bytes());
    } else {
        out.extend_from_slice(&0x7FFFi16.to_be_bytes());
        out.extend_from_slice(&contours.to_be_bytes());
        out.extend_from_slice(bbox);
    }
    for (k, &e) in ends.iter().enumerate() {
        u255(out, if k == 0 { e } else { e - ends[k - 1] });
    }
    let mut data = Vec::new();
    for k in 0..points {
        let (i, bytes) = triplet_for(xs[k], ys[k]);
        out.push(i | if flags[k] & 1 == 0 { 0x80 } else { 0 });
        data.extend(bytes);
    }
    out.extend(data);
    split_instructions(ins, out, push, code);
}
