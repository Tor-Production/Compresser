//! Experimental compression back-ends, for measurement only.
//!
//! **Nothing here is part of the BRP format.** `docs/FORMAT.md` describes what a `.brp` file is;
//! this crate exists to find out what it *should* be. A pipeline earns its way into the format by
//! winning on the corpus, and then only with a version bump and an ADR.
//!
//! Every pipeline implements [`Codec`], so the runner can measure size and speed uniformly and
//! verify losslessness on every image it reports.

use anyhow::Result;
use brp_core::RawImage;
use flate2::{write::DeflateEncoder, Compression};
use std::io::Write;

pub mod blockpack;
pub mod context;
pub mod corpus;
pub mod filters;
pub mod grid;
pub mod huffman;
pub mod lzw;
pub mod predict;
pub mod predictors;
pub mod quadtree;
pub mod remap;
pub mod stats;

/// One compression pipeline, end to end.
pub trait Codec: Sync {
    fn name(&self) -> String;
    fn encode(&self, img: &RawImage) -> Result<Vec<u8>>;
    fn decode(&self, bytes: &[u8]) -> Result<RawImage>;
}

// ---------------------------------------------------------------------------------------------
// Byte-stream stages
// ---------------------------------------------------------------------------------------------

pub fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::best());
    e.write_all(data)?;
    Ok(e.finish()?)
}

pub fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    use flate2::read::DeflateDecoder;
    use std::io::Read;
    let mut out = Vec::new();
    DeflateDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}

/// How a pipeline squeezes its byte stream after the image-aware stage has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entropy {
    None,
    Deflate,
    Huffman,
    Lzw,
}

impl Entropy {
    pub fn suffix(self) -> &'static str {
        match self {
            Entropy::None => "",
            Entropy::Deflate => "+deflate",
            Entropy::Huffman => "+huffman",
            Entropy::Lzw => "+lzw",
        }
    }

    pub fn pack(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Entropy::None => Ok(data.to_vec()),
            Entropy::Deflate => deflate(data),
            Entropy::Huffman => Ok(huffman::encode(data)),
            Entropy::Lzw => Ok(lzw::encode(data)),
        }
    }

    pub fn unpack(self, data: &[u8]) -> Result<Vec<u8>> {
        match self {
            Entropy::None => Ok(data.to_vec()),
            Entropy::Deflate => inflate(data),
            Entropy::Huffman => huffman::decode(data),
            Entropy::Lzw => lzw::decode(data),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pipelines
// ---------------------------------------------------------------------------------------------

/// The shipped codec at a fixed block size, optionally followed by an entropy stage.
pub struct Brp {
    pub block: Option<u32>,
    pub entropy: Entropy,
    /// Which prediction setting the format itself is asked to use.
    pub filter: brp_core::FilterChoice,
    /// Which block coder the format itself is asked to use.
    pub coder: brp_core::CoderChoice,
}

impl Codec for Brp {
    fn name(&self) -> String {
        let block = match self.block {
            None => "whole".to_string(),
            Some(n) => format!("{n}x{n}"),
        };
        let f = match self.filter {
            brp_core::FilterChoice::Off => "",
            brp_core::FilterChoice::Row => ",pred",
            brp_core::FilterChoice::Block => ",predblk",
            brp_core::FilterChoice::Auto => ",auto",
        };
        let c = match self.coder {
            brp_core::CoderChoice::Fixed => "",
            brp_core::CoderChoice::Rice => ",rice",
            brp_core::CoderChoice::Context => ",ctxrice",
            brp_core::CoderChoice::Auto => ",bestcoder",
        };
        format!("brp[{block}{f}{c}]{}", self.entropy.suffix())
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        let opts = brp_core::EncodeOptions {
            block_size: self.block.map(|n| (n, n)),
            filter: self.filter,
            coder: self.coder,
            ..Default::default()
        };
        let raw = brp_core::encode(img, &opts).map_err(|e| anyhow::anyhow!(e))?;
        self.entropy.pack(&raw)
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        let raw = self.entropy.unpack(bytes)?;
        brp_core::decode(&raw).map_err(|e| anyhow::anyhow!(e))
    }
}

/// Adaptive block splitting, optionally followed by an entropy stage.
pub struct Quadtree {
    pub min_leaf: u32,
    pub entropy: Entropy,
}

impl Codec for Quadtree {
    fn name(&self) -> String {
        format!("quadtree[>={}]{}", self.min_leaf, self.entropy.suffix())
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        self.entropy.pack(&quadtree::encode(img, self.min_leaf))
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        quadtree::decode(&self.entropy.unpack(bytes)?)
    }
}

/// Raw interleaved samples through an entropy stage. The floor a codec must beat to justify itself.
pub struct Raw {
    pub entropy: Entropy,
}

impl Codec for Raw {
    fn name(&self) -> String {
        format!("raw{}", self.entropy.suffix())
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(9 + img.data().len());
        out.extend_from_slice(&img.width().to_le_bytes());
        out.extend_from_slice(&img.height().to_le_bytes());
        out.push(img.channels());
        out.extend_from_slice(&self.entropy.pack(img.data())?);
        Ok(out)
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        anyhow::ensure!(bytes.len() >= 9, "raw stream is shorter than its header");
        let w = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let h = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let ch = bytes[8];
        let data = self.entropy.unpack(&bytes[9..])?;
        RawImage::new(w, h, ch, data).map_err(|e| anyhow::anyhow!(e))
    }
}

/// PNG's own approach: per-row prediction filters, then an entropy stage. With `Entropy::Deflate`
/// this is essentially PNG minus the chunk framing, and is the number BRP has to beat.
pub struct Filtered {
    pub entropy: Entropy,
}

impl Codec for Filtered {
    fn name(&self) -> String {
        format!("filter{}", self.entropy.suffix())
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(9 + img.data().len());
        out.extend_from_slice(&img.width().to_le_bytes());
        out.extend_from_slice(&img.height().to_le_bytes());
        out.push(img.channels());
        out.extend_from_slice(&self.entropy.pack(&filters::apply(img))?);
        Ok(out)
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        anyhow::ensure!(
            bytes.len() >= 9,
            "filtered stream is shorter than its header"
        );
        let w = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let h = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let ch = bytes[8];
        let filtered = self.entropy.unpack(&bytes[9..])?;
        filters::undo(&filtered, w, h, ch)
    }
}

/// Prediction first, then BRP's range packing over the residuals.
///
/// The decisive experiment once the photograph numbers arrive: prediction and range packing both
/// exploit local similarity, so this asks whether they compose or merely overlap.
pub struct FilteredBrp {
    pub block: Option<u32>,
    pub entropy: Entropy,
    /// Zigzag the residuals first, so sign does not blow up a block's range.
    pub zigzag: bool,
}

impl Codec for FilteredBrp {
    fn name(&self) -> String {
        let block = match self.block {
            None => "whole".to_string(),
            Some(n) => format!("{n}x{n}"),
        };
        let zz = if self.zigzag { "+zigzag" } else { "" };
        format!("filter{zz}+brp[{block}]{}", self.entropy.suffix())
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        let (types, residuals) = if self.zigzag {
            filters::split_zigzag(img)
        } else {
            filters::split(img)
        };
        let opts = brp_core::EncodeOptions {
            block_size: self.block.map(|n| (n, n)),
            filter: brp_core::FilterChoice::Off,
            coder: brp_core::CoderChoice::Fixed,
            ..Default::default()
        };
        let packed = brp_core::encode(&residuals, &opts).map_err(|e| anyhow::anyhow!(e))?;

        let mut body = Vec::with_capacity(types.len() + packed.len());
        body.extend_from_slice(&types);
        body.extend_from_slice(&packed);

        let mut out = (types.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&self.entropy.pack(&body)?);
        Ok(out)
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        anyhow::ensure!(bytes.len() >= 4, "stream is shorter than its header");
        let rows = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let body = self.entropy.unpack(&bytes[4..])?;
        anyhow::ensure!(body.len() >= rows, "stream is missing its filter types");
        let residuals = brp_core::decode(&body[rows..]).map_err(|e| anyhow::anyhow!(e))?;
        if self.zigzag {
            filters::join_zigzag(&body[..rows], &residuals)
        } else {
            filters::join(&body[..rows], &residuals)
        }
    }
}

/// Range-shaped prediction, then BRP. The variant study behind roadmap item 1.
pub struct Predicted {
    pub options: predict::PredictOptions,
    pub block: Option<u32>,
    pub entropy: Entropy,
}

impl Codec for Predicted {
    fn name(&self) -> String {
        let block = match self.block {
            None => "whole".to_string(),
            Some(n) => format!("{n}x{n}"),
        };
        let h = match self.options.heuristic {
            predict::Heuristic::Sad => "sad",
            predict::Heuristic::MaxAbs => "max",
        };
        let sc = match self.options.scope {
            predict::Scope::SharedRow => "row",
            predict::Scope::PerChannel => "chan",
        };
        format!("pred[{h},{sc}]+brp[{block}]{}", self.entropy.suffix())
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        let (kinds, residuals) = predict::apply(img, &self.options);
        let opts = brp_core::EncodeOptions {
            block_size: self.block.map(|n| (n, n)),
            filter: brp_core::FilterChoice::Off,
            coder: brp_core::CoderChoice::Fixed,
            ..Default::default()
        };
        let packed = brp_core::encode(&residuals, &opts).map_err(|e| anyhow::anyhow!(e))?;

        let mut body = Vec::with_capacity(kinds.len() + packed.len());
        body.extend_from_slice(&kinds);
        body.extend_from_slice(&packed);

        let mut out = (kinds.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&self.entropy.pack(&body)?);
        Ok(out)
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        anyhow::ensure!(bytes.len() >= 4, "stream is shorter than its header");
        let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let body = self.entropy.unpack(&bytes[4..])?;
        anyhow::ensure!(body.len() >= n, "stream is missing its filter kinds");
        let residuals = brp_core::decode(&body[n..]).map_err(|e| anyhow::anyhow!(e))?;
        predict::undo(&body[..n], &residuals)
    }
}

/// An alternative block coder over the format's own residuals. See [`blockpack`].
pub struct Packed {
    pub coder: blockpack::BlockCoder,
    pub block: u32,
    pub predictor: Option<predictors::Variant>,
    pub entropy: Entropy,
}

impl Codec for Packed {
    fn name(&self) -> String {
        let p = match self.predictor {
            None => String::new(),
            Some(v) if v == predictors::Variant::shipped() => ",pred".to_string(),
            Some(v) => format!(",{}", v.name()),
        };
        format!(
            "{}[{}x{}{p}]{}",
            self.coder.name(),
            self.block,
            self.block,
            self.entropy.suffix()
        )
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        let opts = blockpack::Options {
            coder: self.coder,
            block: self.block,
            predictor: self.predictor,
        };
        self.entropy.pack(&blockpack::encode(img, &opts))
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        blockpack::decode(&self.entropy.unpack(bytes)?)
    }
}

/// A contextual Rice parameter over the format's own residuals. See [`context`].
pub struct Contextual {
    pub options: context::Options,
    pub entropy: Entropy,
}

impl Codec for Contextual {
    fn name(&self) -> String {
        format!("{}{}", self.options.name(), self.entropy.suffix())
    }

    fn encode(&self, img: &RawImage) -> Result<Vec<u8>> {
        self.entropy.pack(&context::encode(img, &self.options))
    }

    fn decode(&self, bytes: &[u8]) -> Result<RawImage> {
        context::decode(&self.entropy.unpack(bytes)?)
    }
}

/// The pipelines the runner measures, in report order.
pub fn all_codecs() -> Vec<Box<dyn Codec>> {
    use blockpack::BlockCoder;
    use brp_core::{CoderChoice, FilterChoice};

    let mut v: Vec<Box<dyn Codec>> = vec![
        // The format as it stands, and what each stage contributes.
        Box::new(Brp {
            block: None,
            entropy: Entropy::None,
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
        }),
        Box::new(Brp {
            block: Some(8),
            entropy: Entropy::None,
            filter: FilterChoice::Off,
            coder: CoderChoice::Fixed,
        }),
        Box::new(Brp {
            block: Some(8),
            entropy: Entropy::None,
            filter: FilterChoice::Auto,
            coder: CoderChoice::Fixed,
        }),
        Box::new(Brp {
            block: Some(8),
            entropy: Entropy::None,
            filter: FilterChoice::Auto,
            coder: CoderChoice::Auto,
        }),
        Box::new(Brp {
            block: Some(8),
            entropy: Entropy::Deflate,
            filter: FilterChoice::Auto,
            coder: CoderChoice::Auto,
        }),
        // Adaptive block size, still a candidate.
        Box::new(Quadtree {
            min_leaf: 2,
            entropy: Entropy::None,
        }),
        // References: no image model, and what PNG actually does.
        Box::new(Raw {
            entropy: Entropy::Deflate,
        }),
        Box::new(Filtered {
            entropy: Entropy::Deflate,
        }),
        Box::new(Filtered {
            entropy: Entropy::Huffman,
        }),
    ];

    // Roadmap item 1: which coder suits the residuals we actually produce?
    for coder in [
        BlockCoder::Fixed,
        BlockCoder::Pfor,
        BlockCoder::Rice,
        BlockCoder::Hybrid,
    ] {
        v.push(Box::new(Packed {
            coder,
            block: 8,
            predictor: Some(predictors::Variant::shipped()),
            entropy: Entropy::None,
        }));
    }
    // The best of them with a dictionary stage, to see how much Deflate still adds.
    v.push(Box::new(Packed {
        coder: BlockCoder::Rice,
        block: 8,
        predictor: Some(predictors::Variant::shipped()),
        entropy: Entropy::Deflate,
    }));
    // And Rice without prediction, to separate the two contributions.
    v.push(Box::new(Packed {
        coder: BlockCoder::Rice,
        block: 8,
        predictor: None,
        entropy: Entropy::None,
    }));

    // The context coder as the format now defines it, at the block size ADR 0009 measured it at.
    // `Auto` never selects it, so it has to be asked for by name.
    v.push(Box::new(Brp {
        block: Some(32),
        entropy: Entropy::None,
        filter: FilterChoice::Auto,
        coder: CoderChoice::Context,
    }));

    // And the prototype it came from, in the domain the format did *not* adopt, so the 0.18 points
    // ADR 0009 turned down stay measurable rather than becoming a claim in a document.
    v.push(Box::new(Contextual {
        options: context::Options {
            source: context::ContextSource::Sample,
            block: Some(32),
            base: false,
            per_channel: false,
            ..Default::default()
        },
        entropy: Entropy::None,
    }));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, ch: u8, f: impl Fn(u32, u32, u8) -> u8) -> RawImage {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                for c in 0..ch {
                    data.push(f(x, y, c));
                }
            }
        }
        RawImage::new(w, h, ch, data).unwrap()
    }

    /// The property that makes every number the runner prints meaningful.
    #[test]
    fn every_pipeline_is_lossless() {
        let images = [
            image(23, 17, 3, |x, y, c| (x * 7 + y * 3 + u32::from(c)) as u8),
            image(
                16,
                16,
                4,
                |x, y, c| if c == 3 { 255 } else { (x ^ y) as u8 },
            ),
            image(9, 5, 1, |_, _, _| 200),
            image(1, 1, 3, |_, _, c| 10 + c),
            image(31, 3, 3, |x, y, c| {
                let mut v = x.wrapping_mul(2_654_435_761).wrapping_add(y);
                v ^= v >> 11;
                (v.wrapping_add(u32::from(c))) as u8
            }),
        ];

        for codec in all_codecs() {
            for img in &images {
                let bytes = codec
                    .encode(img)
                    .unwrap_or_else(|e| panic!("{} failed to encode: {e}", codec.name()));
                let back = codec
                    .decode(&bytes)
                    .unwrap_or_else(|e| panic!("{} failed to decode: {e}", codec.name()));
                assert_eq!(&back, img, "{} is not lossless", codec.name());
            }
        }
    }

    #[test]
    fn deflate_round_trips() {
        let data: Vec<u8> = (0..5000).map(|i| (i % 40) as u8).collect();
        assert_eq!(inflate(&deflate(&data).unwrap()).unwrap(), data);
        assert_eq!(inflate(&deflate(&[]).unwrap()).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn every_pipeline_has_a_distinct_name() {
        let names: Vec<String> = all_codecs().iter().map(|c| c.name()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "duplicate pipeline name in {names:?}"
        );
    }
}
