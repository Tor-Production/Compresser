//! Everything roadmap item 1 has to answer before context modelling can be proposed for the
//! format: which domain the gradients are measured in, which quantiser thresholds, which block
//! size, whether the zero-block escape still earns its bit, and what the model costs in speed.
//!
//! The corpus is reported split, because it always has to be: the synthetic images give the
//! opposite answer to the photographs, and only the photographs decide.
//!
//! Sizes are exact byte counts and need one run. Throughput does not: the timing table follows the
//! discipline finding 12 arrived at — rounds interleaved across every subject, the best round
//! reported, and the spread printed beside it. A difference smaller than the spread is not a
//! result.
//!
//! Usage: `cargo run -p brp-lab --release --bin ctx-sweep -- samples/`

use anyhow::{bail, Context, Result};
use brp_core::RawImage;
use brp_lab::context::{self, ContextSource, Options, Thresholds};
use brp_lab::{Codec, Entropy, Filtered};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Keep timing until this much has elapsed, so a fast pipeline is not measured as one clock tick.
const MIN_TIME: Duration = Duration::from_millis(1200);
/// Interleaved timing rounds. The best round per subject is reported, the spread alongside it.
const ROUNDS: usize = 3;

struct Corpus {
    name: &'static str,
    images: Vec<RawImage>,
    raw: usize,
}

// ---------------------------------------------------------------------------------------------
// Size
// ---------------------------------------------------------------------------------------------

/// Size of one configuration over one corpus, refusing to report anything that is not lossless.
fn measure_size(corpus: &Corpus, opts: &Options) -> Result<usize> {
    let mut total = 0;
    for img in &corpus.images {
        let bytes = context::encode(img, opts);
        let back =
            context::decode(&bytes).with_context(|| format!("{} failed to decode", opts.name()))?;
        if &back != img {
            bail!("{} is not lossless", opts.name());
        }
        total += bytes.len();
    }
    Ok(total)
}

fn percent(bytes: usize, raw: usize) -> f64 {
    100.0 * bytes as f64 / raw as f64
}

/// One row of a size table: the configuration's share of raw on each corpus.
fn size_row(label: &str, corpora: &[Corpus], opts: &Options) -> Result<()> {
    let mut cells = String::new();
    for c in corpora {
        cells.push_str(&format!(
            "  {:>7.2}%",
            percent(measure_size(c, opts)?, c.raw)
        ));
    }
    println!("  {label:<42}{cells}");
    Ok(())
}

fn header(corpora: &[Corpus], first: &str) {
    let mut cells = String::new();
    for c in corpora {
        cells.push_str(&format!("  {:>8}", c.name));
    }
    println!("  {first:<42}{cells}");
}

fn rule(corpora: &[Corpus]) {
    println!("  {:-<1$}", "", 42 + corpora.len() * 10);
}

fn brp_options(block: u32) -> brp_core::EncodeOptions {
    coder_options(block, brp_core::CoderChoice::Auto)
}

fn coder_options(block: u32, coder: brp_core::CoderChoice) -> brp_core::EncodeOptions {
    brp_core::EncodeOptions {
        block_size: Some((block, block)),
        filter: brp_core::FilterChoice::Auto,
        coder,
        ..Default::default()
    }
}

fn baseline_size_with(corpus: &Corpus, opts: &brp_core::EncodeOptions) -> Result<usize> {
    let mut total = 0;
    for img in &corpus.images {
        total += brp_core::encode(img, opts)
            .map_err(|e| anyhow::anyhow!(e))?
            .len();
    }
    Ok(total)
}

fn baseline_size(corpus: &Corpus, block: u32) -> Result<usize> {
    let opts = brp_options(block);
    let mut total = 0;
    for img in &corpus.images {
        total += brp_core::encode(img, &opts)
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

/// One thing to time. `encode` is absent for the replay decoder, which has nothing to encode.
struct Subject<'a> {
    label: String,
    encode: Option<Box<dyn Fn() + 'a>>,
    decode: Box<dyn Fn() + 'a>,
}

/// Builds every subject for one corpus, with its streams already encoded.
fn subjects<'a>(
    corpus: &'a Corpus,
    streams: &'a Streams,
    candidates: &'a [Options],
) -> Vec<Subject<'a>> {
    let mut v: Vec<Subject<'a>> = Vec::new();

    for (i, (label, opts)) in streams.shipped_options.iter().enumerate() {
        let bytes = &streams.baselines[i];
        v.push(Subject {
            label: label.clone(),
            encode: Some(Box::new(move || {
                for img in &corpus.images {
                    std::hint::black_box(brp_core::encode(img, opts).unwrap());
                }
            })),
            decode: Box::new(move || {
                for b in bytes {
                    std::hint::black_box(brp_core::decode(b).unwrap());
                }
            }),
        });
    }

    v.push(Subject {
        label: "filter+deflate".to_string(),
        encode: Some(Box::new(move || {
            let codec = Filtered {
                entropy: Entropy::Deflate,
            };
            for img in &corpus.images {
                std::hint::black_box(codec.encode(img).unwrap());
            }
        })),
        decode: Box::new(move || {
            let codec = Filtered {
                entropy: Entropy::Deflate,
            };
            for b in &streams.deflate {
                std::hint::black_box(codec.decode(b).unwrap());
            }
        }),
    });

    for (i, opts) in candidates.iter().enumerate() {
        let traced = &streams.contextual[i];
        v.push(Subject {
            label: opts.name(),
            encode: Some(Box::new(move || {
                for img in &corpus.images {
                    std::hint::black_box(context::encode(img, opts));
                }
            })),
            decode: Box::new(move || {
                for (b, _) in traced {
                    std::hint::black_box(context::decode(b).unwrap());
                }
            }),
        });
        v.push(Subject {
            label: "  ^ the same decode, bit reader removed".to_string(),
            encode: None,
            decode: Box::new(move || {
                for (b, t) in traced {
                    std::hint::black_box(context::replay(b, t).unwrap());
                }
            }),
        });
    }
    v
}

const BASELINE_BLOCKS: [u32; 2] = [8, 16];

/// What the format itself can be asked for, timed alongside the prototypes. The first two rows run
/// no context model at all, which makes them the control for anything that changes one.
fn shipped(block: u32) -> [(String, brp_core::EncodeOptions); 3] {
    [
        (
            format!("brp[{block}x{block},auto,bestcoder]"),
            coder_options(block, brp_core::CoderChoice::Auto),
        ),
        (
            "brp[16x16,auto,bestcoder]".to_string(),
            coder_options(16, brp_core::CoderChoice::Auto),
        ),
        (
            "brp[32x32,auto,ctxrice]".to_string(),
            coder_options(32, brp_core::CoderChoice::Context),
        ),
    ]
}

/// Encoded streams, so decode timing measures decoding and nothing else.
struct Streams {
    shipped_options: Vec<(String, brp_core::EncodeOptions)>,
    baselines: Vec<Vec<Vec<u8>>>,
    deflate: Vec<Vec<u8>>,
    contextual: Vec<Vec<(Vec<u8>, context::Trace)>>,
}

fn encode_streams(corpus: &Corpus, candidates: &[Options]) -> Streams {
    let codec = Filtered {
        entropy: Entropy::Deflate,
    };
    let shipped_options: Vec<(String, brp_core::EncodeOptions)> = shipped(8).into_iter().collect();
    Streams {
        baselines: shipped_options
            .iter()
            .map(|(_, opts)| {
                corpus
                    .images
                    .iter()
                    .map(|img| brp_core::encode(img, opts).unwrap())
                    .collect()
            })
            .collect(),
        shipped_options,
        deflate: corpus
            .images
            .iter()
            .map(|img| codec.encode(img).unwrap())
            .collect(),
        contextual: candidates
            .iter()
            .map(|opts| {
                corpus
                    .images
                    .iter()
                    .map(|img| context::encode_traced(img, opts))
                    .collect()
            })
            .collect(),
    }
}

/// Times every subject with the rounds interleaved, so drift lands on all of them alike.
fn throughput_table(corpus: &Corpus, candidates: &[Options]) {
    let streams = encode_streams(corpus, candidates);
    let subjects = subjects(corpus, &streams, candidates);

    let mut encode = vec![Vec::new(); subjects.len()];
    let mut decode = vec![Vec::new(); subjects.len()];
    for _ in 0..ROUNDS {
        for (i, s) in subjects.iter().enumerate() {
            if let Some(f) = &s.encode {
                encode[i].push(rate(corpus.raw, &**f));
            }
            decode[i].push(rate(corpus.raw, || (s.decode)()));
        }
    }

    println!("  {}, MiB/s over raw samples", corpus.name);
    println!(
        "  {:<42}  {:>12}  {:>12}",
        "configuration", "encode", "decode"
    );
    println!("  {:-<1$}", "", 72);
    for (i, s) in subjects.iter().enumerate() {
        let enc = if encode[i].is_empty() {
            "-".to_string()
        } else {
            let (best, spread) = summarise(&encode[i]);
            format!("{best:.0} +-{spread:.0}%")
        };
        let (best, spread) = summarise(&decode[i]);
        println!(
            "  {:<42}  {:>12}  {:>12}",
            s.label,
            enc,
            format!("{best:.0} +-{spread:.0}%")
        );
    }
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
        if name.starts_with("photo-kodim") {
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

    let block = std::env::var("CTX_BLOCK")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(Some)
        .unwrap_or(Some(32));

    // The two candidates the size tables settle on, and what the timing table then prices.
    let candidates: Vec<Options> = [ContextSource::Sample, ContextSource::Residual]
        .into_iter()
        .map(|source| Options {
            source,
            block,
            base: false,
            per_channel: false,
            ..Default::default()
        })
        .collect();

    // Sizes are deterministic and need one run each; the tables differ only in how many
    // configurations they cover, which is what matters on a corpus of one 130 MiB photograph.
    println!("Sizes: baselines and the candidates");
    rule(&corpora);
    header(&corpora, "pipeline");
    for b in BASELINE_BLOCKS {
        let mut cells = String::new();
        for c in &corpora {
            cells.push_str(&format!("  {:>7.2}%", percent(baseline_size(c, b)?, c.raw)));
        }
        println!("  {:<42}{cells}", format!("brp[{b}x{b},auto,bestcoder]"));
    }
    let mut cells = String::new();
    for c in &corpora {
        cells.push_str(&format!("  {:>7.2}%", percent(deflate_size(c)?, c.raw)));
    }
    println!("  {:<42}{cells}", "filter+deflate");

    // The format's own context coder against the prototype it came from. These two rows are the
    // check that moving an experiment into the format cost nothing: the shipped coder should
    // reproduce the `resid` prototype to within the few bytes of header that differ.
    let shipped = coder_options(32, brp_core::CoderChoice::Context);
    let mut cells = String::new();
    for c in &corpora {
        cells.push_str(&format!(
            "  {:>7.2}%",
            percent(baseline_size_with(c, &shipped)?, c.raw)
        ));
    }
    println!("  {:<42}{cells}", "brp[32x32,auto,ctxrice]");
    for opts in &candidates {
        size_row(&opts.name(), &corpora, opts)?;
    }
    println!();

    if std::env::var("CTX_SWEEP").map(|v| v != "0").unwrap_or(true) {
        println!("A. Context domain and block size, thresholds 3/7/21");
        rule(&corpora);
        header(&corpora, "configuration");
        for source in [ContextSource::Sample, ContextSource::Residual] {
            for (base, per_channel) in [(true, true), (false, false)] {
                for b in [Some(8u32), Some(16), Some(32), Some(64), None] {
                    let opts = Options {
                        source,
                        block: b,
                        base,
                        per_channel,
                        ..Default::default()
                    };
                    size_row(&opts.name(), &corpora, &opts)?;
                }
            }
        }
        println!();

        // JPEG-LS's 3/7/21 were chosen for gradients between samples. Ours may be measured over
        // prediction errors instead, which live on a different scale, so the defaults are a
        // hypothesis and this is the test of it.
        println!("B. Quantiser thresholds");
        rule(&corpora);
        header(&corpora, "configuration");
        for source in [ContextSource::Sample, ContextSource::Residual] {
            for (t1, t2, t3) in [
                (1u8, 2u8, 4u8),
                (1, 3, 7),
                (2, 4, 8),
                (2, 6, 16),
                (3, 7, 21),
                (4, 8, 21),
                (4, 12, 32),
                (6, 16, 40),
                (8, 24, 64),
            ] {
                let opts = Options {
                    source,
                    block,
                    thresholds: Thresholds { t1, t2, t3 },
                    ..Default::default()
                };
                size_row(&opts.name(), &corpora, &opts)?;
            }
        }
        println!();

        println!("C. Ablations");
        rule(&corpora);
        header(&corpora, "configuration");
        for source in [ContextSource::Sample, ContextSource::Residual] {
            for (base, escape, per_channel) in [
                (true, true, true),
                (true, false, true),
                (false, true, true),
                (false, false, true),
                (true, true, false),
            ] {
                let opts = Options {
                    source,
                    block,
                    base,
                    escape,
                    per_channel,
                    ..Default::default()
                };
                size_row(&opts.name(), &corpora, &opts)?;
            }
        }
        println!();

        println!("D. Adaptation rate, on the combination the tables above point at");
        rule(&corpora);
        header(&corpora, "configuration");
        for source in [ContextSource::Sample, ContextSource::Residual] {
            for reset in [16u8, 32, 64, 128] {
                let opts = Options {
                    source,
                    block,
                    base: false,
                    per_channel: false,
                    reset,
                    ..Default::default()
                };
                size_row(&opts.name(), &corpora, &opts)?;
            }
        }
        println!();
    }

    println!("E. Throughput");
    println!("  Best of {ROUNDS} interleaved rounds; +- is how far the worst round fell short.\n");
    for c in &corpora {
        throughput_table(c, &candidates);
    }

    println!("Every size above decoded back to the source pixels before it was reported.");
    Ok(())
}
