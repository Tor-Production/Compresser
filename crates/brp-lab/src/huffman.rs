//! Order-0 canonical Huffman coding over bytes.
//!
//! Written here rather than pulled from a crate because measuring it *is* the point: this is the
//! candidate for replacing BRP's fixed-width residual packing, and the comparison is only honest
//! if the header cost and the code-length limit are ours to see.
//!
//! Stream layout:
//!
//! ```text
//! u32 LE   symbol count of the original data
//! 256 B    code length per symbol, 0 for symbols that never occur
//! bits     the codes, MSB first, zero-padded to a byte
//! ```
//!
//! The 256-byte length table is deliberately uncompressed. Deflate spends real effort packing its
//! equivalent, and pretending we had done the same would flatter these numbers.

use anyhow::{bail, Result};
use brp_core::{BitReader, BitWriter};

/// Longest code we allow. Bounding it keeps the decoder's inner loop fixed-length and the code
/// table small; real data rarely reaches even 15.
const MAX_CODE_BITS: usize = 15;

const TABLE_BYTES: usize = 256;
const HEADER_BYTES: usize = 4 + TABLE_BYTES;

/// Huffman code lengths for every byte value, 0 where the byte never occurs.
fn code_lengths(data: &[u8]) -> [u8; 256] {
    let mut freq = [0u64; 256];
    for &b in data {
        freq[usize::from(b)] += 1;
    }

    // A code longer than MAX_CODE_BITS cannot be represented. Halving every frequency flattens the
    // distribution and shortens the longest code; repeating it always terminates, because once all
    // frequencies are equal the tree is balanced at 8 bits. The result is slightly suboptimal and
    // always valid, which is the right trade for a measurement harness.
    loop {
        let lengths = huffman_lengths(&freq);
        if lengths.iter().all(|&l| usize::from(l) <= MAX_CODE_BITS) {
            return lengths;
        }
        for f in freq.iter_mut() {
            if *f > 0 {
                *f = f.div_ceil(2);
            }
        }
    }
}

/// Classic Huffman tree construction, returning depth per symbol.
fn huffman_lengths(freq: &[u64; 256]) -> [u8; 256] {
    // Nodes: leaves 0..256, then internal nodes appended.
    let mut parent = vec![usize::MAX; 256];
    // (weight, node index), kept in a vector we scan for the two smallest. 256 symbols makes the
    // quadratic scan irrelevant next to the image data itself.
    let mut live: Vec<(u64, usize)> = (0..256)
        .filter(|&s| freq[s] > 0)
        .map(|s| (freq[s], s))
        .collect();

    let mut lengths = [0u8; 256];
    match live.len() {
        // Nothing to code.
        0 => return lengths,
        // One distinct symbol still needs a code; give it one bit.
        1 => {
            lengths[live[0].1] = 1;
            return lengths;
        }
        _ => {}
    }

    while live.len() > 1 {
        // Two smallest weights, ties broken by node index so the result is deterministic.
        live.sort_unstable();
        let (wa, a) = live.remove(0);
        let (wb, b) = live.remove(0);
        let node = parent.len();
        parent.push(usize::MAX);
        parent[a] = node;
        parent[b] = node;
        live.push((wa + wb, node));
    }

    for (s, len) in lengths.iter_mut().enumerate() {
        if freq[s] == 0 {
            continue;
        }
        let mut depth = 0u32;
        let mut node = s;
        while parent[node] != usize::MAX {
            node = parent[node];
            depth += 1;
        }
        *len = depth.min(255) as u8;
    }
    lengths
}

/// Canonical codes, in the order the length table implies.
fn canonical_codes(lengths: &[u8; 256]) -> [u32; 256] {
    let mut counts = [0u32; MAX_CODE_BITS + 1];
    for &l in lengths.iter() {
        if l > 0 {
            counts[usize::from(l)] += 1;
        }
    }
    let mut next = [0u32; MAX_CODE_BITS + 2];
    let mut code = 0u32;
    for (len, slot) in next.iter_mut().enumerate().take(MAX_CODE_BITS + 1).skip(1) {
        code = (code + counts[len - 1]) << 1;
        *slot = code;
    }
    // `counts[0]` is unused above; the loop above needs counts[len-1] for len=1 to be zero.
    let mut codes = [0u32; 256];
    for (s, &l) in lengths.iter().enumerate() {
        if l > 0 {
            codes[s] = next[usize::from(l)];
            next[usize::from(l)] += 1;
        }
    }
    codes
}

pub fn encode(data: &[u8]) -> Vec<u8> {
    let lengths = code_lengths(data);
    let codes = canonical_codes(&lengths);

    let mut out = Vec::with_capacity(HEADER_BYTES + data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&lengths);

    let mut w = BitWriter::with_capacity(data.len());
    for &b in data {
        let s = usize::from(b);
        w.write(codes[s], u32::from(lengths[s]));
    }
    out.extend_from_slice(&w.finish());
    out
}

pub fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < HEADER_BYTES {
        bail!("huffman stream is shorter than its header");
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let mut lengths = [0u8; 256];
    lengths.copy_from_slice(&bytes[4..HEADER_BYTES]);
    if lengths.iter().any(|&l| usize::from(l) > MAX_CODE_BITS) {
        bail!("huffman code length exceeds {MAX_CODE_BITS} bits");
    }

    // Symbols ordered by (length, symbol) — the order canonical codes are assigned in.
    let mut counts = [0u32; MAX_CODE_BITS + 1];
    let mut symbols: Vec<u8> = Vec::new();
    for (len, count) in counts.iter_mut().enumerate().skip(1) {
        for (s, &l) in lengths.iter().enumerate() {
            if usize::from(l) == len {
                *count += 1;
                symbols.push(s as u8);
            }
        }
    }
    if count > 0 && symbols.is_empty() {
        bail!("huffman stream has data but no codes");
    }

    let mut r = BitReader::new(&bytes[HEADER_BYTES..]);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        // Canonical decode: walk lengths, tracking the first code and first symbol index at each.
        let mut code = 0i64;
        let mut first = 0i64;
        let mut index = 0i64;
        let mut decoded = None;
        for &at_this_length in counts.iter().skip(1) {
            code |= i64::from(r.read(1).map_err(|e| anyhow::anyhow!(e))?);
            let n = i64::from(at_this_length);
            if code - first < n {
                decoded = Some(symbols[(index + code - first) as usize]);
                break;
            }
            index += n;
            first = (first + n) << 1;
            code <<= 1;
        }
        match decoded {
            Some(s) => out.push(s),
            None => bail!("huffman stream contains an invalid code"),
        }
    }
    Ok(out)
}

/// Size in bytes this coder would produce, without producing it. Used by the quadtree cost model.
pub fn estimated_len(data: &[u8]) -> usize {
    let lengths = code_lengths(data);
    let mut freq = [0u64; 256];
    for &b in data {
        freq[usize::from(b)] += 1;
    }
    let bits: u64 = (0..256)
        .map(|s| freq[s] * u64::from(lengths[s]))
        .sum::<u64>();
    HEADER_BYTES + bits.div_ceil(8) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(data: &[u8]) {
        let encoded = encode(data);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data, "round trip failed for {} bytes", data.len());
    }

    #[test]
    fn round_trips_ordinary_data() {
        round_trip(b"the quick brown fox jumps over the lazy dog");
        round_trip(&(0..=255u8).collect::<Vec<_>>());
        round_trip(&(0..4096).map(|i| (i * 7 % 251) as u8).collect::<Vec<_>>());
    }

    #[test]
    fn round_trips_degenerate_inputs() {
        round_trip(&[]);
        round_trip(&[42]);
        round_trip(&[7; 1000]);
    }

    /// A distribution skewed enough to push a naive tree past the length limit.
    #[test]
    fn round_trips_a_deep_distribution() {
        // Fibonacci-weighted frequencies are the classic worst case for Huffman depth.
        let mut data = Vec::new();
        let (mut a, mut b) = (1u32, 1u32);
        for s in 0..40u8 {
            data.extend(std::iter::repeat_n(s, a as usize));
            let next = a + b;
            a = b;
            b = next;
        }
        round_trip(&data);

        let lengths = code_lengths(&data);
        assert!(
            lengths.iter().all(|&l| usize::from(l) <= MAX_CODE_BITS),
            "code lengths must be bounded"
        );
    }

    #[test]
    fn skewed_data_actually_compresses() {
        // 90% zeros: entropy is well under one bit per symbol.
        let mut data = vec![0u8; 9000];
        data.extend(std::iter::repeat_n(1u8, 1000));
        let encoded = encode(&data);
        assert!(
            encoded.len() < data.len() / 2,
            "expected real compression, got {} from {}",
            encoded.len(),
            data.len()
        );
        round_trip(&data);
    }

    #[test]
    fn estimate_matches_the_real_thing() {
        for data in [
            b"aaaaaabbbbcccd".to_vec(),
            (0..1000).map(|i| (i % 17) as u8).collect(),
            vec![9u8; 500],
        ] {
            assert_eq!(estimated_len(&data), encode(&data).len());
        }
    }

    #[test]
    fn rejects_malformed_streams() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0; 10]).is_err());
        // Claims data but declares no codes.
        let mut bytes = vec![0u8; HEADER_BYTES];
        bytes[0] = 5;
        assert!(decode(&bytes).is_err());
        // A length beyond the limit.
        let mut bytes = encode(b"abc");
        bytes[4] = 31;
        assert!(decode(&bytes).is_err());
    }
}
