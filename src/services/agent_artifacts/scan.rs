//! Pure, bounded scanning of one submitted file. Nothing here touches the
//! database or runs the file; containers are opened only to read their bytes.

use std::borrow::Cow;
use std::io::Read;

use super::{pdf, CompiledSignature};

/// Decoded size of one archive member or of a gzip body.
const MAX_MEMBER_BYTES: usize = 8 * 1024 * 1024;
/// Decoded size of all members of one archive.
const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ARCHIVE_MEMBERS: usize = 256;
const MAX_MEMBER_RATIO: u64 = 200;
const SNIPPET_CONTEXT: usize = 80;
const MAX_SNIPPET_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactLocation {
    /// The file's own bytes.
    Raw,
    /// A decompressed PDF stream, archive member, or gzip body.
    Stream,
    /// Text recovered from PDF pages.
    Text,
}

impl ArtifactLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "Raw",
            Self::Stream => "Stream",
            Self::Text => "Text",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactHit {
    pub signature_key: String,
    pub signature_label: String,
    pub location: ArtifactLocation,
    pub byte_offset: u64,
    pub snippet: String,
}

struct Segment<'a> {
    location: ArtifactLocation,
    /// Archive member name, shown in front of the snippet.
    member: Option<String>,
    bytes: Cow<'a, [u8]>,
}

fn snippet(bytes: &[u8], start: usize, end: usize, member: Option<&str>) -> String {
    let from = start.saturating_sub(SNIPPET_CONTEXT);
    let to = end.saturating_add(SNIPPET_CONTEXT).min(bytes.len());
    let text = String::from_utf8_lossy(&bytes[from..to])
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = match member {
        Some(name) => format!("{name}: {text}"),
        None => text,
    };
    let mut clipped = text.chars().take(MAX_SNIPPET_CHARS).collect::<String>();
    if clipped.is_empty() {
        clipped.push('?');
    }
    clipped
}

fn zip_members(bytes: &[u8]) -> Vec<Segment<'static>> {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    let mut segments = Vec::new();
    let mut total = 0usize;
    for index in 0..archive.len().min(MAX_ARCHIVE_MEMBERS) {
        let Ok(member) = archive.by_index(index) else {
            continue;
        };
        if member.is_dir() {
            continue;
        }
        let name = member.name().chars().take(120).collect::<String>();
        let cap = (member.compressed_size().saturating_mul(MAX_MEMBER_RATIO) + 64 * 1024)
            .min(MAX_MEMBER_BYTES as u64)
            .min(MAX_ARCHIVE_BYTES.saturating_sub(total) as u64);
        if cap == 0 {
            break;
        }
        let mut data = Vec::new();
        if member.take(cap).read_to_end(&mut data).is_err() && data.is_empty() {
            continue;
        }
        total += data.len();
        let mut named = name.clone().into_bytes();
        named.push(b'\n');
        named.extend_from_slice(&data);
        segments.push(Segment {
            location: ArtifactLocation::Stream,
            member: Some(name),
            bytes: Cow::Owned(named),
        });
    }
    segments
}

fn gzip_body(bytes: &[u8]) -> Option<Segment<'static>> {
    let cap = (bytes.len() as u64)
        .saturating_mul(MAX_MEMBER_RATIO)
        .min(MAX_MEMBER_BYTES as u64);
    let mut data = Vec::new();
    let _ = flate2::read::MultiGzDecoder::new(bytes)
        .take(cap)
        .read_to_end(&mut data);
    (!data.is_empty()).then_some(Segment {
        location: ArtifactLocation::Stream,
        member: None,
        bytes: Cow::Owned(data),
    })
}

/// A zip archive's end-of-central-directory record sits in the last 64 KiB,
/// so this also finds archives with bytes prepended to them.
fn has_zip_directory(bytes: &[u8]) -> bool {
    let tail = &bytes[bytes.len().saturating_sub(64 * 1024 + 22)..];
    tail.windows(4).any(|window| window == b"PK\x05\x06")
}

/// UTF-16 text with a byte-order mark, as Windows tools often write it.
fn utf16_text(bytes: &[u8]) -> Option<Vec<u8>> {
    let from_pair: fn([u8; 2]) -> u16 = match bytes.get(..2)? {
        [0xff, 0xfe] => u16::from_le_bytes,
        [0xfe, 0xff] => u16::from_be_bytes,
        _ => return None,
    };
    let units = bytes[2..]
        .chunks_exact(2)
        .map(|pair| from_pair([pair[0], pair[1]]));
    Some(
        char::decode_utf16(units)
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
            .into_bytes(),
    )
}

/// Page text with line breaks and the spacing around them removed, so a long
/// path that wraps onto the next line is matched whole.
fn join_lines(text: &[u8]) -> Option<Vec<u8>> {
    if !text.contains(&b'\n') {
        return None;
    }
    let mut joined = Vec::with_capacity(text.len());
    let mut at_break = false;
    for &byte in text {
        match byte {
            b'\n' | b'\r' => {
                while joined
                    .last()
                    .is_some_and(|last| matches!(last, b' ' | b'\t'))
                {
                    joined.pop();
                }
                at_break = true;
            }
            b' ' | b'\t' if at_break => {}
            _ => {
                at_break = false;
                joined.push(byte);
            }
        }
    }
    Some(joined)
}

fn segments(bytes: &[u8]) -> Vec<Segment<'_>> {
    let mut segments = vec![Segment {
        location: ArtifactLocation::Raw,
        member: None,
        bytes: Cow::Borrowed(bytes),
    }];
    if pdf::is_pdf(bytes) {
        let content = pdf::extract(bytes);
        let joined = join_lines(&content.text);
        segments.push(Segment {
            location: ArtifactLocation::Text,
            member: None,
            bytes: Cow::Owned(content.text),
        });
        segments.extend(joined.map(|joined| Segment {
            location: ArtifactLocation::Text,
            member: Some("line breaks removed".to_string()),
            bytes: Cow::Owned(joined),
        }));
        segments.extend(content.streams.into_iter().map(|stream| Segment {
            location: ArtifactLocation::Stream,
            member: None,
            bytes: Cow::Owned(stream),
        }));
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        segments.extend(gzip_body(bytes));
    }
    // Checked apart from the PDF case: a PDF can also be a valid zip.
    if has_zip_directory(bytes) {
        segments.extend(zip_members(bytes));
    }
    let decoded = segments
        .iter()
        .filter_map(|segment| {
            // Archive members carry their name and a newline in front.
            let body = match &segment.member {
                Some(name) if segment.location == ArtifactLocation::Stream => {
                    segment.bytes.get(name.len() + 1..)?
                }
                _ => &segment.bytes,
            };
            utf16_text(body).map(|text| Segment {
                location: segment.location,
                member: segment.member.clone(),
                bytes: Cow::Owned(text),
            })
        })
        .collect::<Vec<_>>();
    segments.extend(decoded);
    segments
}

/// First match of every enabled signature in a submitted file, looking at the
/// raw bytes, recovered PDF text, and decompressed PDF streams or archive
/// members. CPU-bound: call from a blocking context for large files.
pub fn scan_file(signatures: &[CompiledSignature], bytes: &[u8]) -> Vec<ArtifactHit> {
    if signatures.is_empty() || bytes.is_empty() {
        return Vec::new();
    }
    let segments = segments(bytes);
    let mut hits: Vec<ArtifactHit> = Vec::new();
    for signature in signatures {
        for segment in &segments {
            if let Some(found) = signature.regex.find(&segment.bytes) {
                hits.push(ArtifactHit {
                    signature_key: signature.key.clone(),
                    signature_label: signature.label.clone(),
                    location: segment.location,
                    byte_offset: found.start() as u64,
                    snippet: snippet(
                        &segment.bytes,
                        found.start(),
                        found.end(),
                        segment.member.as_deref(),
                    ),
                });
                break;
            }
        }
    }
    hits
}
