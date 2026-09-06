//! Property tests for the two invariants that matter most: losslessness, and never panicking.

use brp_core::{
    analyze, decode, encode, ChannelOptions, CoderChoice, EncodeOptions, FilterChoice, RawImage, RemapChoice};
use proptest::prelude::*;

/// An arbitrary image: dimensions up to 32x32, any channel count, arbitrary samples.
fn arb_image() -> impl Strategy<Value = RawImage> {
    (1u32..=32, 1u32..=32, 1u8..=4).prop_flat_map(|(w, h, ch)| {
        let len = (w * h * u32::from(ch)) as usize;
        prop::collection::vec(any::<u8>(), len)
            .prop_map(move |data| RawImage::new(w, h, ch, data).unwrap())
    })
}

/// Every combination of the two stage-1 reductions.
fn arb_channel_options() -> impl Strategy<Value = ChannelOptions> {
    (any::<bool>(), any::<bool>())
        .prop_map(|(constants, aliases)| ChannelOptions { constants, aliases })
}

fn arb_coder() -> impl Strategy<Value = CoderChoice> {
    prop_oneof![
        Just(CoderChoice::Fixed),
        Just(CoderChoice::Rice),
        Just(CoderChoice::Context),
        Just(CoderChoice::Auto),
    ]
}

fn arb_remap() -> impl Strategy<Value = RemapChoice> {
    prop_oneof![Just(RemapChoice::Off), Just(RemapChoice::Gaps)]
}

fn arb_filter() -> impl Strategy<Value = FilterChoice> {
    prop_oneof![
        Just(FilterChoice::Off),
        Just(FilterChoice::Row),
        Just(FilterChoice::Block),
        Just(FilterChoice::Auto),
    ]
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
        channels in arb_channel_options(),
        filter in arb_filter(),
        coder in arb_coder(),
        remap in arb_remap(),
    ) {
        let opts = EncodeOptions { block_size, channels, filter, coder, remap };
        let bytes = encode(&src, &opts).unwrap();
        let back = decode(&bytes).unwrap();
        prop_assert_eq!(back, src);
    }

    /// Encoding is deterministic: the same input and options always give the same bytes.
    #[test]
    fn encoding_is_deterministic(
        src in arb_image(),
        block_size in arb_block_size(),
        filter in arb_filter(),
        coder in arb_coder(),
    ) {
        let opts = EncodeOptions {
            block_size,
            channels: ChannelOptions::default(),
            filter,
            coder,
            remap: RemapChoice::Gaps,
        };
        prop_assert_eq!(encode(&src, &opts).unwrap(), encode(&src, &opts).unwrap());
    }

    /// Whatever the encoder produces, the analyzer must be able to walk.
    #[test]
    fn analysis_accounts_for_the_whole_file(
        src in arb_image(),
        block_size in arb_block_size(),
        filter in arb_filter(),
        coder in arb_coder(),
    ) {
        let opts = EncodeOptions {
            block_size,
            channels: ChannelOptions::default(),
            filter,
            coder,
            remap: RemapChoice::Gaps,
        };
        let bytes = encode(&src, &opts).unwrap();
        let a = analyze(&bytes).unwrap();

        let body_bits = a.filter_bits + a.block_header_bits + a.payload_bits + a.padding_bits;
        prop_assert_eq!(body_bits % 8, 0);
        prop_assert_eq!(a.header_bytes + (body_bits / 8) as usize, a.file_bytes);
        prop_assert_eq!(a.trailing_bytes, 0);
        prop_assert_eq!(a.raw_bytes, src.data().len());
    }

    /// `Auto` is not allowed to be worse than either fixed choice: it encodes both and keeps the
    /// smaller, so its output length must equal the minimum.
    #[test]
    fn auto_is_never_worse(src in arb_image(), block_size in arb_block_size()) {
        let base = |filter| EncodeOptions {
            block_size,
            channels: ChannelOptions::default(),
            filter,
            coder: CoderChoice::Fixed,
            remap: RemapChoice::Gaps,
        };
        let off = encode(&src, &base(FilterChoice::Off)).unwrap().len();
        let by_row = encode(&src, &base(FilterChoice::Row)).unwrap().len();
        let by_block = encode(&src, &base(FilterChoice::Block)).unwrap().len();
        let auto = encode(&src, &base(FilterChoice::Auto)).unwrap().len();

        // Auto's rule, exactly as `FilterChoice::Auto` documents it: prediction has to earn its
        // place before the finer layout is even tried, because a file that does not want to be
        // predicted does not want to pay more side information for it.
        let expected = if by_row < off {
            by_row.min(by_block)
        } else {
            off
        };
        prop_assert_eq!(auto, expected);
    }

    /// `Auto` must not lose to either fixed coder choice either.
    #[test]
    fn auto_coder_is_never_worse(src in arb_image(), block_size in arb_block_size()) {
        let base = |coder| EncodeOptions {
            block_size,
            channels: ChannelOptions::default(),
            filter: FilterChoice::Off,
            coder,
            remap: RemapChoice::Gaps,
        };
        let fixed = encode(&src, &base(CoderChoice::Fixed)).unwrap().len();
        let rice = encode(&src, &base(CoderChoice::Rice)).unwrap().len();
        let auto = encode(&src, &base(CoderChoice::Auto)).unwrap().len();
        prop_assert_eq!(auto, fixed.min(rice));

        // And `Auto` weighs only those two. The context coder is often smaller than both, and
        // still must not be reached for: it costs most of the decode speed. See ADR 0009.
        let bytes = encode(&src, &base(CoderChoice::Auto)).unwrap();
        prop_assert_ne!(bytes[25], brp_core::BLOCK_CODER_CONTEXT);
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
        // Varying samples with stage 1 off, so the header is exactly 25 bytes and the arbitrary
        // body lands on the block loop rather than on a truncated channel section.
        let len = (w * h * u32::from(ch)) as usize;
        let samples: Vec<u8> = (0..len).map(|i| (i * 37 % 251) as u8).collect();
        let src = RawImage::new(w, h, ch, samples).unwrap();
        let opts = EncodeOptions {
            block_size: None,
            channels: ChannelOptions { constants: false, aliases: false },
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
            remap: RemapChoice::Gaps,
        };
        let mut bytes = encode(&src, &opts).unwrap();
        assert_eq!(bytes.len().min(27), 27);
        bytes.truncate(27);
        bytes.extend_from_slice(&body);
        let _ = decode(&bytes);
        let _ = analyze(&bytes);
    }
}
