//! Decoder. Mirror of [`crate::encode`] — change both together.
//!
//! Everything read from the bitstream is untrusted until validated. See `docs/FORMAT.md` section 8.

use crate::alphabet;
use crate::bitio::BitReader;
use crate::block::BlockGrid;
use crate::channels::{ChannelMode, CodedIndices};
use crate::context::{ContextModel, Plane};
use crate::encode::sample_index;
use crate::error::BrpError;
use crate::header::{
    Header, BLOCK_CODER_CONTEXT, BLOCK_CODER_RICE, WIDTH_CODE_BITS, ZERO_BLOCK_BITS,
};
use crate::image::{required_len, RawImage, MAX_CHANNELS};
use crate::predict::{self, FilterLayout, FILTER_KINDS, FILTER_KIND_BITS};
use crate::rice;
use crate::Result;

/// Default ceiling on a decoded image, in bytes of samples.
///
/// A header declares its dimensions in 27 bytes, so a tiny file can claim an enormous image. The
/// claim can even be legitimate — an image of constant channels really is just a header — so the
/// bitstream length is no defence. A decoder reading untrusted files needs an explicit limit
/// instead. 256 MiB covers an 8192x8192 RGBA image.
pub const DEFAULT_MAX_IMAGE_BYTES: usize = 256 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Refuse to allocate a pixel buffer larger than this.
    pub max_image_bytes: usize,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
        }
    }
}

/// Decodes a BRP v6 bitstream, with [`DEFAULT_MAX_IMAGE_BYTES`] as the size limit.
///
/// Returns an error for any malformed input; never panics.
pub fn decode(bytes: &[u8]) -> Result<RawImage> {
    decode_with(bytes, &DecodeOptions::default())
}

/// Decodes a BRP v6 bitstream with an explicit resource limit.
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<RawImage> {
    let (header, header_len) = Header::parse(bytes)?;
    let stride = usize::from(header.channels);
    let coded = header.coded_indices();
    let len = required_len(header.width, header.height, header.channels)?;

    if len > opts.max_image_bytes {
        return Err(BrpError::ImageTooLarge {
            need: len,
            limit: opts.max_image_bytes,
        });
    }
    // Fallible allocation: a limit raised past what the machine has must still be an error
    // rather than an abort.
    let mut data: Vec<u8> = Vec::new();
    data.try_reserve_exact(len)
        .map_err(|_| BrpError::AllocationFailed { bytes: len })?;
    data.resize(len, 0);

    // Reconstruction step 1: constant channels. See FORMAT.md section 6.
    for c in 0..stride {
        if let ChannelMode::Constant(v) = header.plan.mode(c) {
            let mut i = c;
            while i < len {
                data[i] = v;
                i += stride;
            }
        }
    }

    // Step 2: coded channels, from the block stream.
    if !coded.is_empty() {
        let grid = BlockGrid::new(header.width, header.height, header.block_w, header.block_h);
        let mut reader = BitReader::new(&bytes[header_len..]);

        // The predictor codes precede the blocks, so they are available before they are needed.
        // How many there are is what `filter_mode` decides: one per row, or one per 8x8 block.
        let layout = FilterLayout::from_mode(header.filter_mode);
        let mut kinds = Vec::new();
        if let Some(layout) = layout {
            kinds.reserve(layout.units(header.width, header.height));
            for _ in 0..layout.units(header.width, header.height) {
                let kind = reader.read(FILTER_KIND_BITS)? as u8;
                if kind >= FILTER_KINDS {
                    return Err(BrpError::InvalidFilterKind(kind));
                }
                kinds.push(kind);
            }
        }

        if header.block_coder == BLOCK_CODER_CONTEXT {
            read_context_blocks(&mut reader, &mut data, &header, &coded, &grid)?;
        } else {
            read_parametered_blocks(&mut reader, &mut data, &header, &coded, &grid)?;
        }
        reader.verify_padding()?;

        // Step 2.5: turn residuals back into samples, in raster order so each prediction reads
        // neighbours this loop has already restored.
        if let Some(layout) = layout {
            predict::undo_in_place(
                layout,
                &mut data,
                header.width,
                header.height,
                stride,
                &coded,
                &kinds,
            );
        }
    }

    // Step 3: aliases, copied from their targets. Targets are always coded, so they exist by now.
    for c in 0..stride {
        if let ChannelMode::Alias(target) = header.plan.mode(c) {
            let mut src = usize::from(target);
            let mut dst = c;
            while dst < len {
                data[dst] = data[src];
                src += stride;
                dst += stride;
            }
        }
    }

    // Step 4: turn ranks back into samples, last, because stage 1 worked in rank space — an alias
    // copied its target's ranks and each channel has its own table.
    alphabet::undo_in_place(&mut data, stride, &header.alphabet)?;

    RawImage::new(header.width, header.height, header.channels, data)
}

/// Reads every block under `block_coder` 0 and 1, where each block-channel names its own base and
/// parameter. See `FORMAT.md` sections 6.1 and 6.2.
fn read_parametered_blocks(
    reader: &mut BitReader,
    data: &mut [u8],
    header: &Header,
    coded: &CodedIndices,
    grid: &BlockGrid,
) -> Result<()> {
    let stride = usize::from(header.channels);
    let rice_coded = header.block_coder == BLOCK_CODER_RICE;
    let mut bases = [0u8; MAX_CHANNELS];
    let mut widths = [0u8; MAX_CHANNELS];

    for rect in grid.iter() {
        // Part 1: channel headers.
        for slot in 0..coded.len() {
            bases[slot] = reader.read(u32::from(header.bit_depth))? as u8;
            let param = reader.read(WIDTH_CODE_BITS)?;
            if rice_coded {
                // Rejects the reserved modes here; the parameter is derived below.
                rice::parameter_for(param)?;
            } else if param > u32::from(header.bit_depth) {
                return Err(BrpError::InvalidWidthCode(param as u8));
            }
            widths[slot] = param as u8;
        }

        // Part 2: payloads, planar.
        for slot in 0..coded.len() {
            let param = u32::from(widths[slot]);
            let base = bases[slot];
            let channel = coded.channel(slot);
            // `None` means the block carries no payload: every residual in it is zero.
            let k = if rice_coded {
                rice::parameter_for(param)?
            } else if param == 0 {
                None
            } else {
                Some(param)
            };

            for row in 0..rect.h {
                let mut i = sample_index(header.width, stride, rect.x, rect.y + row, channel);
                match k {
                    None => {
                        for _ in 0..rect.w {
                            data[i] = base;
                            i += stride;
                        }
                    }
                    Some(k) => {
                        for _ in 0..rect.w {
                            let residual = if rice_coded {
                                rice::read(reader, k)? as u8
                            } else {
                                reader.read(k)? as u8
                            };
                            // Wraps rather than erroring; see FORMAT.md section 9.
                            data[i] = base.wrapping_add(residual);
                            i += stride;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reads every block under `block_coder` 2, the mirror of `encode::write_context_blocks`.
///
/// The context of each residual is read out of `data`, which by then holds the residuals this loop
/// has already stored — so the neighbours it needs are always present, and the reconstruction
/// order of `FORMAT.md` section 7 is untouched. Nothing here is indexed by a value from the file:
/// the parameter is derived and bounded by construction, and the escape bit has no invalid value.
fn read_context_blocks(
    reader: &mut BitReader,
    data: &mut [u8],
    header: &Header,
    coded: &CodedIndices,
    grid: &BlockGrid,
) -> Result<()> {
    let stride = usize::from(header.channels);
    let plane = Plane::new(header.width, stride);
    let mut model = ContextModel::new();
    let mut all_zero = [false; MAX_CHANNELS];

    for rect in grid.iter() {
        for zero in all_zero[..coded.len()].iter_mut() {
            *zero = reader.read(ZERO_BLOCK_BITS)? == 1;
        }

        for (slot, &all_zero) in all_zero[..coded.len()].iter().enumerate() {
            let channel = coded.channel(slot);
            for row in 0..rect.h {
                let y = rect.y + row;
                let mut i = sample_index(header.width, stride, rect.x, y, channel);
                if all_zero {
                    // No payload, and the model never saw these samples.
                    for _ in 0..rect.w {
                        data[i] = 0;
                        i += stride;
                    }
                    continue;
                }
                for col in 0..rect.w {
                    let x = rect.x + col;
                    let context = plane.context(data, x, y, channel);
                    let v = rice::read(reader, model.parameter(context))?;
                    model.update(context, v);
                    data[i] = v as u8;
                    i += stride;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alphabet::RemapChoice;
    use crate::channels::ChannelOptions;
    use crate::encode::{encode, CoderChoice, EncodeOptions, FilterChoice};

    /// Most decoder tests reason about the block stream directly, so they pin both stages off.
    fn plain() -> EncodeOptions {
        EncodeOptions {
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
            ..Default::default()
        }
    }

    fn img(w: u32, h: u32, ch: u8, data: Vec<u8>) -> RawImage {
        RawImage::new(w, h, ch, data).unwrap()
    }

    #[test]
    fn round_trips_a_gradient() {
        let src = img(4, 4, 1, (0..16u8).collect());
        let bytes = encode(&src, &plain()).unwrap();
        assert_eq!(decode(&bytes).unwrap(), src);
    }

    #[test]
    fn constant_channel_costs_nothing_at_all() {
        let src = img(8, 8, 1, vec![42; 64]);
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        // 26 fixed header bytes + modes byte + one constant. No block stream.
        assert_eq!(bytes.len(), 28);
        assert_eq!(decode(&bytes).unwrap(), src);
    }

    #[test]
    fn grayscale_in_rgb_costs_one_channel() {
        let gray: Vec<u8> = (0..64u8).collect();
        let as_rgb: Vec<u8> = gray.iter().flat_map(|&v| [v, v, v]).collect();
        let one = img(8, 8, 1, gray);
        let three = img(8, 8, 3, as_rgb);

        let a = encode(&one, &plain()).unwrap();
        let b = encode(&three, &plain()).unwrap();
        assert_eq!(decode(&b).unwrap(), three);
        // Same payload; the RGB file pays only for the alias byte.
        assert_eq!(b.len(), a.len() + 1);
    }

    #[test]
    fn truncated_bitstream_is_an_error() {
        let src = img(4, 4, 3, (0..48u8).collect());
        let bytes = encode(&src, &plain()).unwrap();
        for cut in 27..bytes.len() {
            assert!(
                decode(&bytes[..cut]).is_err(),
                "truncation to {cut} bytes should fail"
            );
        }
    }

    #[test]
    fn rejects_an_out_of_range_width_code() {
        let src = img(2, 2, 1, vec![0, 1, 2, 3]);
        let mut bytes = encode(&src, &plain()).unwrap();
        // Block header is base (8 bits) then width_code (4 bits): the code is the high nibble
        // of the byte right after the header.
        let wc_byte = 27 + 1;
        bytes[wc_byte] = (bytes[wc_byte] & 0x0F) | 0x90; // width_code = 9
        assert_eq!(decode(&bytes).unwrap_err(), BrpError::InvalidWidthCode(9));
    }

    #[test]
    fn rejects_dirty_padding() {
        let src = img(2, 2, 1, vec![1, 2, 3, 4]);
        let mut bytes = encode(&src, &plain()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] |= 0b0000_0001;
        assert_eq!(decode(&bytes).unwrap_err(), BrpError::NonZeroPadding);
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        let src = img(4, 4, 1, (0..16u8).collect());
        let mut bytes = encode(&src, &plain()).unwrap();
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(decode(&bytes).unwrap(), src);
    }

    /// A file that declares a huge image must be refused before the buffer is allocated, even
    /// though such a file can be perfectly well-formed.
    #[test]
    fn enforces_the_size_limit() {
        let src = img(4, 4, 1, (0..16u8).collect());
        let mut bytes = encode(&src, &plain()).unwrap();
        bytes[6..10].copy_from_slice(&100_000u32.to_le_bytes());
        bytes[10..14].copy_from_slice(&100_000u32.to_le_bytes());

        assert_eq!(
            decode(&bytes).unwrap_err(),
            BrpError::ImageTooLarge {
                need: 10_000_000_000,
                limit: DEFAULT_MAX_IMAGE_BYTES,
            }
        );
    }

    #[test]
    fn the_limit_is_adjustable() {
        let src = img(64, 64, 3, (0..(64 * 64 * 3)).map(|i| i as u8).collect());
        let bytes = encode(&src, &plain()).unwrap();

        let tight = DecodeOptions { max_image_bytes: 8 };
        assert!(matches!(
            decode_with(&bytes, &tight).unwrap_err(),
            BrpError::ImageTooLarge { .. }
        ));

        let roomy = DecodeOptions {
            max_image_bytes: 64 * 64 * 3,
        };
        assert_eq!(decode_with(&bytes, &roomy).unwrap(), src);
    }

    /// Disabling stage 1 must still produce a valid, losslessly decodable file.
    #[test]
    fn stage_one_is_optional() {
        let src = img(8, 8, 4, (0..64u8).flat_map(|v| [v, v, v, 255]).collect());
        let opts = EncodeOptions {
            block_size: None,
            channels: ChannelOptions {
                constants: false,
                aliases: false,
            },
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
            remap: RemapChoice::Gaps,
        };
        let all_coded = encode(&src, &opts).unwrap();
        let reduced = encode(&src, &plain()).unwrap();

        assert_eq!(decode(&all_coded).unwrap(), src);
        assert_eq!(decode(&reduced).unwrap(), src);
        assert!(reduced.len() < all_coded.len());
    }

    fn with_filter(choice: FilterChoice) -> EncodeOptions {
        EncodeOptions {
            block_size: Some((8, 8)),
            filter: choice,
            coder: CoderChoice::Fixed,
            ..Default::default()
        }
    }

    /// Prediction is a round trip in its own right, and `Auto` must never lose to either fixed
    /// choice, since it encodes both and keeps the smaller.
    #[test]
    fn prediction_round_trips_and_auto_never_loses() {
        let data: Vec<u8> = (0..(32 * 32 * 3))
            .map(|i| (100 + (i / 3) % 5) as u8)
            .collect();
        let smooth = img(32, 32, 3, data);

        let off = encode(&smooth, &with_filter(FilterChoice::Off)).unwrap();
        let on = encode(&smooth, &with_filter(FilterChoice::Row)).unwrap();
        let auto = encode(&smooth, &with_filter(FilterChoice::Auto)).unwrap();

        assert_eq!(decode(&off).unwrap(), smooth);
        assert_eq!(decode(&on).unwrap(), smooth);
        assert_eq!(decode(&auto).unwrap(), smooth);
        assert_eq!(auto.len(), on.len().min(off.len()));
    }

    #[test]
    fn rejects_an_invalid_filter_kind() {
        let src = img(8, 8, 1, (0..64u8).map(|v| v / 2).collect());
        let mut bytes = encode(&src, &with_filter(FilterChoice::Row)).unwrap();
        // The first row's predictor occupies the top three bits of the first body byte.
        bytes[27] = (bytes[27] & 0b0001_1111) | 0b1110_0000; // kind 7
        assert_eq!(decode(&bytes).unwrap_err(), BrpError::InvalidFilterKind(7));
    }
}
