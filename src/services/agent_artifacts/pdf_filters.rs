//! Bounded decoders for the PDF stream filters that can carry text. Image
//! codecs (DCT, JPX, CCITT, JBIG2) are not decoded; such streams are skipped.

use std::io::Read;

/// Largest decoded size of one filter stage.
const MAX_STREAM_BYTES: usize = 8 * 1024 * 1024;
/// Decoded bytes, over every stage of every stream, allowed per document.
const MAX_TOTAL_DECODED: usize = 64 * 1024 * 1024;
/// A stage may not grow beyond this multiple of its input (plus a small
/// allowance), which defeats decompression bombs early.
const MAX_DECODE_RATIO: usize = 200;
const DECODE_ALLOWANCE: usize = 64 * 1024;
/// Real producers chain at most two or three filters.
const MAX_FILTERS: usize = 4;

pub(super) struct StreamDecoder {
    total: usize,
}

impl StreamDecoder {
    pub(super) fn new() -> Self {
        Self { total: 0 }
    }

    fn cap(&self, input: usize) -> usize {
        input
            .saturating_mul(MAX_DECODE_RATIO)
            .saturating_add(DECODE_ALLOWANCE)
            .min(MAX_STREAM_BYTES)
            .min(MAX_TOTAL_DECODED.saturating_sub(self.total))
    }

    /// Apply a stream's filters in order. `None` when a filter is not a text
    /// filter or the document's decode budget is spent.
    pub(super) fn decode(&mut self, raw: &[u8], filters: &[&[u8]]) -> Option<Vec<u8>> {
        if filters.len() > MAX_FILTERS {
            return None;
        }
        let mut data = raw.to_vec();
        for filter in filters {
            let cap = self.cap(data.len());
            if cap == 0 {
                return None;
            }
            data = match *filter {
                b"FlateDecode" | b"Fl" => inflate(&data, cap)?,
                b"ASCIIHexDecode" | b"AHx" => ascii_hex(&data, cap),
                b"ASCII85Decode" | b"A85" => ascii85(&data, cap),
                b"LZWDecode" | b"LZW" => lzw(&data, cap),
                b"RunLengthDecode" | b"RL" => run_length(&data, cap),
                _ => return None,
            };
            self.total += data.len();
        }
        Some(data)
    }
}

fn inflate(compressed: &[u8], cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = flate2::read::ZlibDecoder::new(compressed).take(cap as u64);
    if decoder.read_to_end(&mut out).is_err() && out.is_empty() {
        // Raw deflate without a zlib header also appears in the wild.
        let mut raw = flate2::read::DeflateDecoder::new(compressed).take(cap as u64);
        out.clear();
        raw.read_to_end(&mut out).ok()?;
    }
    Some(out)
}

fn ascii_hex(data: &[u8], cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;
    for &byte in data {
        if byte == b'>' || out.len() >= cap {
            break;
        }
        let nibble = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => continue,
        };
        match high.take() {
            Some(value) => out.push(value << 4 | nibble),
            None => high = Some(nibble),
        }
    }
    if let (Some(value), true) = (high, out.len() < cap) {
        out.push(value << 4);
    }
    out
}

fn ascii85(data: &[u8], cap: usize) -> Vec<u8> {
    let data = data.strip_prefix(b"<~").unwrap_or(data);
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut filled = 0;
    for &byte in data {
        if out.len() >= cap {
            break;
        }
        match byte {
            b'~' => break,
            b'z' if filled == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[filled] = byte - b'!';
                filled += 1;
                if filled == 5 {
                    let value = group
                        .iter()
                        .fold(0u64, |acc, digit| acc * 85 + u64::from(*digit));
                    out.extend_from_slice(&(value as u32).to_be_bytes());
                    filled = 0;
                }
            }
            _ => {}
        }
    }
    // A final partial group of n digits encodes n - 1 bytes.
    if filled > 1 && out.len() < cap {
        for digit in group.iter_mut().skip(filled) {
            *digit = 84;
        }
        let value = group
            .iter()
            .fold(0u64, |acc, digit| acc * 85 + u64::from(*digit));
        out.extend_from_slice(&(value as u32).to_be_bytes()[..filled - 1]);
    }
    out.truncate(cap);
    out
}

/// LZW as PDF uses it: MSB-first codes of 9-12 bits, clear 256, end 257, and
/// the default early change.
fn lzw(data: &[u8], cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = (0..=255u8).map(|byte| vec![byte]).collect();
    table.push(Vec::new());
    table.push(Vec::new());
    let mut width = 9;
    let mut previous: Option<usize> = None;
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in data {
        buffer = buffer << 8 | u32::from(byte);
        bits += 8;
        while bits >= width {
            bits -= width;
            let code = ((buffer >> bits) & ((1 << width) - 1)) as usize;
            buffer &= (1 << bits) - 1;
            match code {
                256 => {
                    table.truncate(258);
                    width = 9;
                    previous = None;
                    continue;
                }
                257 => return out,
                _ => {}
            }
            let entry = match (table.get(code), previous) {
                (Some(entry), _) => entry.clone(),
                (None, Some(last)) if code == table.len() => {
                    let mut entry = table[last].clone();
                    entry.push(table[last][0]);
                    entry
                }
                _ => return out,
            };
            if let Some(last) = previous {
                if table.len() < 4096 {
                    let mut added = table[last].clone();
                    added.push(entry[0]);
                    table.push(added);
                }
            }
            out.extend_from_slice(&entry);
            if out.len() >= cap {
                out.truncate(cap);
                return out;
            }
            previous = Some(code);
            width = match table.len() + 1 {
                length if length >= 2048 => 12,
                length if length >= 1024 => 11,
                length if length >= 512 => 10,
                _ => 9,
            };
        }
    }
    out
}

fn run_length(data: &[u8], cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < data.len() && out.len() < cap {
        let length = data[index];
        index += 1;
        match length {
            128 => break,
            0..=127 => {
                let end = (index + usize::from(length) + 1).min(data.len());
                out.extend_from_slice(&data[index..end]);
                index = end;
            }
            _ => {
                if let Some(&byte) = data.get(index) {
                    out.extend(std::iter::repeat_n(byte, 257 - usize::from(length)));
                }
                index += 1;
            }
        }
    }
    out.truncate(cap);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_filters_round_trip_known_encodings() {
        assert_eq!(ascii_hex(b"48 65 6C6c 6f7>", 64), b"Hellop");
        assert_eq!(ascii85(b"<~87cURD]i,\"Ebo7~>", 64), b"Hello World");
        assert_eq!(ascii85(b"z!!", 64), [0, 0, 0, 0, 0]);
        // "-----A---B" from the PDF reference's LZW example.
        assert_eq!(
            lzw(&[0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01], 64),
            b"-----A---B"
        );
        assert_eq!(
            run_length(&[2, b'a', b'b', b'c', 254, b'z', 128], 64),
            b"abczzz"
        );
    }

    #[test]
    fn chains_apply_in_order_and_unknown_filters_skip() {
        let mut compressed =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut compressed, b"BT (hidden) Tj ET").unwrap();
        let hex = hex::encode(compressed.finish().unwrap()).into_bytes();
        let mut decoder = StreamDecoder::new();
        assert_eq!(
            decoder
                .decode(&hex, &[b"AHx".as_slice(), b"FlateDecode".as_slice()])
                .unwrap(),
            b"BT (hidden) Tj ET"
        );
        assert!(decoder.decode(b"x", &[b"DCTDecode".as_slice()]).is_none());
        assert!(decoder.decode(b"x", &[b"AHx".as_slice(); 5]).is_none());
    }

    #[test]
    fn the_document_budget_counts_every_decoded_stream() {
        // Each run decodes 128 KiB into 8 MiB, the per-stream maximum.
        let run = [129u8, 0].repeat(64 * 1024);
        let mut decoder = StreamDecoder::new();
        for _ in 0..MAX_TOTAL_DECODED / MAX_STREAM_BYTES {
            let out = decoder.decode(&run, &[b"RL".as_slice()]).unwrap();
            assert_eq!(out.len(), MAX_STREAM_BYTES);
        }
        assert!(decoder.decode(&run, &[b"RL".as_slice()]).is_none());
        assert!(ascii85(&b"z".repeat(1024), 100).len() <= 100);
    }
}
