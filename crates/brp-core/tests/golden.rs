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
    decode, encode, ChannelOptions, CoderChoice, EncodeOptions, FilterChoice, RawImage, RemapChoice,
};

/// No prediction, fixed-width block packing: the plainest encoding the format can produce.
fn plain(block: Option<(u32, u32)>) -> EncodeOptions {
    EncodeOptions {
        block_size: block,
        channels: ChannelOptions::default(),
        filter: FilterChoice::Off,
        coder: CoderChoice::Fixed,
        remap: RemapChoice::Off,
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
        7,                      // version
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
        7,
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
        7,
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
/// Encoded with `FilterChoice::Row`, not the default: on a 2x2 image the unpredicted file is
/// smaller, because two rows of filter codes cost more than they save.
#[test]
fn prediction_and_zigzag() {
    let src = RawImage::new(2, 2, 1, vec![10, 12, 14, 16]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', 0x1A,
        7,
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
        filter: FilterChoice::Row,
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
        7,
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
        remap: RemapChoice::Off,
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
        remap: RemapChoice::Off,
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
        remap: RemapChoice::Off,
        ..plain(None)
    };
    let bytes = encode(&src, &opts).unwrap();
    assert_eq!(bytes[25], 1, "block coder is Rice");
    assert_eq!(decode(&bytes).unwrap(), src);

    let a = brp_core::analyze(&bytes).unwrap();
    // Red is constant within every 2x2 block, so every one of its blocks takes mode 0.
    assert_eq!(a.width_code_histogram[0][0], 4, "four blocks, all mode 0");
}

/// 2x2 grayscale under the context coder, pinning the derived parameter and the whole model.
///
/// Samples 10, 12, 14, 16, prediction off, so the residual plane *is* the samples and there is no
/// base to subtract. Part 1 is one escape bit; the block is not all zero, so it is 0.
///
/// Every parameter below is derived by section 6.3, never read. A fresh context has A = 4 and
/// N = 1, and `k` is the smallest with `N << k >= A`, so an unseen context always gives k = 2.
///
///   (0,0) = 10  neighbours all outside the image, so every gradient is 0
///               context (0,0,0) -> 364, k = 2; 10 >> 2 = 2, low bits 2  -> `11` `0` `10`
///   (1,0) = 12  left 10 -> err 5 -> Q 2; above and upper-left absent
///               context (0,0,2) -> 366, k = 2; 12 >> 2 = 3, low bits 0  -> `111` `0` `00`
///   (0,1) = 14  above 10 -> err 5 -> Q 2; left and upper-left absent
///               context (2,0,0) -> 526, k = 2; 14 >> 2 = 3, low bits 2  -> `111` `0` `10`
///   (1,1) = 16  above 12 -> err 6 -> Q 2, upper-left 10 -> err 5 -> Q 2, left 14 -> err 7 -> Q 2
///               context (2,2,2) -> 546, k = 2; 16 >> 2 = 4, low bits 0  -> `1111` `0` `00`
///
/// Each sample lands in a context of its own, so none of them sees an updated counter. The four
/// updates still happen — `A += (v + 1) >> 1` — they just fall on contexts nothing else reaches.
///
/// Body bits, 25 of them plus 7 of padding:
///   0 | 11010 | 111000 | 111010 | 1111000
#[test]
fn context_coded_block() {
    let src = RawImage::new(2, 2, 1, vec![10, 12, 14, 16]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', 0x1A,
        7,
        0,
        2, 0, 0, 0,
        2, 0, 0, 0,
        1,                      // channels
        8,
        2, 0, 0, 0,
        2, 0, 0, 0,
        0,                      // filter mode: none
        2,                      // block coder: context-modelled Golomb-Rice
        0x00,                   // channel modes: coded
        // -- body, 4 bytes --
        0x6B, 0x8E, 0xBC, 0x00,
    ];

    let opts = EncodeOptions {
        coder: CoderChoice::Context,
        remap: RemapChoice::Off,
        ..plain(None)
    };
    let actual = encode(&src, &opts).unwrap();
    assert_eq!(
        actual, expected,
        "
  actual: {actual:02X?}
expected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 31);
    assert_eq!(decode(&expected).unwrap(), src);

    // `Auto` must not reach for this coder, however the sizes fall.
    let auto = EncodeOptions {
        coder: CoderChoice::Auto,
        remap: RemapChoice::Off,
        ..plain(None)
    };
    assert_eq!(
        encode(&src, &auto).unwrap()[25],
        0,
        "Auto stays off coder 2"
    );
}

/// 4x1 grayscale in two 2x1 blocks, pinning the escape bit, sign folding, and the rule that an
/// escaped block-channel advances nothing.
///
/// Samples 0, 0, 5, 7. The channel is not constant, so stage 1 leaves it coded; the first block is
/// all zero and takes the escape.
///
///   block 0  escape bit 1, no payload, no model update
///   block 1  escape bit 0
///     (2,0) = 5  left is the escaped 0, above and upper-left outside
///                context (0,0,0) -> 364, k = 2; 5 >> 2 = 1, low bits 1   -> `1` `0` `01`
///                update: A[364] = 4 + 3 = 7, N[364] = 2
///     (3,0) = 7  left 5 -> err -3 -> Q -1. The first non-zero quantised gradient is negative, so
///                all three are negated: (0,0,-1) folds to (0,0,1) -> 365, a context of its own,
///                so k = 2 again; 7 >> 2 = 1, low bits 3               -> `1` `0` `11`
///
/// Body bits, 10 of them plus 6 of padding:
///   1 | 0 | 1001 | 1011
#[test]
fn context_coded_escape_and_sign_folding() {
    let src = RawImage::new(4, 1, 1, vec![0, 0, 5, 7]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        b'B', b'R', b'P', 0x1A,
        7,
        0,
        4, 0, 0, 0,             // width
        1, 0, 0, 0,             // height
        1,                      // channels
        8,
        2, 0, 0, 0,             // block_w
        1, 0, 0, 0,             // block_h
        0,                      // filter mode: none
        2,                      // block coder: context-modelled Golomb-Rice
        0x00,                   // channel modes: coded
        // -- body, 2 bytes --
        0xA6, 0xC0,
    ];

    let opts = EncodeOptions {
        coder: CoderChoice::Context,
        remap: RemapChoice::Off,
        ..plain(Some((2, 1)))
    };
    let actual = encode(&src, &opts).unwrap();
    assert_eq!(
        actual, expected,
        "
  actual: {actual:02X?}
expected: {expected:02X?}"
    );
    assert_eq!(actual.len(), 29);
    assert_eq!(decode(&expected).unwrap(), src);

    // The escape is one bit per block-channel, against the four a Rice mode field spends.
    let a = brp_core::analyze(&expected).unwrap();
    assert_eq!(a.block_header_bits, 2, "two blocks, one bit each");
    assert_eq!(a.blocks[0].width_codes[0], 1, "first block escaped");
    assert_eq!(a.blocks[1].width_codes[0], 0);
    assert_eq!(
        a.width_code_histogram[0][2], 2,
        "both coded samples derived k = 2"
    );
}

/// An alphabet map: a channel that skips a value inside its own range (`FORMAT.md` 3.4).
///
/// 16x8 grayscale cycling through 0, 1, 2, 4. Value 3 is never used, so the channel's range is
/// 0..=4 with one interior gap, and the ranks are 0, 1, 2, 3.
///
/// The map costs four bytes: form 1 (bitmap), `lo` 0, `hi` 4, then one bit for each value strictly
/// between them — 1 for 1, 1 for 2, 0 for 3 — as `110` padded to `1100 0000`.
///
/// Stage 2 then packs ranks instead of samples: min 0, max 3, span 3, width code 2 rather than the
/// 3 the raw values need.
///
/// Body bits, 268 of them plus 4 of padding:
///   00000000 0010              base 0, width code 2
///   00 01 10 11 x32            the ranks, in order
///
/// The pattern is periodic over eight bits, and the twelve bits of block header shift it by four,
/// which is why every full byte after the first two reads `1011 0001`.
#[test]
fn alphabet_map_of_a_channel_with_a_gap() {
    let data: Vec<u8> = (0..128).map(|i| [0u8, 1, 2, 4][i % 4]).collect();
    let src = RawImage::new(16, 8, 1, data).unwrap();

    #[rustfmt::skip]
    let mut expected: Vec<u8> = vec![
        // -- header, 27 bytes --
        b'B', b'R', b'P', 0x1A,
        7,                      // version
        0x01,                   // flags: an alphabet map section follows
        16, 0, 0, 0,            // width
        8, 0, 0, 0,             // height
        1,                      // channels
        8,                      // bit depth
        16, 0, 0, 0,            // block width
        8, 0, 0, 0,             // block height
        0,                      // filter mode: none
        0,                      // block coder: fixed width
        0x00,                   // channel modes: coded
        // -- alphabet map, 4 bytes --
        1,                      // form: bitmap
        0,                      // lo
        4,                      // hi
        0xC0,                   // 1 and 2 are used, 3 is not
        // -- body, 34 bytes --
        0x00,                   // base 0
        0x21,                   // width code 2, then the first two ranks
    ];
    expected.extend(std::iter::repeat_n(0xB1, 31));
    expected.push(0xB0);

    let opts = EncodeOptions {
        remap: RemapChoice::Gaps,
        ..plain(None)
    };
    let actual = encode(&src, &opts).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(decode(&expected).unwrap(), src);

    // And the claim the map is there to make: without it every sample costs a third more.
    let plain_bytes = encode(&src, &plain(None)).unwrap();
    assert_eq!(plain_bytes.len(), 77);
    assert_eq!(actual.len(), 65);
    assert_eq!(plain_bytes[5], 0, "the control carries no map section");
}

/// Filter mode 2: the predictor chosen per 8x8 block instead of per row (`FORMAT.md` 5.2).
///
/// 9x1 grayscale, so the row spans two prediction blocks and each one gets its own kind — the
/// thing mode 1 cannot express. Samples: 10, 12, 14, 16, 18, 20, 22, 200, 3.
///
/// Block 0 covers x = 0..8. Costed over it, Sub spends 10 + 2*6 + 78 = 100 and every other kind
/// more (None 168, Average 134, Paeth ties Sub at 100 but loses the tie to the lower kind), so
/// kind 1. Block 1 is the single sample x = 8, where the left neighbour is 200 and the sample is
/// 3: None spends 3, Sub 59, so kind 0.
///
/// Residuals, zigzagged: 20, 4, 4, 4, 4, 4, 4, 155, 6. The one block then packs them: min 4,
/// max 155, span 151, width code 8.
///
/// Body bits, 90 of them plus 6 of padding:
///   001 000                    filter kinds: Sub for block 0, None for block 1
///   00000100 1000              base 4, width code 8
///   00010000                   residual 16
///   00000000 x6                residuals 0
///   10010111 00000010          residuals 151, 2
#[test]
fn per_block_prediction() {
    let src = RawImage::new(9, 1, 1, vec![10, 12, 14, 16, 18, 20, 22, 200, 3]).unwrap();

    #[rustfmt::skip]
    let expected: Vec<u8> = vec![
        // -- header, 27 bytes --
        b'B', b'R', b'P', 0x1A,
        7,                      // version
        0,                      // flags
        9, 0, 0, 0,             // width
        1, 0, 0, 0,             // height
        1,                      // channels
        8,                      // bit depth
        9, 0, 0, 0,             // block width
        1, 0, 0, 0,             // block height
        2,                      // filter mode: adaptive per 8x8 block
        0,                      // block coder: fixed width
        0x00,                   // channel modes: coded
        // -- body, 12 bytes --
        0x20, 0x12, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x25, 0xC0, 0x80,
    ];

    let opts = EncodeOptions {
        filter: FilterChoice::Block,
        ..plain(None)
    };
    let actual = encode(&src, &opts).unwrap();
    assert_eq!(
        actual, expected,
        "\n  actual: {actual:02X?}\nexpected: {expected:02X?}"
    );
    assert_eq!(decode(&expected).unwrap(), src);

    // The kinds differ per block, which is the whole point: mode 1 has one code for this row and
    // must spend it on a single predictor for both halves.
    let by_row = encode(
        &src,
        &EncodeOptions {
            filter: FilterChoice::Row,
            ..plain(None)
        },
    )
    .unwrap();
    assert_eq!(decode(&by_row).unwrap(), src);
    assert_eq!(by_row[24], 1, "the control must be the per-row mode");
}

/// Locks the header field offsets in `FORMAT.md` section 3 against accidental reordering.
#[test]
fn header_field_offsets() {
    // Values chosen so no channel is constant and none aliases another.
    let data: Vec<u8> = (0..15u8).map(|i| i * 3 + (i % 3) * 7).collect();
    let src = RawImage::new(5, 1, 3, data).unwrap();
    let bytes = encode(&src, &plain(Some((2, 4)))).unwrap();

    assert_eq!(&bytes[0..4], &[b'B', b'R', b'P', 0x1A]);
    assert_eq!(bytes[4], 7);
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
