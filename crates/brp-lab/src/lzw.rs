//! Variable-width LZW over bytes, in the GIF dialect.
//!
//! Included because the brief asked for it, and because it is the dictionary half of what Deflate
//! does — comparing LZW alone against Deflate separates "dictionary matching helps" from
//! "dictionary matching plus Huffman helps".
//!
//! Codes start at 9 bits and grow to 12; when the dictionary fills, a clear code resets it. Stream
//! layout is a u32 length followed by the codes, MSB first.

use anyhow::{bail, Result};
use brp_core::{BitReader, BitWriter};

const CLEAR: u16 = 256;
const END: u16 = 257;
const FIRST_FREE: u16 = 258;
const MIN_WIDTH: u32 = 9;
const MAX_WIDTH: u32 = 12;
const MAX_CODES: usize = 1 << MAX_WIDTH;

pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 8);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());

    let mut w = BitWriter::with_capacity(data.len() / 2);
    if data.is_empty() {
        w.write(u32::from(END), MIN_WIDTH);
        out.extend_from_slice(&w.finish());
        return out;
    }

    // Dictionary as a map from (prefix code, next byte) to code. A flat vector keyed by
    // `prefix * 256 + byte` would be 4M entries; a HashMap keeps this honest on memory.
    let mut dict: std::collections::HashMap<(u16, u8), u16> = std::collections::HashMap::new();
    let mut next_code = FIRST_FREE;
    let mut width = MIN_WIDTH;

    w.write(u32::from(CLEAR), width);

    let mut current = u16::from(data[0]);
    for &b in &data[1..] {
        match dict.get(&(current, b)) {
            Some(&code) => current = code,
            None => {
                w.write(u32::from(current), width);
                if (next_code as usize) < MAX_CODES {
                    dict.insert((current, b), next_code);
                    next_code += 1;
                    // Widen once the next code needs another bit.
                    if next_code as usize > (1 << width) && width < MAX_WIDTH {
                        width += 1;
                    }
                } else {
                    w.write(u32::from(CLEAR), width);
                    dict.clear();
                    next_code = FIRST_FREE;
                    width = MIN_WIDTH;
                }
                current = u16::from(b);
            }
        }
    }
    w.write(u32::from(current), width);
    w.write(u32::from(END), width);

    out.extend_from_slice(&w.finish());
    out
}

pub fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 4 {
        bail!("lzw stream is shorter than its header");
    }
    let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let mut r = BitReader::new(&bytes[4..]);
    let mut out = Vec::with_capacity(count);

    // Entries 0..256 are single bytes; the rest are (prefix, suffix) pairs expanded on demand.
    let mut prefix = vec![u16::MAX; MAX_CODES];
    let mut suffix = vec![0u8; MAX_CODES];
    let mut next_code = FIRST_FREE;
    let mut width = MIN_WIDTH;
    let mut previous: Option<u16> = None;
    let mut buffer = Vec::with_capacity(64);

    loop {
        let code = r.read(width).map_err(|e| anyhow::anyhow!(e))? as u16;
        if code == END {
            break;
        }
        if code == CLEAR {
            next_code = FIRST_FREE;
            width = MIN_WIDTH;
            previous = None;
            continue;
        }

        // Expand the code, handling the KwKwK case where a code refers to itself.
        buffer.clear();
        let mut walk = if code < next_code {
            code
        } else if code == next_code {
            let Some(prev) = previous else {
                bail!("lzw stream starts with an undefined code");
            };
            // The new entry is prev's expansion followed by its own first byte; emit that first
            // byte last by seeding the buffer with it.
            let mut first = prev;
            while first >= 256 {
                first = prefix[usize::from(first)];
                if first == u16::MAX {
                    bail!("lzw dictionary entry is undefined");
                }
            }
            buffer.push(first as u8);
            prev
        } else {
            bail!("lzw code {code} is beyond the dictionary");
        };

        let mut guard = 0;
        while walk >= 256 {
            let p = prefix[usize::from(walk)];
            if p == u16::MAX {
                bail!("lzw dictionary entry is undefined");
            }
            buffer.push(suffix[usize::from(walk)]);
            walk = p;
            guard += 1;
            if guard > MAX_CODES {
                bail!("lzw dictionary contains a cycle");
            }
        }
        buffer.push(walk as u8);
        buffer.reverse();
        out.extend_from_slice(&buffer);

        if let Some(prev) = previous {
            if (next_code as usize) < MAX_CODES {
                prefix[usize::from(next_code)] = prev;
                suffix[usize::from(next_code)] = buffer[0];
                next_code += 1;
                if next_code as usize >= (1 << width) && width < MAX_WIDTH {
                    width += 1;
                }
            }
        }
        previous = Some(code);

        if out.len() > count {
            bail!("lzw stream decodes to more than its declared length");
        }
    }

    if out.len() != count {
        bail!(
            "lzw stream declared {count} bytes but produced {}",
            out.len()
        );
    }
    Ok(out)
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
        round_trip(b"TOBEORNOTTOBEORTOBEORNOT");
        round_trip(&(0..=255u8).collect::<Vec<_>>());
        round_trip(&(0..10_000).map(|i| (i * 7 % 251) as u8).collect::<Vec<_>>());
    }

    #[test]
    fn round_trips_degenerate_inputs() {
        round_trip(&[]);
        round_trip(&[42]);
        round_trip(&[7; 5000]);
    }

    /// Long runs drive the KwKwK case, where a code is used in the same step it is defined.
    #[test]
    fn round_trips_repeating_runs() {
        round_trip(&[0xAB; 20_000]);
        let pattern: Vec<u8> = b"abcabcabc".iter().cycle().take(30_000).copied().collect();
        round_trip(&pattern);
    }

    /// Enough distinct content to fill the dictionary and force a reset.
    #[test]
    fn round_trips_across_a_dictionary_reset() {
        let data: Vec<u8> = (0..80_000u32)
            .map(|i| {
                let mut v = i.wrapping_mul(2_654_435_761);
                v ^= v >> 15;
                (v % 251) as u8
            })
            .collect();
        round_trip(&data);
    }

    #[test]
    fn repetitive_data_compresses() {
        let data = vec![0u8; 50_000];
        assert!(encode(&data).len() < data.len() / 10);
    }

    #[test]
    fn rejects_malformed_streams() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0, 0, 0, 0]).is_err());
        // A length that does not match the payload.
        let mut bytes = encode(b"hello world");
        bytes[0] = 200;
        assert!(decode(&bytes).is_err());
    }
}
