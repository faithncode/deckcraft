use super::encode::{self, Options};
use super::lzcomp;
use crate::{FontDb, embedded_sfnt};
use skrifa::MetadataProvider;
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::prelude::{LocationRef, Size};
use skrifa::raw::TableProvider;

/// The font the fixtures are made from: Ubuntu Light, which the crate already ships as its
/// last-resort face (Ubuntu Font Licence 1.0; see ATTRIBUTION.md). Nothing new is committed:
/// each test compresses it with the test encoder.
const FONT: &[u8] = epaint_default_fonts::UBUNTU_LIGHT;

const MTX: u32 = 0x4;
const XOR: u32 = 0x1000_0000;

#[derive(Default)]
struct Pen(Vec<(char, i32, i32)>);

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(('M', x as i32, y as i32));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(('L', x as i32, y as i32));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push(('Q', cx as i32, cy as i32));
        self.0.push(('q', x as i32, y as i32));
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, x: f32, y: f32) {
        self.0.push(('C', x as i32, y as i32));
    }
    fn close(&mut self) {
        self.0.push(('Z', 0, 0));
    }
}

/// Every glyph's unhinted outline and advance.
fn outlines(font: &[u8]) -> Vec<(Vec<(char, i32, i32)>, f32)> {
    let f = skrifa::FontRef::new(font).unwrap();
    let metrics = f.glyph_metrics(Size::unscaled(), LocationRef::default());
    let glyphs = f.outline_glyphs();
    (0..f.maxp().unwrap().num_glyphs())
        .map(|g| {
            let id = skrifa::GlyphId::new(u32::from(g));
            let mut pen = Pen::default();
            if let Some(o) = glyphs.get(id) {
                o.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).unwrap();
            }
            (pen.0, metrics.advance_width(id).unwrap_or(0.0))
        })
        .collect()
}

fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    skrifa::FontRef::new(font).unwrap().table_data(skrifa::raw::types::Tag::new(tag)).map(|d| d.as_bytes())
}

#[test]
fn lzcomp_round_trips_every_construct() {
    let text: Vec<u8> = b"abababab hello hello hello world \x00\x00\x00\x00\x00\x00\x00\x00\x00 xyzxyz"
        .iter()
        .copied()
        .cycle()
        .take(5000)
        .chain((0..=255u8).cycle().take(3000))
        .collect();
    for opt in [
        Options { run_length: false, copies: true },
        Options { run_length: true, copies: true },
        Options { run_length: false, copies: false },
        Options { run_length: true, copies: false },
    ] {
        for data in [&text[..], b"", b"a", &[7u8; 2000]] {
            let packed = encode::compress(data, opt);
            assert_eq!(lzcomp::decompress(&packed).as_deref(), Some(data), "run_length {} copies {}", opt.run_length, opt.copies);
        }
    }
    // Copies reach into the preload and far back (distances of 512 and more).
    let mut far = vec![0u8; 0];
    far.extend((0..3000u32).map(|i| (i * 7 % 251) as u8));
    far.extend_from_within(..600);
    let opt = Options { run_length: false, copies: true };
    assert_eq!(lzcomp::decompress(&encode::compress(&far, opt)).as_deref(), Some(&far[..]));
    assert!(encode::compress(&far, opt).len() < far.len());
}

#[test]
fn an_mtx_font_decodes_to_the_same_glyphs() {
    // The fixture covers both CTF glyph kinds.
    let f = skrifa::FontRef::new(FONT).unwrap();
    let (glyf, loca) = (f.glyf().unwrap(), f.loca(None).unwrap());
    let kinds: Vec<_> =
        (0..loca.len().saturating_sub(1) as u32).filter_map(|g| loca.get_glyf(skrifa::GlyphId::new(g), &glyf).ok().flatten()).collect();
    assert!(kinds.iter().any(|g| matches!(g, skrifa::raw::tables::glyf::Glyph::Composite(_))));
    assert!(kinds.iter().any(|g| matches!(g, skrifa::raw::tables::glyf::Glyph::Simple(_))));
    let original = outlines(FONT);
    for opt in [Options { run_length: false, copies: true }, Options { run_length: true, copies: true }] {
        let mtx = encode::compress_font(FONT, opt);
        assert!(mtx.len() < FONT.len(), "the fixture is really compressed ({} of {})", mtx.len(), FONT.len());
        let font = super::decode(&mtx).expect("the MTX font decodes");
        assert_eq!(outlines(&font), original, "every glyph outline and advance survives");
        assert_eq!(table(&font, b"cvt "), table(FONT, b"cvt "), "cvt survives");
        assert_eq!(table(&font, b"name"), table(FONT, b"name"));
        assert_eq!(table(&font, b"cmap"), table(FONT, b"cmap"));
    }
}

#[test]
fn an_eot_mtx_part_loads_and_draws_its_family() {
    let mtx = encode::compress_font(FONT, Options { run_length: false, copies: true });
    for flags in [MTX, MTX | XOR] {
        let part = encode::eot(&mtx, flags, "Ubuntu Light");
        let sfnt = embedded_sfnt(&part).expect("an EOT+MTX part yields a font");
        assert_eq!(outlines(&sfnt), outlines(FONT));
        // Ubuntu Light is already the last-resort face, so register it under a deck's typeface.
        let db = FontDb::with_font_dirs(Vec::new());
        assert_eq!(db.add_embedded_font(sfnt.into_owned(), "Sermon Display", "Regular"), 1);
        let face = db.face("Sermon Display", "Regular");
        assert_eq!(face.family, "Sermon Display");
        let g = face.glyph_for('a');
        assert_ne!(g, 0);
        assert!(!db.outline(&face, g).elements().is_empty(), "'a' draws");
    }
    // XOR alone (an uncompressed, obfuscated font).
    let part = encode::eot(FONT, XOR, "Ubuntu Light");
    assert_eq!(embedded_sfnt(&part).as_deref(), Some(FONT));
}

#[test]
fn a_renamed_embedded_face_draws_under_the_document_typeface() {
    let font = super::decode(&encode::compress_font(FONT, Options { run_length: false, copies: true })).unwrap();
    let db = FontDb::with_font_dirs(Vec::new());
    assert_eq!(db.add_embedded_font(font.clone(), "Deck Display", "Bold"), 1);
    assert!(db.has_family("Deck Display"));
    let face = db.face("Deck Display", "Bold");
    assert_eq!((face.family.as_str(), face.style.as_str()), ("Deck Display", "Bold"));
    assert!(face.weight >= 700.0);
    // A face whose own family (or Windows family) is the typeface keeps its names; one already
    // loaded (Ubuntu Light is the last-resort face) isn't added twice.
    let db = FontDb::with_font_dirs(Vec::new());
    let ubuntu = db.families().into_iter().find(|f| f.starts_with("Ubuntu")).expect("the last-resort face");
    assert_eq!(db.add_embedded_font(font, &ubuntu, "Regular"), 0);
    assert!(!db.has_family("Deck Display"));
}

/// Truncated, bit-flipped and nonsense MTX data returns none (or some font) without panicking,
/// and a hostile header can't make the decoder allocate without bound.
#[test]
fn corrupt_mtx_input_never_panics() {
    let mtx = encode::compress_font(FONT, Options { run_length: true, copies: true });
    for n in (0..mtx.len()).step_by(mtx.len() / 97 + 1).chain([1, 9, 10, 11, mtx.len() - 1]) {
        assert!(super::decode(&mtx[..n]).is_none(), "truncated to {n}");
        let _ = embedded_sfnt(&encode::eot(&mtx[..n], MTX, "x"));
    }
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..300 {
        let mut bad = mtx.clone();
        for _ in 0..1 + next() % 8 {
            let i = (next() % bad.len() as u64) as usize;
            bad[i] ^= 1 << (next() % 8);
        }
        let _ = super::decode(&bad);
    }
    // Each block alone, corrupted.
    for _ in 0..300 {
        let len = (next() % 64) as usize;
        let junk: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let _ = lzcomp::decompress(&junk);
        let mut hdr = vec![3, 0, 0, 0, 0, 0, 10, 0, 0, 10];
        hdr.extend_from_slice(&junk);
        let _ = super::decode(&hdr);
    }
    // A block claiming 16 MiB of output from four bytes ends at the input, not in a huge buffer.
    assert!(lzcomp::decompress(&[0x7F, 0xFF, 0xFF, 0xF0]).is_none());
    // Header offsets out of order or past the end.
    assert!(super::decode(&[3, 0, 0, 0, 0, 0, 20, 0, 0, 12, 0, 0]).is_none());
    assert!(super::decode(&[3, 0, 0, 0, 0, 0, 12, 0, 0, 200, 0, 0]).is_none());
    assert!(super::decode(&[9; 40]).is_none());
}
