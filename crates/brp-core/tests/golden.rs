//! Byte-exact fixtures, derived by hand from `docs/FORMAT.md`.
//!
//! These are the only tests that catch *silent format drift* — a change where encoder and decoder
//! still agree with each other but no longer agree with the specification. If one fails, either
//! the change was unintended, or the format version must be bumped and an ADR written. Do not
//! regenerate the expected bytes to make a failure go away.

use brp_core::{decode, encode, EncodeOptions, RawImage};

/// 2x2 RGBA. Green and alpha are constant; red and blue are coded.
///
/// Pixels (R, G, B, A):
///   (0,0) = 10, 100, 200, 255      (1,0) = 13, 100, 210, 255
///   (0,1) = 11, 100, 205, 255      (1,1) = 12, 100, 199, 255
///
/// Stage 1: G is constant 100, A is constant 255, so `channel_modes` is
/// `01 00 01 00` read from the low bits up = 0x44, with constants [100, 255] and no alias byte.
///
/// Stage 2 over the coded channels [0, 2]:
///   R: min 10, max 13  -> span  3 -> width code 2; residuals 0, 3, 1, 2
///   B: min 199, max 210 -> span 11 -> width code 4; residuals 1, 11, 6, 0
///
/// Body bits, MSB first, 48 of them so no padding:
///   00001010 0010 | 11000111 0100      channel headers
///   00 11 01 10                        R residuals
///   0001 1011 0110 0000                B residuals
#[test]
fn rgba_with_constant_channels() {
    let data = vec![
        10, 100, 200, 255, // (0,0)
        13, 100, 210, 255, // (1,0)
        11, 100, 205, 255, // (0,1)
        12, 100, 199, 255, // (1,1)
    ];
    let src = RawImage::new(2, 2, 4, data).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        // -- header, 27 bytes --
        b'B', b'R', b'P', 0x1A,
        2,                      // version
        0,                      // flags
        2, 0, 0, 0,             // width
        2, 0, 0, 0,             // height
        4,                      // channels
        8,                      // bit depth
        2, 0, 0, 0,             // block_w
        2, 0, 0, 0,             // block_h
        0x44,                   // channel modes: R coded, G constant, B coded, A constant
        100,                    // constant for G
        255,                    // constant for A
        // -- body, 6 bytes --
        0x0A, 0x2C, 0x74, 0x36, 0x1B, 0x60,
    ];

    let actual = encode(&src, &EncodeOptions::default()).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 33);
    assert_eq!(decode(&expected).unwrap(), src);
}

/// 2x2 grayscale, every sample 7. The fully degenerate case: stage 1 removes the only channel, so
/// there is no block stream at all and the file is a bare header.
#[test]
fn constant_grayscale_is_header_only() {
    let src = RawImage::new(2, 2, 1, vec![7; 4]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', 0x1A,
        2,
        0,
        2, 0, 0, 0,
        2, 0, 0, 0,
        1,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        0x01,                   // channel modes: channel 0 constant
        7,                      // its value
    ];

    let actual = encode(&src, &EncodeOptions::default()).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 26);
    assert_eq!(decode(&expected).unwrap(), src);
}

/// 2x2 grayscale carried in RGB: every pixel has R == G == B.
///
/// Stage 1 aliases G and B to channel 0 — both to the *root*, never forming a chain — so
/// `channel_modes` is `10 10 00` from the low bits up = 0x28 and the alias byte is 0x00
/// (two targets, both channel 0).
///
/// Stage 2 over coded channel [0]: min 10, max 40, span 30, width code 5; residuals 0, 10, 20, 30.
///
/// Body bits, 32 of them so no padding:
///   00001010 0101 | 00000 01010 10100 11110
#[test]
fn grayscale_carried_in_rgb() {
    let data: Vec<u8> = [10u8, 20, 30, 40].iter().flat_map(|&v| [v, v, v]).collect();
    let src = RawImage::new(2, 2, 3, data).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', 0x1A,
        2,
        0,
        2, 0, 0, 0,
        2, 0, 0, 0,
        3,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        0x28,                   // channel modes: R coded, G alias, B alias
        0x00,                   // alias targets: both channel 0
        // -- body, 4 bytes --
        0x0A, 0x50, 0x2A, 0x9E,
    ];

    let actual = encode(&src, &EncodeOptions::default()).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 30);
    assert_eq!(decode(&expected).unwrap(), src);
}

/// Locks the header field offsets in `FORMAT.md` section 3 against accidental reordering.
#[test]
fn header_field_offsets() {
    // Values chosen so no channel is constant and none aliases another.
    let data: Vec<u8> = (0..15u8).map(|i| i * 3 + (i % 3) * 7).collect();
    let src = RawImage::new(5, 1, 3, data).unwrap();
    let bytes = encode(
        &src,
        &EncodeOptions {
            block_size: Some((2, 4)),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(&bytes[0..4], &[b'B', b'R', b'P', 0x1A]);
    assert_eq!(bytes[4], 2);
    assert_eq!(bytes[5], 0);
    assert_eq!(&bytes[6..10], &5u32.to_le_bytes());
    assert_eq!(&bytes[10..14], &1u32.to_le_bytes());
    assert_eq!(bytes[14], 3);
    assert_eq!(bytes[15], 8);
    assert_eq!(&bytes[16..20], &2u32.to_le_bytes());
    assert_eq!(&bytes[20..24], &4u32.to_le_bytes());
    assert_eq!(bytes[24], 0x00, "all three channels coded");
}
