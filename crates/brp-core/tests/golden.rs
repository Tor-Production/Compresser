//! Byte-exact fixtures, derived by hand from `docs/FORMAT.md`.
//!
//! These are the only tests that catch *silent format drift* — a change where encoder and decoder
//! still agree with each other but no longer agree with the specification. If one fails, either
//! the change was unintended, or the format version must be bumped and an ADR written. Do not
//! regenerate the expected bytes to make a failure go away.

use brp_core::{decode, encode, EncodeOptions, RawImage};

/// 2x2 RGBA, constant alpha, one whole-image block.
///
/// Pixels (R, G, B, A):
///   (0,0) = 10, 100, 200, 255      (1,0) = 13, 100, 210, 255
///   (0,1) = 11, 100, 205, 255      (1,1) = 12, 100, 199, 255
///
/// Alpha is constant, so ALPHA_CONSTANT is set and the channel is dropped:
///   R: min 10, max 13 -> span  3 -> width code 2; residuals 0, 3, 1, 2
///   G: min 100, max 100 -> span 0 -> width code 0; no payload
///   B: min 199, max 210 -> span 11 -> width code 4; residuals 1, 11, 6, 0
///
/// Body bits, MSB first:
///   00001010 0010 | 01100100 0000 | 11000111 0100      channel headers
///   00 11 01 10                                        R residuals
///   0001 1011 0110 0000                                B residuals
///   0000                                               padding to 64 bits
#[test]
fn rgba_constant_alpha() {
    let data = vec![
        10, 100, 200, 255, // (0,0)
        13, 100, 210, 255, // (1,0)
        11, 100, 205, 255, // (0,1)
        12, 100, 199, 255, // (1,1)
    ];
    let src = RawImage::new(2, 2, 4, data).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        // -- header, 25 bytes --
        b'B', b'R', b'P', b'1',
        1,                      // version
        0b0000_0001,            // flags: ALPHA_CONSTANT
        2, 0, 0, 0,             // width
        2, 0, 0, 0,             // height
        4,                      // channels
        8,                      // bit depth
        2, 0, 0, 0,             // block_w
        2, 0, 0, 0,             // block_h
        255,                    // alpha_const
        // -- body, 8 bytes --
        0x0A, 0x26, 0x40, 0xC7, 0x43, 0x61, 0xB6, 0x00,
    ];

    let actual = encode(&src, &EncodeOptions::default()).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 33);
    assert_eq!(decode(&expected).unwrap(), src);
}

/// 2x2 grayscale, every sample 7. The degenerate case: one base, a zero width code, no payload.
///
/// Body bits: 00000111 0000 | 0000 padding -> 0x07, 0x00
#[test]
fn constant_grayscale() {
    let src = RawImage::new(2, 2, 1, vec![7; 4]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', b'1',
        1,
        0,                      // flags: no alpha channel to elide
        2, 0, 0, 0,
        2, 0, 0, 0,
        1,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        0x07, 0x00,             // base 7, width code 0, padding
    ];

    let actual = encode(&src, &EncodeOptions::default()).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 26);
    assert_eq!(decode(&expected).unwrap(), src);
}

/// Locks the header field offsets in `FORMAT.md` section 3 against accidental reordering.
#[test]
fn header_field_offsets() {
    let src = RawImage::new(3, 5, 3, vec![0; 45]).unwrap();
    let bytes = encode(
        &src,
        &EncodeOptions {
            block_size: Some((2, 4)),
            alpha_opt: true,
        },
    )
    .unwrap();

    assert_eq!(&bytes[0..4], b"BRP1");
    assert_eq!(bytes[4], 1);
    assert_eq!(bytes[5], 0);
    assert_eq!(&bytes[6..10], &3u32.to_le_bytes());
    assert_eq!(&bytes[10..14], &5u32.to_le_bytes());
    assert_eq!(bytes[14], 3);
    assert_eq!(bytes[15], 8);
    assert_eq!(&bytes[16..20], &2u32.to_le_bytes());
    assert_eq!(&bytes[20..24], &4u32.to_le_bytes());
}
