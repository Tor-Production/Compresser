//! BRP — Block Range Packing: a lossless image codec.
//!
//! The image is split into blocks. Within each block, each channel stores its minimum as a *base*
//! and the number of bits needed for `max - min` as a *width code*, then packs every sample as
//! `sample - base` using exactly that many bits. A constant channel has a width code of zero and
//! costs no payload bits at all. A constant alpha channel is dropped from the file entirely.
//!
//! `docs/FORMAT.md` is the normative specification and outranks this code.
//!
//! ```
//! use brp_core::{decode, encode, EncodeOptions, RawImage};
//!
//! let src = RawImage::new(2, 2, 3, vec![10, 20, 30, 11, 21, 31, 12, 22, 32, 13, 23, 33])?;
//! let bytes = encode(&src, &EncodeOptions::default())?;
//! assert_eq!(decode(&bytes)?, src);
//! # Ok::<(), brp_core::BrpError>(())
//! ```

#![forbid(unsafe_code)]

mod analysis;
mod bitio;
mod block;
mod channels;
mod decode;
mod encode;
mod error;
mod header;
mod image;

pub use analysis::{analyze, Analysis, BlockInfo};
pub use bitio::{BitReader, BitWriter};
pub use block::{BlockGrid, BlockRect};
pub use channels::{ChannelMode, ChannelOptions, ChannelPlan, CodedIndices};
pub use decode::{decode, decode_with, DecodeOptions, DEFAULT_MAX_IMAGE_BYTES};
pub use encode::{encode, EncodeOptions};
pub use error::BrpError;
pub use header::{Header, BIT_DEPTH, HEADER_BASE_SIZE, MAGIC, VERSION, WIDTH_CODE_BITS};
pub use image::{required_len, RawImage, MAX_CHANNELS};

/// Result type used throughout the crate.
pub type Result<T> = core::result::Result<T, BrpError>;
