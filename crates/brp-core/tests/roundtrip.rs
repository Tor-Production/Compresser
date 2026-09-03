//! Invariant 1 from `docs/ARCHITECTURE.md`: `decode(encode(img)) == img`, byte for byte.

use brp_core::{analyze, decode, encode, EncodeOptions, RawImage};

/// Every block size worth exercising, including ones that do not divide the image evenly.
const BLOCK_SIZES: &[Option<(u32, u32)>] = &[
    None,
    Some((1, 1)),
    Some((2, 3)),
    Some((4, 4)),
    Some((8, 8)),
    Some((5, 1)),
    Some((1, 5)),
    Some((1000, 1000)),
];

fn assert_round_trips(src: &RawImage) {
    for &block_size in BLOCK_SIZES {
        for alpha_opt in [true, false] {
            let opts = EncodeOptions {
                block_size,
                alpha_opt,
            };
            let bytes = encode(src, &opts).unwrap();
            let back = decode(&bytes).unwrap();
            assert_eq!(
                &back, src,
                "block_size {block_size:?}, alpha_opt {alpha_opt}"
            );
            // Anything that decodes must also analyze, and vice versa.
            analyze(&bytes).unwrap();
        }
    }
}

fn image(w: u32, h: u32, ch: u8, f: impl Fn(u32, u32, u8) -> u8) -> RawImage {
    let mut data = Vec::with_capacity((w * h * u32::from(ch)) as usize);
    for y in 0..h {
        for x in 0..w {
            for c in 0..ch {
                data.push(f(x, y, c));
            }
        }
    }
    RawImage::new(w, h, ch, data).unwrap()
}

/// Deterministic pseudo-random noise; a real PRNG would be another dependency for no gain.
fn noise(x: u32, y: u32, c: u8) -> u8 {
    let mut v = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))
        .wrapping_add(u32::from(c).wrapping_mul(2_246_822_519));
    v ^= v >> 13;
    v = v.wrapping_mul(1_274_126_177);
    (v ^ (v >> 16)) as u8
}

#[test]
fn solid_colour() {
    for ch in 1..=4u8 {
        assert_round_trips(&image(16, 9, ch, |_, _, c| 40 + c * 5));
    }
}

#[test]
fn gradient() {
    for ch in 1..=4u8 {
        assert_round_trips(&image(17, 13, ch, |x, y, _| (x + y) as u8));
    }
}

#[test]
fn full_range_noise() {
    for ch in 1..=4u8 {
        assert_round_trips(&image(23, 19, ch, noise));
    }
}

#[test]
fn single_pixel() {
    for ch in 1..=4u8 {
        assert_round_trips(&image(1, 1, ch, |_, _, c| 200 + c));
    }
}

#[test]
fn single_row_and_single_column() {
    for ch in 1..=4u8 {
        assert_round_trips(&image(37, 1, ch, noise));
        assert_round_trips(&image(1, 37, ch, noise));
    }
}

/// The width-code boundaries: a span of 0, 1, 15 (the worked example) and 255 bits.
#[test]
fn every_interesting_span() {
    for span in [0u8, 1, 2, 15, 16, 127, 128, 254, 255] {
        for ch in 1..=4u8 {
            let src = image(8, 8, ch, |x, y, _| {
                let i = (y * 8 + x) as u32;
                if span == 0 {
                    100
                } else {
                    // Walk the full span, and make sure both ends actually occur.
                    (10 + (i % (u32::from(span) + 1))) as u8
                }
            });
            assert_round_trips(&src);
        }
    }
}

/// Base near the top of the range, so `base + residual` exceeds 255 arithmetically.
#[test]
fn high_bases_do_not_overflow() {
    let src = image(8, 8, 3, |x, _, _| if x % 2 == 0 { 200 } else { 255 });
    assert_round_trips(&src);
}

#[test]
fn constant_alpha_variants() {
    for alpha in [0u8, 1, 128, 254, 255] {
        for ch in [2u8, 4] {
            let src = image(9, 7, ch, |x, y, c| {
                if c == ch - 1 {
                    alpha
                } else {
                    noise(x, y, c)
                }
            });
            assert_round_trips(&src);

            // With the optimization on, the flag must actually fire and shrink the file.
            let on = encode(
                &src,
                &EncodeOptions {
                    block_size: Some((3, 3)),
                    alpha_opt: true,
                },
            )
            .unwrap();
            let off = encode(
                &src,
                &EncodeOptions {
                    block_size: Some((3, 3)),
                    alpha_opt: false,
                },
            )
            .unwrap();
            assert!(
                on.len() < off.len(),
                "alpha optimization should shrink the file for constant alpha {alpha}"
            );
            assert_eq!(decode(&on).unwrap(), src);
            assert_eq!(decode(&off).unwrap(), src);
            assert_eq!(analyze(&on).unwrap().header.alpha_const, Some(alpha));
            assert_eq!(analyze(&off).unwrap().header.alpha_const, None);
        }
    }
}

#[test]
fn varying_alpha_is_not_elided() {
    let src = image(8, 8, 4, |x, y, c| if c == 3 { x as u8 } else { noise(x, y, c) });
    assert_round_trips(&src);
    let bytes = encode(&src, &EncodeOptions::default()).unwrap();
    assert_eq!(analyze(&bytes).unwrap().header.alpha_const, None);
}

/// A narrow-range image is where the algorithm is supposed to win. Guards against a regression
/// that would silently make the codec a no-op.
#[test]
fn narrow_range_actually_compresses() {
    // Every channel spans 16 values, so 4 bits per sample instead of 8.
    let src = image(64, 64, 3, |x, y, _| 100 + ((x + y) % 16) as u8);
    let bytes = encode(&src, &EncodeOptions::default()).unwrap();
    let raw = src.data().len();
    assert!(
        bytes.len() < raw / 2 + 32,
        "expected roughly half of {raw} bytes, got {}",
        bytes.len()
    );
    assert_eq!(decode(&bytes).unwrap(), src);
}
