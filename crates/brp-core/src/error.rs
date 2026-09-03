use thiserror::Error;

/// Every way a BRP bitstream or an API call can be invalid.
///
/// One variant per validation rule in `docs/FORMAT.md` section 7. The decoder returns these; it
/// never panics on malformed input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrpError {
    #[error("not a BRP file: bad magic")]
    BadMagic,

    #[error("truncated header: {got} bytes available, {need} required")]
    HeaderTooShort { got: usize, need: usize },

    #[error("unsupported format version {found}, this build reads version {expected}")]
    UnsupportedVersion { found: u8, expected: u8 },

    #[error("unsupported bit depth {0}, only 8 is defined in version 1")]
    UnsupportedBitDepth(u8),

    #[error("invalid channel count {0}, expected 1, 2, 3 or 4")]
    InvalidChannelCount(u8),

    #[error("{what} must be greater than zero")]
    ZeroDimension { what: &'static str },

    #[error("reserved flag bits set: 0x{0:02x}")]
    ReservedFlagsSet(u8),

    #[error("ALPHA_CONSTANT is set but the image has no alpha channel ({0} channels)")]
    AlphaConstantWithoutAlpha(u8),

    #[error("image dimensions overflow the address space")]
    DimensionOverflow,

    #[error("invalid width code {0}, must not exceed the bit depth")]
    InvalidWidthCode(u8),

    #[error("bitstream ended before the declared geometry was satisfied")]
    UnexpectedEof,

    #[error("trailing padding bits are not zero")]
    NonZeroPadding,

    #[error("declared image needs {need} bytes, above the decoder limit of {limit}")]
    ImageTooLarge { need: usize, limit: usize },

    #[error("could not allocate {bytes} bytes for the decoded image")]
    AllocationFailed { bytes: usize },

    #[error("pixel buffer has {got} bytes, {need} required for {width}x{height}x{channels}")]
    BufferLengthMismatch {
        got: usize,
        need: usize,
        width: u32,
        height: u32,
        channels: u8,
    },
}
