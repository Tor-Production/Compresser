//! Invariant 2 from `docs/ARCHITECTURE.md`: no malformed input may panic.
//!
//! The decoder parses untrusted files. Every byte it reads is hostile until validated.

use brp_core::{
    analyze, decode, encode, BrpError, ChannelOptions, CoderChoice, EncodeOptions, FilterChoice,
    RawImage,
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
        },
    )
    .unwrap()
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

/// Version 3 reserves the whole flags byte.
#[test]
fn reserved_flag_bits_are_rejected() {
    for bit in 0..8 {
        let mut bytes = sample_file();
        bytes[5] = 1 << bit;
        assert!(matches!(
            decode(&bytes).unwrap_err(),
            BrpError::ReservedFlagsSet(_)
        ));
    }
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

/// Every possible `filter_mode` byte. Only 0 and 1 exist; the rest must be refused.
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
        if mode > 1 {
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
            filter: FilterChoice::On,
            coder: CoderChoice::Fixed,
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

/// Every possible `block_coder` byte. Only 0 and 1 exist; the rest must be refused.
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
        if coder > 1 {
            assert!(matches!(
                decoded.unwrap_err(),
                BrpError::UnsupportedBlockCoder(_)
            ));
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
