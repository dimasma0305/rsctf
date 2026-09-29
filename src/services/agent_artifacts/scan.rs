//! Pure, bounded scanning of one submitted file. Nothing here touches the
//! database or runs the file; containers are opened only to read their bytes.

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

struct Segment {
    location: ArtifactLocation,
    /// Archive member name, shown in front of the snippet.
    member: Option<String>,
    bytes: Vec<u8>,
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

fn zip_members(bytes: &[u8]) -> Vec<Segment> {
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
            bytes: named,
        });
    }
    segments
}

fn gzip_body(bytes: &[u8]) -> Option<Segment> {
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
        bytes: data,
    })
}

fn segments(bytes: &[u8]) -> Vec<Segment> {
    let mut segments = vec![Segment {
        location: ArtifactLocation::Raw,
        member: None,
        bytes: bytes.to_vec(),
    }];
    if pdf::is_pdf(bytes) {
        let content = pdf::extract(bytes);
        segments.push(Segment {
            location: ArtifactLocation::Text,
            member: None,
            bytes: content.text,
        });
        segments.extend(content.streams.into_iter().map(|stream| Segment {
            location: ArtifactLocation::Stream,
            member: None,
            bytes: stream,
        }));
    } else if bytes.starts_with(b"PK\x03\x04") {
        segments.extend(zip_members(bytes));
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        segments.extend(gzip_body(bytes));
    }
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
