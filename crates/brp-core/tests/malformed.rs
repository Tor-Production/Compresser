//! Invariant 2 from `docs/ARCHITECTURE.md`: no malformed input may panic.
//!
//! The decoder parses untrusted files. Every byte it reads is hostile until validated.

use brp_core::{analyze, decode, encode, BrpError, EncodeOptions, RawImage};

fn sample_file() -> Vec<u8> {
    let data: Vec<u8> = (0..(6 * 5 * 4)).map(|i| (i * 7 % 251) as u8).collect();
    let src = RawImage::new(6, 5, 4, data).unwrap();
    encode(
        &src,
        &EncodeOptions {
            block_size: Some((2, 2)),
            alpha_opt: false,
        },
    )
    .unwrap()
}

#[test]
fn empty_and_tiny_inputs() {
    for len in 0..24 {
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

#[test]
fn every_truncation_is_rejected() {
    let bytes = sample_file();
    for cut in 0..bytes.len() {
        let err = decode(&bytes[..cut]);
        assert!(err.is_err(), "truncating to {cut} bytes must fail");
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
    // analyze() never allocates the pixel buffer at all, and must reach the same verdict.
    assert!(matches!(
        analyze(&bytes).unwrap_err(),
        BrpError::UnexpectedEof | BrpError::InvalidWidthCode(_)
    ));
}

/// Regression: a single corrupted byte in `width` used to make the decoder attempt an 85 GB
/// allocation from a 33-byte file. The claim cannot be refuted from the bitstream length -- an
/// image of constant blocks legitimately compresses to almost nothing -- so the decoder needs an
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

#[test]
fn reserved_flag_bits_are_rejected() {
    for bit in 1..8 {
        let mut bytes = sample_file();
        bytes[5] |= 1 << bit;
        assert!(matches!(
            decode(&bytes).unwrap_err(),
            BrpError::ReservedFlagsSet(_)
        ));
    }
}

#[test]
fn alpha_constant_on_an_opaque_format_is_rejected() {
    let src = RawImage::new(4, 4, 3, vec![9; 48]).unwrap();
    let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
    bytes[5] = 0b0000_0001;
    assert_eq!(
        decode(&bytes).unwrap_err(),
        BrpError::AlphaConstantWithoutAlpha(3)
    );
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

/// Sweeps the whole byte range through the first body byte, which carries a base and part of a
/// width code. Some values are valid, some are not; none may panic.
#[test]
fn arbitrary_block_headers() {
    let original = sample_file();
    let body = 24;
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            let mut bytes = original.clone();
            bytes[body] = a;
            bytes[body + 1] = b;
            let _ = decode(&bytes);
            let _ = analyze(&bytes);
        }
    }
}
