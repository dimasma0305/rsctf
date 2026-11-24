//! Public diffs contain only organizer-selected paths and line coordinates.
use super::{ssh_sources::SshSource, ChangeKind, PatchChange, PatchManifest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

const MAX_BYTES: u64 = 16 * 1024;
const MAX_LINES: usize = 256;
const MAX_HUNKS: usize = 16;

#[derive(Serialize, Deserialize)]
pub(crate) struct PublishedChange {
    #[serde(flatten)]
    change: PatchChange,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hunks: Vec<LineHunk>,
}

/// Zero-context unified diff ranges. An empty span starts after the indicated line.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineHunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
}

fn text_member(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    path: &str,
) -> anyhow::Result<Option<String>> {
    let entry = zip.by_name(path)?;
    if entry.size() > MAX_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok().filter(|text| {
        !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
    }))
}

fn line_hunks(old: &str, new: &str) -> Vec<LineHunk> {
    let mut ids = BTreeMap::new();
    let mut encode = |line| {
        let next_id = ids.len();
        *ids.entry(line).or_insert(next_id)
    };
    let before: Vec<_> = old
        .split_inclusive('\n')
        .take(MAX_LINES + 1)
        .map(&mut encode)
        .collect();
    let after: Vec<_> = new
        .split_inclusive('\n')
        .take(MAX_LINES + 1)
        .map(encode)
        .collect();
    let (n, m) = (before.len(), after.len());
    if n > MAX_LINES || m > MAX_LINES {
        return Vec::new();
    }
    // LCS on interned lines bounds the cost regardless of the length of a repeated line.
    let width = m + 1;
    let mut lengths = vec![0u16; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let k = i * width + j;
            lengths[k] = if before[i] == after[j] {
                1 + lengths[k + width + 1]
            } else {
                lengths[k + width].max(lengths[k + 1])
            };
        }
    }
    let mut hunks = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && before[i] == after[j] {
            i += 1;
            j += 1;
            continue;
        }
        let (start_i, start_j) = (i, j);
        while (i < n || j < m) && !(i < n && j < m && before[i] == after[j]) {
            let k = i * width + j;
            if i < n && (j == m || lengths[k + width] >= lengths[k + 1]) {
                i += 1;
            } else {
                j += 1;
            }
        }
        if hunks.len() == MAX_HUNKS {
            return Vec::new();
        }
        hunks.push(LineHunk {
            old_start: start_i + usize::from(i > start_i),
            old_count: i - start_i,
            new_start: start_j + usize::from(j > start_j),
            new_count: j - start_j,
        });
    }
    hunks
}

pub(super) fn changes(
    sources: &[SshSource],
    starter: &PatchManifest,
    candidate: &PatchManifest,
    original_zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    candidate_zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
) -> anyhow::Result<Vec<PublishedChange>> {
    let mut changes = Vec::new();
    for source in sources
        .iter()
        .filter(|s| s.share_after_close || s.share_line_ranges_after_close)
    {
        let path = &source.source_path;
        let Some(original) = starter.0.get(path) else {
            continue;
        };
        let current = candidate.0.get(path);
        if current == Some(original) {
            continue;
        }
        let mut hunks = Vec::new();
        if source.share_line_ranges_after_close {
            if let Some(before) = text_member(original_zip, path)? {
                let after = if current.is_some() {
                    text_member(candidate_zip, path)?
                } else {
                    Some(String::new())
                };
                if let Some(after) = after {
                    hunks = line_hunks(&before, &after);
                }
            }
        }
        changes.push(PublishedChange {
            change: PatchChange {
                path: path.clone(),
                kind: if current.is_some() {
                    ChangeKind::Modified
                } else {
                    ChangeKind::Deleted
                },
            },
            hunks,
        });
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_separate_edits_and_empty_span_coordinates() {
        let hunks = line_hunks("middle\nold\nend\n", "added\nmiddle\nend\n");
        let ranges: Vec<_> = hunks
            .iter()
            .map(|h| (h.old_start, h.old_count, h.new_start, h.new_count))
            .collect();
        assert_eq!(ranges, [(0, 0, 1, 1), (2, 1, 2, 0)]);
        assert!(line_hunks("equal\n", "equal\n").is_empty());
        let eof = line_hunks("equal\n", "equal");
        assert_eq!((eof[0].old_count, eof[0].new_count), (1, 1));
    }

    #[test]
    fn legacy_changes_round_trip_and_diff_size_is_bounded() {
        let legacy = serde_json::json!({"path":"app.rs","kind":"Deleted"});
        let change: PublishedChange = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(change).unwrap(), legacy);
        assert!(line_hunks(&"line\n".repeat(MAX_LINES + 1), "").is_empty());
        let before = "keep\nremove\n".repeat(MAX_HUNKS + 1);
        let after = "keep\n".repeat(MAX_HUNKS + 1);
        assert!(line_hunks(&before, &after).is_empty());
    }
}
