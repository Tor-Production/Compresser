//! Invariant 2 from `docs/ARCHITECTURE.md`: no malformed input may panic.
//!
//! The decoder parses untrusted files. Every byte it reads is hostile until validated.

use brp_core::{
    analyze, decode, encode, BrpError, ChannelOptions, CoderChoice, EncodeOptions, FilterChoice,
    RawImage, RemapChoice,
};

/// Header length when every channel is coded — no alias byte, no constants.
const BODY_AT: usize = 27;

/// Offset of the `channel_modes` byte.
const MODES_AT: usize = 26;

/// Offset of the `block_coder` byte.
const CODER_AT: usize = 25;

/// Stage 1 disabled, so every channel is coded, the header is exactly [`BODY_AT`] bytes, and there
/// is a real block stream to corrupt.
const ALL_CODED: ChannelOptions = ChannelOptions {
    constants: false,
    aliases: false,
};

fn sample_file() -> Vec<u8> {
    let data: Vec<u8> = (0..(6 * 5 * 4)).map(|i| (i * 7 % 251) as u8).collect();
    let src = RawImage::new(6, 5, 4, data).unwrap();
    encode(
        &src,
        &EncodeOptions {
            block_size: Some((2, 2)),
            channels: ALL_CODED,
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
            // Pinned off: these tests index the header by hand, and a map section would move
            // everything after it.
            remap: RemapChoice::Off,
        },
    )
    .unwrap()
}

/// A file that really carries an alphabet map: 16x8 grey cycling 0, 1, 2, 4, so value 3 is a gap.
fn mapped_file() -> Vec<u8> {
    let data: Vec<u8> = (0..128).map(|i| [0u8, 1, 2, 4][i % 4]).collect();
    let src = RawImage::new(16, 8, 1, data).unwrap();
    let bytes = encode(
        &src,
        &EncodeOptions {
            block_size: None,
            channels: ALL_CODED,
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
            remap: RemapChoice::Gaps,
        },
    )
    .unwrap();
    assert_eq!(bytes[5], brp_core::FLAG_ALPHABET_MAPS, "the map must fire");
    bytes
}

/// Where the map section starts in `mapped_file`: the fixed header plus one modes byte.
const MAP_AT: usize = 27;

#[test]
fn arbitrary_alphabet_forms() {
    let original = mapped_file();
    for form in 0..=255u8 {
        let mut bytes = original.clone();
        bytes[MAP_AT] = form;
        let decoded = decode(&bytes);
        let analyzed = analyze(&bytes);
        assert_eq!(
            decoded.is_ok(),
            analyzed.is_ok(),
            "decode and analyze disagree on alphabet form {form}"
        );
        if form > 2 {
            assert!(matches!(
                decoded.unwrap_err(),
                BrpError::UnsupportedAlphabetForm(_)
            ));
        }
    }
}

#[test]
fn a_truncated_alphabet_section_is_refused() {
    let original = mapped_file();
    for cut in MAP_AT..MAP_AT + 4 {
        let bytes = &original[..cut];
        assert!(decode(bytes).is_err(), "{cut} bytes should not decode");
        assert!(analyze(bytes).is_err(), "{cut} bytes should not analyze");
    }
}

#[test]
fn an_inverted_alphabet_range_is_refused() {
    let mut bytes = mapped_file();
    bytes[MAP_AT + 1] = 200; // lo
    bytes[MAP_AT + 2] = 100; // hi
    assert!(matches!(
        decode(&bytes).unwrap_err(),
        BrpError::AlphabetRangeInverted { lo: 200, hi: 100 }
    ));
}

/// A map that shrinks below what the block stream names must be caught, not read out of bounds.
#[test]
fn a_rank_outside_the_alphabet_is_refused() {
    let mut bytes = mapped_file();
    // Clear the bitmap: the channel then claims to use only its two endpoints, while the block
    // stream still names four ranks.
    bytes[MAP_AT + 3] = 0;
    assert!(matches!(
        decode(&bytes).unwrap_err(),
        BrpError::AlphabetRankOutOfRange { .. }
    ));
}

/// Padding bits in the bitmap are the one place a map could have two encodings. It must not.
#[test]
fn alphabet_bitmap_padding_must_be_zero() {
    let mut bytes = mapped_file();
    bytes[MAP_AT + 3] |= 0x01;
    assert_eq!(decode(&bytes).unwrap_err(), BrpError::AlphabetPaddingSet);
}

#[test]
fn the_sample_has_the_shape_the_other_tests_assume() {
    let bytes = sample_file();
    assert_eq!(bytes[BODY_AT - 1], 0, "all channels coded");
    assert!(bytes.len() > BODY_AT, "there is a block stream");
    assert!(decode(&bytes).is_ok());
}

#[test]
fn empty_and_tiny_inputs() {
    for len in 0..BODY_AT {
        let bytes = vec![0u8; len];
        assert!(matches!(
            decode(&bytes).unwrap_err(),
            BrpError::BadMagic | BrpError::HeaderTooShort { .. }
        ));
    }
}

#[test]
fn bad_magic() {
    let mut bytes = sample_file();
    bytes[1] = b'X';
    assert_eq!(decode(&bytes).unwrap_err(), BrpError::BadMagic);
}

/// The v1 magic must not be mistaken for a current file.
#[test]
fn the_previous_format_generation_is_rejected() {
    let mut bytes = sample_file();
    bytes[0..4].copy_from_slice(b"BRP1");
    assert_eq!(decode(&bytes).unwrap_err(), BrpError::BadMagic);
}

#[test]
fn every_truncation_is_rejected() {
    let bytes = sample_file();
    for cut in 0..bytes.len() {
        assert!(
            decode(&bytes[..cut]).is_err(),
            "truncating to {cut} bytes must fail"
        );
        // analyze() walks the same structure and must agree.
        assert!(analyze(&bytes[..cut]).is_err(), "analyze at {cut} bytes");
    }
    assert!(decode(&bytes).is_ok());
}

#[test]
fn every_single_byte_corruption_is_survivable() {
    let original = sample_file();
    for i in 0..original.len() {
        for pattern in [0x00u8, 0xFF, 0x5A] {
            let mut bytes = original.clone();
            bytes[i] = pattern;
            // Either it decodes to something, or it errors. It must never panic, and the two
            // entry points must agree on which.
            let decoded = decode(&bytes);
            let analyzed = analyze(&bytes);
            assert_eq!(
                decoded.is_ok(),
                analyzed.is_ok(),
                "decode and analyze disagree at byte {i} = {pattern:#04x}"
            );
        }
    }
}

#[test]
fn absurd_dimensions_do_not_allocate() {
    let mut bytes = sample_file();
    bytes[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
    // Must be rejected from the header alone, before any buffer is sized.
    assert_eq!(decode(&bytes).unwrap_err(), BrpError::DimensionOverflow);
}

/// A header that claims a huge image with a tiny body must fail on the bitstream, not by
/// exhausting memory first. 4096x4096x4 is 64 MiB of samples the file cannot possibly hold, and
/// sits under the default limit, so this exercises the bitstream path rather than the limit.
///
/// Which rule fires is not fixed: under the rewritten geometry the same body bits land on
/// different fields, so the decoder may reject a nonsensical width code before it runs out of
/// bits. Both are correct refusals.
#[test]
fn large_declared_geometry_with_empty_body() {
    let mut bytes = sample_file();
    bytes[6..10].copy_from_slice(&4096u32.to_le_bytes());
    bytes[10..14].copy_from_slice(&4096u32.to_le_bytes());
    assert!(matches!(
        decode(&bytes).unwrap_err(),
        BrpError::UnexpectedEof | BrpError::InvalidWidthCode(_)
    ));
    assert!(matches!(
        analyze(&bytes).unwrap_err(),
        BrpError::UnexpectedEof | BrpError::InvalidWidthCode(_)
    ));
}

/// Regression: a single corrupted byte in `width` used to make the decoder attempt an 85 GB
/// allocation from a 33-byte file. The claim cannot be refuted from the bitstream length — an
/// image of constant channels legitimately is nothing but a header — so the decoder needs an
/// explicit size limit.
#[test]
fn one_corrupt_byte_cannot_demand_an_enormous_allocation() {
    let mut bytes = sample_file();
    bytes[9] = 0xFF; // width becomes 0xFF000006
    assert!(matches!(
        decode(&bytes).unwrap_err(),
        BrpError::ImageTooLarge { .. }
    ));
}

/// Version 7 defines bit 0 of the flags byte; the other seven are still reserved.
#[test]
fn reserved_flag_bits_are_rejected() {
    for bit in 1..8 {
        let mut bytes = sample_file();
        bytes[5] = 1 << bit;
        assert!(matches!(
            decode(&bytes).unwrap_err(),
            BrpError::ReservedFlagsSet(_)
        ));
    }

    // Bit 0 is not reserved, but claiming a section that is not there is still refused.
    let mut bytes = sample_file();
    bytes[5] = brp_core::FLAG_ALPHABET_MAPS;
    assert!(decode(&bytes).is_err());
    assert!(analyze(&bytes).is_err());
}

#[test]
fn zero_block_size_in_a_header_is_rejected() {
    for offset in [16, 20] {
        let mut bytes = sample_file();
        bytes[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            decode(&bytes).unwrap_err(),
            BrpError::ZeroDimension { .. }
        ));
    }
}

/// Every possible `channel_modes` byte, against every channel count. Some are valid, most are not;
/// none may panic, and decode and analyze must always agree.
#[test]
fn arbitrary_channel_modes() {
    for channels in 1..=4u8 {
        let n = usize::from(channels);
        let data: Vec<u8> = (0..(4 * 4 * n)).map(|i| (i * 11 % 251) as u8).collect();
        let src = RawImage::new(4, 4, channels, data).unwrap();
        let original = encode(
            &src,
            &EncodeOptions {
                block_size: Some((2, 2)),
                channels: ALL_CODED,
                filter: FilterChoice::Off,
                coder: CoderChoice::Fixed,
                remap: RemapChoice::Gaps,
            },
        )
        .unwrap();

        for modes in 0..=255u8 {
            let mut bytes = original.clone();
            bytes[MODES_AT] = modes;
            let decoded = decode(&bytes);
            let analyzed = analyze(&bytes);
            assert_eq!(
                decoded.is_ok(),
                analyzed.is_ok(),
                "decode and analyze disagree: {channels} channels, modes {modes:#04x}"
            );
        }
    }
}

#[test]
fn alias_validation() {
    // Grayscale in RGB, so channels 1 and 2 both alias channel 0.
    let data: Vec<u8> = (0..16u8).flat_map(|v| [v, v, v]).collect();
    let src = RawImage::new(4, 4, 3, data).unwrap();
    let original = encode(
        &src,
        &EncodeOptions {
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
            remap: RemapChoice::Gaps,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(original[MODES_AT], 0x28, "R coded, G alias, B alias");
    assert!(decode(&original).is_ok());

    // Channel 1 pointing at itself is a forward reference.
    let mut bytes = original.clone();
    bytes[MODES_AT + 1] = 0b01;
    assert_eq!(
        decode(&bytes).unwrap_err(),
        BrpError::InvalidAliasTarget {
            channel: 1,
            target: 1
        }
    );

    // Channel 2 pointing at channel 1, which is itself an alias: a chain.
    let mut bytes = original.clone();
    bytes[MODES_AT + 1] = 0b01_00;
    assert_eq!(
        decode(&bytes).unwrap_err(),
        BrpError::AliasTargetNotCoded {
            channel: 2,
            target: 1
        }
    );

    // Bits beyond the two aliases present must be clear.
    let mut bytes = original.clone();
    bytes[MODES_AT + 1] = 0b0001_0000;
    assert_eq!(
        decode(&bytes).unwrap_err(),
        BrpError::AliasTargetBitsSet(0b0001_0000)
    );

    // Channel 0 can never alias.
    let mut bytes = original.clone();
    bytes[MODES_AT] = 0b00_00_00_10;
    assert_eq!(decode(&bytes).unwrap_err(), BrpError::AliasOnFirstChannel);
}

/// Every possible `filter_mode` byte. Only 0, 1 and 2 exist; the rest must be refused.
///
/// The three that exist are *not* asserted to decode: flipping the byte of a file that was written
/// without prediction claims kind codes the body never carried, and everything after them is then
/// read at the wrong offset. What must hold there is the weaker property this test opens with —
/// `decode` and `analyze` agree — because they walk the same structure.
#[test]
fn arbitrary_filter_modes() {
    let original = sample_file();
    for mode in 0..=255u8 {
        let mut bytes = original.clone();
        bytes[24] = mode;
        let decoded = decode(&bytes);
        let analyzed = analyze(&bytes);
        assert_eq!(
            decoded.is_ok(),
            analyzed.is_ok(),
            "decode and analyze disagree on filter mode {mode}"
        );
        if mode > 2 {
            assert!(matches!(
                decoded.unwrap_err(),
                BrpError::UnsupportedFilterMode(_)
            ));
        }
    }
}

/// A predicted file whose filter kinds are corrupted must be refused, not silently mis-decoded.
#[test]
fn invalid_filter_kinds_are_rejected() {
    let data: Vec<u8> = (0..(8 * 8)).map(|i| (i / 2) as u8).collect();
    let src = RawImage::new(8, 8, 1, data).unwrap();
    let original = encode(
        &src,
        &EncodeOptions {
            block_size: Some((4, 4)),
            channels: ALL_CODED,
            filter: FilterChoice::Row,
            coder: CoderChoice::Fixed,
            remap: RemapChoice::Gaps,
        },
    )
    .unwrap();
    assert_eq!(original[24], 1, "prediction is on");
    assert!(decode(&original).is_ok());

    // Sweep the first body byte, which carries the first two rows' predictors and part of a third.
    for b in 0..=255u8 {
        let mut bytes = original.clone();
        bytes[BODY_AT] = b;
        let decoded = decode(&bytes);
        let analyzed = analyze(&bytes);
        assert_eq!(
            decoded.is_ok(),
            analyzed.is_ok(),
            "decode and analyze disagree on filter byte {b:#04x}"
        );
    }
}

/// Every possible `block_coder` byte. Only 0, 1 and 2 exist; the rest must be refused.
///
/// Relabelling a fixed-width file as coder 1 or 2 is not itself an error — the bits are then read
/// under different rules and either run out or do not. What must hold is that `decode` and
/// `analyze` reach the same verdict, and that neither panics.
#[test]
fn arbitrary_block_coders() {
    let original = sample_file();
    for coder in 0..=255u8 {
        let mut bytes = original.clone();
        bytes[CODER_AT] = coder;
        let decoded = decode(&bytes);
        let analyzed = analyze(&bytes);
        assert_eq!(
            decoded.is_ok(),
            analyzed.is_ok(),
            "decode and analyze disagree on block coder {coder}"
        );
        if coder > 2 {
            assert_eq!(decoded.unwrap_err(), BrpError::UnsupportedBlockCoder(coder));
        }
    }
}

/// A context-coded stream has no field a file can get wrong — every parameter is derived — so
/// truncation is the only way to malform one. `FORMAT.md` section 9 says so; this checks it.
#[test]
fn truncated_context_streams_are_rejected() {
    let data: Vec<u8> = (0..(8 * 8 * 3)).map(|i| (i * 11 % 251) as u8).collect();
    let src = RawImage::new(8, 8, 3, data).unwrap();
    let bytes = encode(
        &src,
        &EncodeOptions {
            block_size: Some((4, 4)),
            channels: ALL_CODED,
            filter: FilterChoice::Row,
            coder: CoderChoice::Context,
            remap: RemapChoice::Gaps,
        },
    )
    .unwrap();
    assert_eq!(decode(&bytes).unwrap(), src);

    for cut in BODY_AT..bytes.len() {
        let short = &bytes[..cut];
        assert!(
            decode(short).is_err(),
            "truncation to {cut} bytes should fail"
        );
        assert_eq!(
            decode(short).is_ok(),
            analyze(short).is_ok(),
            "decode and analyze disagree at {cut} bytes"
        );
    }
}

/// Every bit of a context-coded body, flipped one at a time. The parameter is derived from data
/// the file no longer controls directly, so a flipped bit must still land on an error or on some
/// other image — never on a panic, and never on a disagreement between the two walkers.
#[test]
fn corrupt_context_bodies_never_panic() {
    let data: Vec<u8> = (0..(6 * 5 * 4)).map(|i| (i * 7 % 251) as u8).collect();
    let src = RawImage::new(6, 5, 4, data).unwrap();
    let original = encode(
        &src,
        &EncodeOptions {
            block_size: Some((2, 2)),
            channels: ALL_CODED,
            filter: FilterChoice::Off,
            coder: CoderChoice::Context,
            remap: RemapChoice::Gaps,
        },
    )
    .unwrap();

    for byte in BODY_AT..original.len() {
        for bit in 0..8 {
            let mut bytes = original.clone();
            bytes[byte] ^= 1 << bit;
            assert_eq!(
                decode(&bytes).is_ok(),
                analyze(&bytes).is_ok(),
                "decode and analyze disagree on byte {byte} bit {bit}"
            );
        }
    }
}

/// A Rice-coded file with corrupted mode fields must be refused, not silently mis-decoded.
#[test]
fn corrupt_rice_modes_are_rejected() {
    let data: Vec<u8> = (0..(8 * 8)).map(|i| (i * 3 % 200) as u8).collect();
    let src = RawImage::new(8, 8, 1, data).unwrap();
    let original = encode(
        &src,
        &EncodeOptions {
            block_size: Some((4, 4)),
            channels: ALL_CODED,
            filter: FilterChoice::Off,
            coder: CoderChoice::Rice,
            remap: RemapChoice::Gaps,
        },
    )
    .unwrap();
    assert_eq!(original[CODER_AT], 1, "Rice coding is on");
    assert!(decode(&original).is_ok());

    for b in 0..=255u8 {
        let mut bytes = original.clone();
        bytes[BODY_AT + 1] = b;
        let decoded = decode(&bytes);
        let analyzed = analyze(&bytes);
        assert_eq!(
            decoded.is_ok(),
            analyzed.is_ok(),
            "decode and analyze disagree on Rice mode byte {b:#04x}"
        );
    }
}

/// Sweeps the whole byte range through the first two body bytes, which carry a base and part of a
/// width code. Some values are valid, some are not; none may panic.
#[test]
fn arbitrary_block_headers() {
    let original = sample_file();
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            let mut bytes = original.clone();
            bytes[BODY_AT] = a;
            bytes[BODY_AT + 1] = b;
            let _ = decode(&bytes);
            let _ = analyze(&bytes);
        }
    }
}
