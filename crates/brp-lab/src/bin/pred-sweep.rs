//! Roadmap item 1: is there a better predictor than a per-row choice among PNG's five?
//!
//! Two candidates from the lossless-coding literature, measured against the shipped choice under
//! *both* block coders, because a predictor is the one stage that helps all of them:
//!
//! - **MED** (LOCO-I/JPEG-LS) and **GAP** (CALIC), each a fixed rule that adapts at every sample
//!   and signals nothing at all.
//! - The same five as today, but chosen **per block** rather than per row.
//! - A **menu of seven** — the five plus MED and GAP — chosen per row or per block. Three bits
//!   still covers it, so this row costs exactly what the format already pays.
//!
//! Every table quotes `png5/row` first: that variant reproduces `brp_core::apply_prediction` bit
//! for bit, so it is the control, and the difference between it and the format's own row is the
//! prototype's framing rather than anything a predictor did.
//!
//! Sizes are exact byte counts and need one run. Throughput follows finding 12's discipline:
//! rounds interleaved across every subject, the best round reported, the spread beside it.
//!
//! Usage: `cargo run -p brp-lab --release --bin pred-sweep -- samples/`
//! `PRED_TIME=0` skips the timing table and `PRED_SHORT=1` trims the size tables to the rows that
//! matter, which is how this runs on a corpus of one 130 MiB photograph. `PRED_SIZE=0` leaves only
//! the timing table, for when the sizes are already known and the machine is what changed.

use anyhow::{bail, Context, Result};
use brp_core::RawImage;
use brp_lab::blockpack::{self, BlockCoder};
use brp_lab::context::{self, ContextSource, Options};
use brp_lab::predictors::{self, Menu, Scope, Variant};
use brp_lab::{Codec, Entropy, Filtered};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Keep timing until this much has elapsed, so a fast pipeline is not measured as one clock tick.
const MIN_TIME: Duration = Duration::from_millis(1200);
/// Interleaved timing rounds. The best round per subject is reported, the spread alongside it.
const ROUNDS: usize = 3;

/// Where the stored-parameter coder is measured: finding 11's best fixed grid.
const RICE_BLOCK: u32 = 16;
/// Where the context coder is measured: finding 13's best fixed grid.
const CONTEXT_BLOCK: u32 = 32;

struct Corpus {
    name: &'static str,
    images: Vec<RawImage>,
    raw: usize,
}

/// Every predictor the sweep covers, control first.
///
/// `PRED_SHORT=1` keeps the control and the five rows the full sweep points at, which is what a
/// corpus of one 130 MiB photograph can be asked for in reasonable time.
fn candidates() -> Vec<Variant> {
    if std::env::var("PRED_SHORT").map(|v| v != "0").unwrap_or(false) {
        return vec![
            Variant::shipped(),
            Variant::Fixed(predictors::GAP),
            Variant::Choice {
                scope: Scope::Block(8),
                menu: Menu::Png5,
            },
            Variant::Choice {
                scope: Scope::Block(8),
                menu: Menu::Png6,
            },
            Variant::Choice {
                scope: Scope::Block(8),
                menu: Menu::Png7,
            },
            Variant::Choice {
                scope: Scope::Block(16),
                menu: Menu::Png5,
            },
        ];
    }
    let mut v = vec![
        Variant::shipped(),
        Variant::Fixed(1),
        Variant::Fixed(4),
        Variant::Fixed(predictors::MED),
        Variant::Fixed(predictors::GAP),
    ];
    // The two questions the fixed rows cannot answer: how small the unit of choice should be, and
    // whether GAP earns its place in the menu once MED is in it.
    for menu in [Menu::Png5, Menu::Png6, Menu::Png7] {
        for scope in [
            Scope::Row,
            Scope::Block(4),
            Scope::Block(8),
            Scope::Block(16),
            Scope::Block(32),
        ] {
            let variant = Variant::Choice { scope, menu };
            if variant != Variant::shipped() {
                v.push(variant);
            }
        }
    }
    v
}

/// The two coders a predictor has to help, each at the block size its own finding settled on.
#[derive(Clone, Copy)]
enum Coder {
    /// Golomb-Rice with the parameter stored per block-channel — format coder 1.
    Rice,
    /// Golomb-Rice with the parameter derived from a context — format coder 2.
    Context,
}

impl Coder {
    fn name(self) -> String {
        match self {
            Coder::Rice => format!("rice[{RICE_BLOCK}x{RICE_BLOCK}]"),
            Coder::Context => format!("ctx[resid,{CONTEXT_BLOCK}x{CONTEXT_BLOCK}]"),
        }
    }

    fn encode(self, img: &RawImage, variant: Variant) -> Vec<u8> {
        match self {
            Coder::Rice => blockpack::encode(
                img,
                &blockpack::Options {
                    coder: BlockCoder::Rice,
                    block: RICE_BLOCK,
                    predictor: Some(variant),
                },
            ),
            Coder::Context => context::encode(img, &self.context_options(variant)),
        }
    }

    fn decode(self, bytes: &[u8]) -> Result<RawImage> {
        match self {
            Coder::Rice => blockpack::decode(bytes),
            Coder::Context => context::decode(bytes),
        }
    }

    /// What ADR 0009 settled on: residual-domain contexts, no base, one shared model.
    fn context_options(self, variant: Variant) -> Options {
        Options {
            source: ContextSource::Residual,
            block: Some(CONTEXT_BLOCK),
            predictor: Some(variant),
            base: false,
            per_channel: false,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Size
// ---------------------------------------------------------------------------------------------

/// Size of one predictor under one coder, refusing to report anything that is not lossless.
fn measure(corpus: &Corpus, coder: Coder, variant: Variant) -> Result<usize> {
    let mut total = 0;
    for img in &corpus.images {
        let bytes = coder.encode(img, variant);
        let back = coder
            .decode(&bytes)
            .with_context(|| format!("{} under {}", variant.name(), coder.name()))?;
        if &back != img {
            bail!("{} under {} is not lossless", variant.name(), coder.name());
        }
        total += bytes.len();
    }
    Ok(total)
}

fn percent(bytes: usize, raw: usize) -> f64 {
    100.0 * bytes as f64 / raw as f64
}

fn header(corpora: &[Corpus], first: &str) {
    let mut cells = String::new();
    for c in corpora {
        cells.push_str(&format!("  {:>8}", c.name));
    }
    println!("  {first:<34}{cells}");
}

fn rule(corpora: &[Corpus]) {
    println!("  {:-<1$}", "", 34 + corpora.len() * 10);
}

fn row(label: &str, cells: String) {
    println!("  {label:<34}{cells}");
}

fn baseline_options(block: u32, coder: brp_core::CoderChoice) -> brp_core::EncodeOptions {
    brp_core::EncodeOptions {
        block_size: Some((block, block)),
        filter: brp_core::FilterChoice::Auto,
        coder,
        ..Default::default()
    }
}

fn baseline_size(corpus: &Corpus, opts: &brp_core::EncodeOptions) -> Result<usize> {
    let mut total = 0;
    for img in &corpus.images {
        total += brp_core::encode(img, opts)
            .map_err(|e| anyhow::anyhow!(e))?
            .len();
    }
    Ok(total)
}

fn deflate_size(corpus: &Corpus) -> Result<usize> {
    let codec = Filtered {
        entropy: Entropy::Deflate,
    };
    let mut total = 0;
    for img in &corpus.images {
        total += codec.encode(img)?.len();
    }
    Ok(total)
}

// ---------------------------------------------------------------------------------------------
// What the menu actually picks
// ---------------------------------------------------------------------------------------------

/// Share of samples each kind ends up governing, when the seven-kind menu is offered.
fn kind_shares(corpus: &Corpus, variant: Variant) -> [f64; predictors::KINDS as usize] {
    let mut samples = [0u64; predictors::KINDS as usize];
    for img in &corpus.images {
        let stride = usize::from(img.channels());
        let plan = brp_core::plan_channels(
            img.data(),
            img.channels(),
            &brp_core::ChannelOptions::default(),
        );
        let coded = plan.coded_indices();
        if coded.is_empty() {
            continue;
        }
        let (kinds, _) = predictors::apply(
            variant,
            img.data(),
            img.width(),
            img.height(),
            stride,
            &coded,
        );
        // Every unit covers the same count except at the right and bottom edges; close enough to
        // weight by unit, and exact for the per-row variant.
        let per_unit = (img.width() as u64 * img.height() as u64) / kinds.len().max(1) as u64;
        for &k in &kinds {
            samples[usize::from(k)] += per_unit;
        }
    }
    let total: u64 = samples.iter().sum();
    let mut out = [0.0; predictors::KINDS as usize];
    if total > 0 {
        for (o, s) in out.iter_mut().zip(samples) {
            *o = 100.0 * s as f64 / total as f64;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Throughput
// ---------------------------------------------------------------------------------------------

/// Runs `f` until [`MIN_TIME`] has passed, returning throughput over the bytes it covered.
fn rate(bytes: usize, mut f: impl FnMut()) -> f64 {
    let start = Instant::now();
    let mut runs = 0u32;
    while start.elapsed() < MIN_TIME {
        f();
        runs += 1;
    }
    (bytes as f64 * f64::from(runs)) / start.elapsed().as_secs_f64() / (1024.0 * 1024.0)
}

/// Best rate over the rounds, and how far the worst round fell below it, in percent.
fn summarise(rates: &[f64]) -> (f64, f64) {
    let best = rates.iter().copied().fold(f64::MIN, f64::max);
    let worst = rates.iter().copied().fold(f64::MAX, f64::min);
    (best, 100.0 * (best - worst) / best)
}

struct Subject {
    label: String,
    coder: Coder,
    variant: Variant,
    streams: Vec<Vec<u8>>,
}

/// Times every subject with the rounds interleaved, so drift lands on all of them alike.
fn throughput_table(corpus: &Corpus, timed: &[(Coder, Variant)]) {
    let subjects: Vec<Subject> = timed
        .iter()
        .map(|&(coder, variant)| Subject {
            label: format!("{} + {}", coder.name(), variant.name()),
            coder,
            variant,
            streams: corpus
                .images
                .iter()
                .map(|img| coder.encode(img, variant))
                .collect(),
        })
        .collect();

    let mut encode = vec![Vec::new(); subjects.len()];
    let mut decode = vec![Vec::new(); subjects.len()];
    for _ in 0..ROUNDS {
        for (i, s) in subjects.iter().enumerate() {
            encode[i].push(rate(corpus.raw, || {
                for img in &corpus.images {
                    std::hint::black_box(s.coder.encode(img, s.variant));
                }
            }));
            decode[i].push(rate(corpus.raw, || {
                for b in &s.streams {
                    std::hint::black_box(s.coder.decode(b).unwrap());
                }
            }));
        }
    }

    println!("  {}, MiB/s over raw samples", corpus.name);
    println!(
        "  {:<42}  {:>12}  {:>12}",
        "configuration", "encode", "decode"
    );
    println!("  {:-<1$}", "", 72);
    for (i, s) in subjects.iter().enumerate() {
        let (e, es) = summarise(&encode[i]);
        let (d, ds) = summarise(&decode[i]);
        println!(
            "  {:<42}  {:>12}  {:>12}",
            s.label,
            format!("{e:.0} +-{es:.0}%"),
            format!("{d:.0} +-{ds:.0}%")
        );
    }
    println!();
}

/// The same question asked of the format itself: what does mode 2 cost the shipped decoder?
///
/// The prototypes above answer it for prototypes, and `cargo bench` answers it for two synthetic
/// 512x512 images where prediction is a large share of a fast decode. Neither is the photograph
/// corpus going through `brp-core`, which is what a caller would actually run.
fn shipped_throughput_table(corpus: &Corpus) {
    let mut configs: Vec<(String, brp_core::EncodeOptions)> = [
        (RICE_BLOCK, brp_core::CoderChoice::Auto),
        (CONTEXT_BLOCK, brp_core::CoderChoice::Context),
    ]
    .into_iter()
    .flat_map(|(block, coder)| {
        [
            (brp_core::FilterChoice::Row, "pred"),
            (brp_core::FilterChoice::Block, "predblk"),
        ]
        .into_iter()
        .map(move |(filter, name)| {
            let c = match coder {
                brp_core::CoderChoice::Context => "ctxrice",
                _ => "bestcoder",
            };
            (
                format!("brp[{block}x{block},{name},{c}]"),
                brp_core::EncodeOptions {
                    block_size: Some((block, block)),
                    filter,
                    coder,
                    ..Default::default()
                },
            )
        })
    })
    .collect();

    // What README.md quotes: the documented block size at default settings, and the same with the
    // context coder asked for by name. Both leave the filter to `Auto`, which is the point.
    for (block, coder, label) in [
        (8, brp_core::CoderChoice::Auto, "brp[8x8,auto,bestcoder]"),
        (
            CONTEXT_BLOCK,
            brp_core::CoderChoice::Context,
            "brp[32x32,auto,ctxrice]",
        ),
    ] {
        configs.push((
            label.to_string(),
            brp_core::EncodeOptions {
                block_size: Some((block, block)),
                filter: brp_core::FilterChoice::Auto,
                coder,
                ..Default::default()
            },
        ));
    }

    let streams: Vec<Vec<Vec<u8>>> = configs
        .iter()
        .map(|(_, opts)| {
            corpus
                .images
                .iter()
                .map(|img| brp_core::encode(img, opts).unwrap())
                .collect()
        })
        .collect();

    let mut encode = vec![Vec::new(); configs.len()];
    let mut decode = vec![Vec::new(); configs.len()];
    for _ in 0..ROUNDS {
        for (i, (_, opts)) in configs.iter().enumerate() {
            encode[i].push(rate(corpus.raw, || {
                for img in &corpus.images {
                    std::hint::black_box(brp_core::encode(img, opts).unwrap());
                }
            }));
            decode[i].push(rate(corpus.raw, || {
                for b in &streams[i] {
                    std::hint::black_box(brp_core::decode(b).unwrap());
                }
            }));
        }
    }

    // PNG's approach, timed in the same rounds, because a ratio quoted against it has to be.
    let deflate = Filtered {
        entropy: Entropy::Deflate,
    };
    let deflate_streams: Vec<Vec<u8>> = corpus
        .images
        .iter()
        .map(|img| deflate.encode(img).unwrap())
        .collect();
    let mut deflate_encode = Vec::new();
    let mut deflate_decode = Vec::new();
    for _ in 0..ROUNDS {
        deflate_encode.push(rate(corpus.raw, || {
            for img in &corpus.images {
                std::hint::black_box(deflate.encode(img).unwrap());
            }
        }));
        deflate_decode.push(rate(corpus.raw, || {
            for b in &deflate_streams {
                std::hint::black_box(deflate.decode(b).unwrap());
            }
        }));
    }

    println!("  {}, the format itself, MiB/s over raw samples", corpus.name);
    println!(
        "  {:<42}  {:>12}  {:>12}  {:>8}",
        "configuration", "encode", "decode", "size"
    );
    println!("  {:-<1$}", "", 82);
    for (i, (label, _)) in configs.iter().enumerate() {
        let (e, es) = summarise(&encode[i]);
        let (d, ds) = summarise(&decode[i]);
        let bytes: usize = streams[i].iter().map(|b| b.len()).sum();
        println!(
            "  {:<42}  {:>12}  {:>12}  {:>7.2}%",
            label,
            format!("{e:.0} +-{es:.0}%"),
            format!("{d:.0} +-{ds:.0}%"),
            percent(bytes, corpus.raw)
        );
    }
    let (e, es) = summarise(&deflate_encode);
    let (d, ds) = summarise(&deflate_decode);
    let bytes: usize = deflate_streams.iter().map(|b| b.len()).sum();
    println!(
        "  {:<42}  {:>12}  {:>12}  {:>7.2}%",
        "filter+deflate",
        format!("{e:.0} +-{es:.0}%"),
        format!("{d:.0} +-{ds:.0}%"),
        percent(bytes, corpus.raw)
    );
    println!();
}

// ---------------------------------------------------------------------------------------------

fn main() -> Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "samples".to_string())
        .into();

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && brp_imageio::is_supported_image(p))
        .collect();
    paths.sort();
    if paths.is_empty() {
        bail!("no images in {}", dir.display());
    }

    // The split is the point: the synthetic corpus flatters block packing and has, before now,
    // pointed at the wrong design.
    let mut photographs = Vec::new();
    let mut synthetic = Vec::new();
    for p in &paths {
        let img = brp_imageio::load(p)?.image;
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        // `gen-samples` makes nothing above a megapixel, so anything this large is a photograph
        // whatever it is called — which is how `samples/large` reports as one.
        let megapixels = img.width() as u64 * img.height() as u64;
        if name.starts_with("photo-kodim") || megapixels > 4_000_000 {
            photographs.push(img);
        } else {
            synthetic.push(img);
        }
    }

    let mut corpora = Vec::new();
    if !photographs.is_empty() {
        corpora.push(Corpus {
            raw: photographs.iter().map(|i| i.data().len()).sum(),
            images: photographs,
            name: "photos",
        });
    }
    if !synthetic.is_empty() {
        corpora.push(Corpus {
            raw: synthetic.iter().map(|i| i.data().len()).sum(),
            images: synthetic,
            name: "synth",
        });
    }
    for c in &corpora {
        println!(
            "{}: {} images, {:.1} MiB of raw samples",
            c.name,
            c.images.len(),
            c.raw as f64 / (1024.0 * 1024.0)
        );
    }
    println!();

    // What the format itself produces at the same two grids, so a prototype row can be read
    // against something shipped rather than only against other prototypes.
    println!("Baselines from the format and from PNG's approach");
    rule(&corpora);
    header(&corpora, "pipeline");
    for (label, opts) in [
        (
            // The configuration every table in EXPERIMENTS.md quotes.
            "brp[8x8,auto,bestcoder]".to_string(),
            baseline_options(8, brp_core::CoderChoice::Auto),
        ),
        (
            format!("brp[{RICE_BLOCK}x{RICE_BLOCK},auto,bestcoder]"),
            baseline_options(RICE_BLOCK, brp_core::CoderChoice::Auto),
        ),
        (
            format!("brp[{CONTEXT_BLOCK}x{CONTEXT_BLOCK},auto,ctxrice]"),
            baseline_options(CONTEXT_BLOCK, brp_core::CoderChoice::Context),
        ),
    ] {
        let mut cells = String::new();
        for c in &corpora {
            cells.push_str(&format!("  {:>7.2}%", percent(baseline_size(c, &opts)?, c.raw)));
        }
        row(&label, cells);
    }
    let mut cells = String::new();
    for c in &corpora {
        cells.push_str(&format!("  {:>7.2}%", percent(deflate_size(c)?, c.raw)));
    }
    row("filter+deflate", cells);
    println!();

    let sizes = std::env::var("PRED_SIZE").map(|v| v != "0").unwrap_or(true);
    for coder in [Coder::Rice, Coder::Context] {
        if !sizes {
            break;
        }
        println!("Predictors under {}", coder.name());
        rule(&corpora);
        header(&corpora, "predictor");
        for variant in candidates() {
            let mut cells = String::new();
            for c in &corpora {
                cells.push_str(&format!(
                    "  {:>7.2}%",
                    percent(measure(c, coder, variant)?, c.raw)
                ));
            }
            let label = if variant == Variant::shipped() {
                format!("{} (control)", variant.name())
            } else {
                variant.name()
            };
            row(&label, cells);
        }
        println!();
    }

    // A predictor that never gets picked is not a predictor the format needs.
    if sizes {
    println!("What the seven-kind menu picks, in percent of samples covered");
    println!(
        "  {:<20}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}{:>8}",
        "corpus / scope", "none", "sub", "up", "avg", "paeth", "med", "gap"
    );
    println!("  {:-<1$}", "", 76);
    for c in &corpora {
        for scope in [Scope::Row, Scope::Block(16)] {
            let variant = Variant::Choice {
                scope,
                menu: Menu::Png7,
            };
            let shares = kind_shares(c, variant);
            let mut cells = String::new();
            for s in shares {
                cells.push_str(&format!("{s:>7.1} "));
            }
            println!("  {:<20}{cells}", format!("{} {}", c.name, variant.name()));
        }
    }
    println!();
    }

    if std::env::var("PRED_TIME").map(|v| v != "0").unwrap_or(true) {
        println!("Throughput");
        println!("  Best of {ROUNDS} interleaved rounds; +- is how far the worst round fell short.");
        println!("  A fixed predictor removes the encoder's search and changes the decoder's");
        println!("  unprediction pass; nothing else about either coder moves.\n");
        let timed: Vec<(Coder, Variant)> = [Coder::Rice, Coder::Context]
            .into_iter()
            .flat_map(|coder| {
                [
                    Variant::shipped(),
                    Variant::Fixed(predictors::MED),
                    Variant::Fixed(predictors::GAP),
                    Variant::Choice {
                        scope: Scope::Block(8),
                        menu: Menu::Png5,
                    },
                    Variant::Choice {
                        scope: Scope::Block(8),
                        menu: Menu::Png6,
                    },
                    Variant::Choice {
                        scope: Scope::Block(8),
                        menu: Menu::Png7,
                    },
                ]
                .into_iter()
                .map(move |v| (coder, v))
            })
            .collect();
        for c in &corpora {
            throughput_table(c, &timed);
            shipped_throughput_table(c);
        }
    }

    println!("Every size above decoded back to the source pixels before it was reported.");
    Ok(())
}
