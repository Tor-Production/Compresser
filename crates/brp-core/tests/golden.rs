//! Byte-exact fixtures, derived by hand from `docs/FORMAT.md`.
//!
//! These are the only tests that catch *silent format drift* — a change where encoder and decoder
//! still agree with each other but no longer agree with the specification. If one fails, either
//! the change was unintended, or the format version must be bumped and an ADR written. Do not
//! regenerate the expected bytes to make a failure go away.
//!
//! Every fixture pins its filter and coder choices explicitly. Both default to a mode that tries
//! more than one encoding and keeps the smaller, so leaving them to the default would make these
//! fixtures depend on which way that comparison happened to fall.

use brp_core::{
    decode, encode, ChannelOptions, CoderChoice, EncodeOptions, FilterChoice, RawImage,
};

/// No prediction, fixed-width block packing: the plainest encoding the format can produce.
fn plain(block: Option<(u32, u32)>) -> EncodeOptions {
    EncodeOptions {
        block_size: block,
        channels: ChannelOptions::default(),
        filter: FilterChoice::Off,
        coder: CoderChoice::Fixed,
    }
}

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
        // -- header, 29 bytes --
        b'B', b'R', b'P', 0x1A,
        4,                      // version
        0,                      // flags
        2, 0, 0, 0,             // width
        2, 0, 0, 0,             // height
        4,                      // channels
        8,                      // bit depth
        2, 0, 0, 0,             // block_w
        2, 0, 0, 0,             // block_h
        0,                      // filter mode: none
        0,                      // block coder: fixed width
        0x44,                   // channel modes: R coded, G constant, B coded, A constant
        100,                    // constant for G
        255,                    // constant for A
        // -- body, 6 bytes --
        0x0A, 0x2C, 0x74, 0x36, 0x1B, 0x60,
    ];

    let actual = encode(&src, &plain(None)).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 35);
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
        4,
        0,
        2, 0, 0, 0,
        2, 0, 0, 0,
        1,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        0,                      // filter mode: nothing to predict
        0,                      // block coder: nothing to code
        0x01,                   // channel modes: channel 0 constant
        7,                      // its value
    ];

    // Every setting must reach the same file: with no coded channel there is nothing to choose.
    for opts in [plain(None), EncodeOptions::default()] {
        let actual = encode(&src, &opts).unwrap();
        assert_eq!(
            actual, expected,
            "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
        );
    }
    assert_eq!(expected.len(), 28);
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
        4,
        0,
        2, 0, 0, 0,
        2, 0, 0, 0,
        3,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        0,                      // filter mode: none
        0,                      // block coder: fixed width
        0x28,                   // channel modes: R coded, G alias, B alias
        0x00,                   // alias targets: both channel 0
        // -- body, 4 bytes --
        0x0A, 0x50, 0x2A, 0x9E,
    ];

    let actual = encode(&src, &plain(None)).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 32);
    assert_eq!(decode(&expected).unwrap(), src);
}

/// 2x2 grayscale with prediction on, pinning the filter-kind field and the zigzag mapping.
///
/// Samples: (0,0) = 10, (1,0) = 12, (0,1) = 14, (1,1) = 16.
///
/// Row 0 — absent neighbours read as zero. Sum of absolute residuals per predictor:
///   None 22, Sub 12, Up 22, Average 17, Paeth 12. Sub wins on the tie, being first.
///   Residuals 10, 2 -> zigzag 20, 4.
/// Row 1 — None 30, Sub 16, Up 8, Average 12, Paeth 6. Paeth wins.
///   Residuals 4, 2 -> zigzag 8, 4.
///
/// The block then packs the residual plane 20, 4, 8, 4: min 4, max 20, span 16, width code 5.
///
/// Body bits, 38 of them plus 2 of padding:
///   001 100                    filter kinds: Sub, Paeth
///   00000100 0101              base 4, width code 5
///   10000 00000 00100 00000    residuals 16, 0, 4, 0
///
/// Encoded with `FilterChoice::On`, not the default: on a 2x2 image the unpredicted file is
/// smaller, because two rows of filter codes cost more than they save.
#[test]
fn prediction_and_zigzag() {
    let src = RawImage::new(2, 2, 1, vec![10, 12, 14, 16]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', 0x1A,
        4,
        0,
        2, 0, 0, 0,
        2, 0, 0, 0,
        1,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        1,                      // filter mode: adaptive
        0,                      // block coder: fixed width
        0x00,                   // channel modes: coded
        // -- body, 5 bytes --
        0x30, 0x11, 0x60, 0x02, 0x00,
    ];

    let opts = EncodeOptions {
        filter: FilterChoice::On,
        ..plain(None)
    };
    let actual = encode(&src, &opts).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 32);
    assert_eq!(decode(&expected).unwrap(), src);

    // And the claim in the doc comment above, so it cannot rot.
    assert_eq!(encode(&src, &plain(None)).unwrap().len(), 30);
}

/// 2x2 grayscale with Golomb-Rice packing, pinning the mode field and the unary codes.
///
/// Samples 10, 12, 14, 16 give base 10 and residuals 0, 2, 4, 6. Payload bits per parameter:
/// k=0 costs 16, k=1 costs 14, k=2 costs 14, k=3 costs 16. The first minimum wins, so k = 1 and
/// the mode field is `k + 1` = 2.
///
/// Codes at k = 1 — a unary quotient, a terminating zero, then one low bit:
///   0 -> q 0 -> `0` `0`            2 bits
///   2 -> q 1 -> `1` `0` `0`        3 bits
///   4 -> q 2 -> `11` `0` `0`       4 bits
///   6 -> q 3 -> `111` `0` `0`      5 bits
///
/// Body bits, 26 of them plus 6 of padding:
///   00001010 0010 | 00 100 1100 11100
///
/// Encoded with `CoderChoice::Rice` rather than the default: on four samples fixed width is
/// smaller (30 bytes against 31), so `Auto` would pick it.
#[test]
fn rice_coded_block() {
    let src = RawImage::new(2, 2, 1, vec![10, 12, 14, 16]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', 0x1A,
        4,
        0,
        2, 0, 0, 0,
        2, 0, 0, 0,
        1,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        0,                      // filter mode: none
        1,                      // block coder: Golomb-Rice
        0x00,                   // channel modes: coded
        // -- body, 4 bytes --
        0x0A, 0x22, 0x67, 0x00,
    ];

    let opts = EncodeOptions {
        coder: CoderChoice::Rice,
        ..plain(None)
    };
    let actual = encode(&src, &opts).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 31);
    assert_eq!(decode(&expected).unwrap(), src);

    // The claim above: fixed width wins on an image this small, and `Auto` finds that.
    assert_eq!(encode(&src, &plain(None)).unwrap().len(), 30);
    let auto = EncodeOptions {
        coder: CoderChoice::Auto,
        ..plain(None)
    };
    assert_eq!(encode(&src, &auto).unwrap().len(), 30);
}

/// A block whose residuals are all zero uses Rice mode 0 and emits no payload — the case plain
/// Rice would charge one bit per sample for.
#[test]
fn rice_constant_block_has_no_payload() {
    // Two channels so stage 1 cannot elide everything: red is flat per block, blue varies.
    let mut data = Vec::new();
    for y in 0..4u8 {
        for x in 0..4u8 {
            data.extend_from_slice(&[if y < 2 { 30 } else { 90 }, 0, x * 9]);
        }
    }
    let src = RawImage::new(4, 4, 3, data).unwrap();

    let opts = EncodeOptions {
        block_size: Some((2, 2)),
        coder: CoderChoice::Rice,
        ..plain(None)
    };
    let bytes = encode(&src, &opts).unwrap();
    assert_eq!(bytes[25], 1, "block coder is Rice");
    assert_eq!(decode(&bytes).unwrap(), src);

    let a = brp_core::analyze(&bytes).unwrap();
    // Red is constant within every 2x2 block, so every one of its blocks takes mode 0.
    assert_eq!(a.width_code_histogram[0][0], 4, "four blocks, all mode 0");
}

/// Locks the header field offsets in `FORMAT.md` section 3 against accidental reordering.
#[test]
fn header_field_offsets() {
    // Values chosen so no channel is constant and none aliases another.
    let data: Vec<u8> = (0..15u8).map(|i| i * 3 + (i % 3) * 7).collect();
    let src = RawImage::new(5, 1, 3, data).unwrap();
    let bytes = encode(&src, &plain(Some((2, 4)))).unwrap();

    assert_eq!(&bytes[0..4], &[b'B', b'R', b'P', 0x1A]);
    assert_eq!(bytes[4], 4);
    assert_eq!(bytes[5], 0);
    assert_eq!(&bytes[6..10], &5u32.to_le_bytes());
    assert_eq!(&bytes[10..14], &1u32.to_le_bytes());
    assert_eq!(bytes[14], 3);
    assert_eq!(bytes[15], 8);
    assert_eq!(&bytes[16..20], &2u32.to_le_bytes());
    assert_eq!(&bytes[20..24], &4u32.to_le_bytes());
    assert_eq!(bytes[24], 0, "filter mode");
    assert_eq!(bytes[25], 0, "block coder");
    assert_eq!(bytes[26], 0x00, "all three channels coded");
}
