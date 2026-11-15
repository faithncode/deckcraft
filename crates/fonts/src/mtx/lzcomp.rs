//! LZCOMP, the MicroType Express compressor: LZ77-style literals, one-byte copies and copy items
//! over a history that starts with fixed preload bytes, every symbol coded with one of three
//! adaptive Huffman trees, and an optional run-length layer underneath (W3C MTX submission,
//! section 3 and appendices B and C).

use super::MAX_OUTPUT;

/// Bytes placed in the history before the data, so early bytes can be copy items too:
/// `(k, j)` for k in 0..32, j in 0..96, then every byte value four times.
pub(super) const PRELOAD: usize = 2 * 32 * 96 + 4 * 256;
/// Copy items farther back than this are one byte longer than their coded length.
pub(super) const MAX_2BYTE_DIST: usize = 512;
/// Bits per length symbol (two value bits and a continue bit) and per distance symbol.
pub(super) const LEN_WIDTH: u32 = 3;
pub(super) const DIST_WIDTH: u32 = 3;

pub(super) fn preload() -> Vec<u8> {
    let mut v = Vec::with_capacity(PRELOAD);
    for k in 0..32u8 {
        for j in 0..96u8 {
            v.push(k);
            v.push(j);
        }
    }
    for b in 0..=255u8 {
        v.extend_from_slice(&[b; 4]);
    }
    v
}

/// Number of 3-bit distance groups needed to reach `len` bytes back (`SetDistRange`).
pub(super) fn dist_ranges(len: usize) -> u32 {
    let mut n = 1;
    while n < 10 && (1usize << (DIST_WIDTH * n)) < len {
        n += 1;
    }
    n
}

/// MSB-first bit reader.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    cur: u8,
    left: u8,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, cur: 0, left: 0 }
    }

    fn bit(&mut self) -> Option<bool> {
        if self.left == 0 {
            self.cur = *self.data.get(self.pos)?;
            self.pos += 1;
            self.left = 8;
        }
        self.left -= 1;
        Some((self.cur >> self.left) & 1 == 1)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.bit()?);
        }
        Some(v)
    }
}

const ROOT: usize = 1;

#[derive(Clone, Copy)]
struct Node {
    up: usize,
    left: usize,
    right: usize,
    /// The symbol of a leaf; `None` for an internal node.
    code: Option<u16>,
    weight: u64,
}

/// An adaptive Huffman tree over symbols `0..range`, kept in sibling order: weights never
/// increase with the node index. Encoder and decoder start from the same tree and update it the
/// same way after every symbol.
pub(super) struct AHuff {
    tree: Vec<Node>,
    index: Vec<usize>,
}

impl AHuff {
    /// `range` is 2..=512 (the callers use 8 and 264..=339).
    pub(super) fn new(range: usize) -> Option<Self> {
        if !(2..=512).contains(&range) {
            return None;
        }
        let n = 2 * range;
        let mut tree = vec![Node { up: 0, left: 0, right: 0, code: None, weight: 1 }; n];
        for (i, node) in tree.iter_mut().enumerate() {
            node.up = i / 2;
            if i < range {
                node.left = 2 * i;
                node.right = 2 * i + 1;
            } else {
                node.code = u16::try_from(i - range).ok();
            }
        }
        // Internal weights are the sums of their children; children always sit at higher
        // indices, so one backwards pass fills them.
        for i in (ROOT..range).rev() {
            let w = tree.get(2 * i)?.weight + tree.get(2 * i + 1)?.weight;
            tree.get_mut(i)?.weight = w;
        }
        // Node 0 is unused; a weight no real node reaches stops the sibling scan there.
        tree.get_mut(0)?.weight = u64::MAX;
        let mut t = AHuff { tree, index: (range..n).collect() };
        if range > 256 {
            // The symbol tree: favour the two shortest copy items and the one-byte copies.
            t.bump(256)?;
            t.bump(257)?;
            for _ in 0..12 {
                t.bump(range - 3)?;
            }
            for _ in 0..6 {
                t.bump(range - 2)?;
            }
        } else {
            for _ in 0..2 {
                for s in 0..range {
                    t.bump(s)?;
                }
            }
        }
        Some(t)
    }

    fn bump(&mut self, symbol: usize) -> Option<()> {
        let a = *self.index.get(symbol)?;
        self.update(a)
    }

    fn node(&self, i: usize) -> Option<&Node> {
        self.tree.get(i)
    }

    /// Adds one to the weight of node `a` and its ancestors, swapping each with the first node
    /// of equal weight so the sibling order holds.
    fn update(&mut self, mut a: usize) -> Option<()> {
        // Each step moves to a parent (a smaller index), so this ends within tree.len() steps.
        for _ in 0..self.tree.len() {
            if a == ROOT {
                self.tree.get_mut(ROOT)?.weight += 1;
                return Some(());
            }
            let w = self.node(a)?.weight;
            let mut b = a.checked_sub(1)?;
            if self.node(b)?.weight == w {
                while b > 0 && self.node(b - 1)?.weight == w {
                    b -= 1;
                }
                if b > ROOT {
                    self.swap(a, b)?;
                    a = b;
                }
            }
            let node = self.tree.get_mut(a)?;
            node.weight = w + 1;
            a = node.up;
        }
        None
    }

    fn swap(&mut self, a: usize, b: usize) -> Option<()> {
        let (upa, upb) = (self.node(a)?.up, self.node(b)?.up);
        let (na, nb) = (*self.node(a)?, *self.node(b)?);
        *self.tree.get_mut(a)? = Node { up: upa, ..nb };
        *self.tree.get_mut(b)? = Node { up: upb, ..na };
        for x in [a, b] {
            let n = *self.node(x)?;
            match n.code {
                None => {
                    self.tree.get_mut(n.left)?.up = x;
                    self.tree.get_mut(n.right)?.up = x;
                }
                Some(c) => *self.index.get_mut(usize::from(c))? = x,
            }
        }
        Some(())
    }

    fn read(&mut self, bits: &mut Bits) -> Option<u16> {
        let mut a = ROOT;
        // A path is at most one step per node.
        for _ in 0..self.tree.len() {
            let n = self.node(a)?;
            a = if bits.bit()? { n.right } else { n.left };
            if let Some(c) = self.node(a)?.code {
                self.update(a)?;
                return Some(c);
            }
        }
        None
    }

    /// The path bits (root first) for `symbol`, then the update. Test encoder only.
    #[cfg(test)]
    pub(super) fn write(&mut self, symbol: usize, out: &mut Vec<bool>) -> Option<()> {
        let leaf = *self.index.get(symbol)?;
        let mut path = Vec::new();
        let mut a = leaf;
        while a != ROOT {
            let up = self.node(a)?.up;
            path.push(self.node(up)?.right == a);
            a = up;
        }
        out.extend(path.iter().rev());
        self.update(leaf)
    }
}

/// One LZCOMP block back to its bytes.
pub(super) fn decompress(block: &[u8]) -> Option<Vec<u8>> {
    let mut bits = Bits::new(block);
    let run_length = bits.bit()?;
    let mut dist_tree = AHuff::new(1 << DIST_WIDTH)?;
    let mut len_tree = AHuff::new(1 << LEN_WIDTH)?;
    let out_len = usize::try_from(bits.bits(24)?).ok()?;
    let ranges = dist_ranges(out_len);
    let dup2 = 256 + (1usize << LEN_WIDTH) * usize::try_from(ranges).ok()?;
    let mut sym_tree = AHuff::new(dup2 + 3)?;

    let mut hist = preload();
    // Every symbol takes at least one bit, but copies can be long: grow as needed.
    hist.reserve(out_len.min(block.len().saturating_mul(8)));
    let end = PRELOAD.checked_add(out_len)?;
    while hist.len() < end {
        let sym = usize::from(sym_tree.read(&mut bits)?);
        if sym < 256 {
            hist.push(u8::try_from(sym).ok()?);
        } else if (dup2..dup2 + 3).contains(&sym) {
            let back = 2 * (sym - dup2 + 1);
            let v = *hist.get(hist.len().checked_sub(back)?)?;
            hist.push(v);
        } else {
            let first = sym - 256;
            let n_dist = first / (1 << LEN_WIDTH) + 1;
            if n_dist > usize::try_from(ranges).ok()? {
                return None;
            }
            // Length: 2-bit groups, most significant first, while the continue bit is set.
            let mut sym_bits = first % (1 << LEN_WIDTH);
            let mut length = 0usize;
            let mut groups = 0;
            loop {
                length = (length << 2) | (sym_bits & 3);
                groups += 1;
                if sym_bits & 4 == 0 {
                    break;
                }
                if groups >= 14 {
                    return None;
                }
                sym_bits = usize::from(len_tree.read(&mut bits)?);
            }
            length += 2;
            let mut distance = 0usize;
            for _ in 0..n_dist {
                distance = (distance << DIST_WIDTH) | usize::from(dist_tree.read(&mut bits)?);
            }
            distance += 1;
            if distance >= MAX_2BYTE_DIST {
                length += 1;
            }
            if hist.len().checked_add(length)? > end {
                return None;
            }
            // The distance is to the copied string's last byte.
            let start = hist.len().checked_sub(distance)?.checked_sub(length - 1)?;
            for j in 0..length {
                let v = *hist.get(start + j)?;
                hist.push(v);
            }
        }
    }
    let out = hist.split_off(PRELOAD);
    if run_length { unrun(&out) } else { Some(out) }
}

/// The run-length layer: the first byte is the escape; `escape, 0` is the escape byte itself and
/// `escape, n, b` is `n` copies of `b`.
fn unrun(data: &[u8]) -> Option<Vec<u8>> {
    let (&escape, mut rest) = data.split_first()?;
    let mut out = Vec::with_capacity(data.len());
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b != escape {
            out.push(b);
        } else {
            let (&n, tail) = rest.split_first()?;
            rest = tail;
            if n == 0 {
                out.push(escape);
            } else {
                let (&v, tail) = rest.split_first()?;
                rest = tail;
                out.resize(out.len() + usize::from(n), v);
            }
        }
        if out.len() > MAX_OUTPUT {
            return None;
        }
    }
    Some(out)
}
