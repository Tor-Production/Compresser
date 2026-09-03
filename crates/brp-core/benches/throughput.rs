//! Encode and decode throughput, measured in bytes of raw samples per second.
//!
//! Read these before optimizing anything. The hot loops are the min/max reduction in `encode` and
//! the bit pack/unpack in both directions; block size changes their balance, because small blocks
//! mean more scans and more header writes per pixel.

use brp_core::{decode, encode, EncodeOptions, RawImage};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

const SIZE: u32 = 512;

fn noise(x: u32, y: u32, c: u8) -> u8 {
    let mut v = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))
        .wrapping_add(u32::from(c).wrapping_mul(2_246_822_519));
    v ^= v >> 13;
    v = v.wrapping_mul(1_274_126_177);
    (v ^ (v >> 16)) as u8
}

fn build(channels: u8, f: impl Fn(u32, u32, u8) -> u8) -> RawImage {
    let mut data = Vec::with_capacity((SIZE * SIZE * u32::from(channels)) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            for c in 0..channels {
                data.push(f(x, y, c));
            }
        }
    }
    RawImage::new(SIZE, SIZE, channels, data).unwrap()
}

/// The two extremes of the bit-packing work: 8 bits per sample versus 4.
fn workloads() -> Vec<(&'static str, RawImage)> {
    vec![
        // Full-range noise: every width code is 8, so the packer moves the most bits possible.
        ("noise-rgb", build(3, noise)),
        // A narrow band: width code 4, so half the payload bits and a different loop balance.
        (
            "narrow-rgb",
            build(3, |x, y, c| {
                100 + ((x / 3 + y / 5 + u32::from(c) * 4) % 16) as u8
            }),
        ),
    ]
}

const BLOCKS: &[Option<u32>] = &[None, Some(16), Some(8)];

fn label(block: Option<u32>) -> String {
    match block {
        None => "whole".to_string(),
        Some(n) => format!("{n}x{n}"),
    }
}

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode");
    for (name, img) in workloads() {
        group.throughput(Throughput::Bytes(img.data().len() as u64));
        for &block in BLOCKS {
            let opts = EncodeOptions {
                block_size: block.map(|n| (n, n)),
                alpha_opt: true,
            };
            group.bench_with_input(BenchmarkId::new(name, label(block)), &opts, |b, opts| {
                b.iter(|| encode(black_box(&img), black_box(opts)).unwrap())
            });
        }
    }
    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");
    for (name, img) in workloads() {
        group.throughput(Throughput::Bytes(img.data().len() as u64));
        for &block in BLOCKS {
            let opts = EncodeOptions {
                block_size: block.map(|n| (n, n)),
                alpha_opt: true,
            };
            let bytes = encode(&img, &opts).unwrap();
            group.bench_with_input(BenchmarkId::new(name, label(block)), &bytes, |b, bytes| {
                b.iter(|| decode(black_box(bytes)).unwrap())
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_encode, bench_decode);
criterion_main!(benches);
