//! Property tests for the two invariants that matter most: losslessness, and never panicking.

use brp_core::{analyze, decode, encode, EncodeOptions, RawImage};
use proptest::prelude::*;

/// An arbitrary image: dimensions up to 32x32, any channel count, arbitrary samples.
fn arb_image() -> impl Strategy<Value = RawImage> {
    (1u32..=32, 1u32..=32, 1u8..=4).prop_flat_map(|(w, h, ch)| {
        let len = (w * h * u32::from(ch)) as usize;
        prop::collection::vec(any::<u8>(), len)
            .prop_map(move |data| RawImage::new(w, h, ch, data).unwrap())
    })
}

/// Block sizes that mostly do not divide the image evenly.
fn arb_block_size() -> impl Strategy<Value = Option<(u32, u32)>> {
    prop_oneof![
        1 => Just(None),
        4 => (1u32..=40, 1u32..=40).prop_map(Some),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The central property: encoding then decoding returns exactly what went in.
    #[test]
    fn round_trip_is_lossless(
        src in arb_image(),
        block_size in arb_block_size(),
        alpha_opt in any::<bool>(),
    ) {
        let opts = EncodeOptions { block_size, alpha_opt };
        let bytes = encode(&src, &opts).unwrap();
        let back = decode(&bytes).unwrap();
        prop_assert_eq!(back, src);
    }

    /// Encoding is deterministic: the same input and options always give the same bytes.
    #[test]
    fn encoding_is_deterministic(src in arb_image(), block_size in arb_block_size()) {
        let opts = EncodeOptions { block_size, alpha_opt: true };
        prop_assert_eq!(encode(&src, &opts).unwrap(), encode(&src, &opts).unwrap());
    }

    /// Whatever the encoder produces, the analyzer must be able to walk.
    #[test]
    fn analysis_accounts_for_the_whole_file(
        src in arb_image(),
        block_size in arb_block_size(),
    ) {
        let opts = EncodeOptions { block_size, alpha_opt: true };
        let bytes = encode(&src, &opts).unwrap();
        let a = analyze(&bytes).unwrap();

        let body_bits = a.block_header_bits + a.payload_bits + a.padding_bits;
        prop_assert_eq!(body_bits % 8, 0);
        prop_assert_eq!(a.header_bytes + (body_bits / 8) as usize, a.file_bytes);
        prop_assert_eq!(a.trailing_bytes, 0);
        prop_assert_eq!(a.raw_bytes, src.data().len());
    }

    /// Arbitrary bytes must never panic the decoder, whatever they happen to say.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = decode(&bytes);
        let _ = analyze(&bytes);
    }

    /// The same, but starting from a valid header so the fuzzing reaches the block loop.
    #[test]
    fn valid_header_with_arbitrary_body_never_panics(
        body in prop::collection::vec(any::<u8>(), 0..256),
        w in 1u32..=16,
        h in 1u32..=16,
        ch in 1u8..=4,
    ) {
        let src = RawImage::new(w, h, ch, vec![0; (w * h * u32::from(ch)) as usize]).unwrap();
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        bytes.truncate(24);
        bytes.extend_from_slice(&body);
        let _ = decode(&bytes);
        let _ = analyze(&bytes);
    }
}
