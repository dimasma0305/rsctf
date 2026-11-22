use super::PatchManifest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{Cursor, Read};

const REGISTRATION: &str = ".rsctf-ssh.json";
const MAX_BYTES: u64 = 16 * 1024;
const MAX_FILES: usize = 32;

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SshSource {
    pub source_path: String,
    pub container_path: String,
}

impl PatchManifest {
    /// Compare the saved ZIP with exactly the paths sampled in the team's shell.
    pub(crate) fn changes_since_ssh(
        &self,
        observed: &crate::services::byoc_tunnel::source::Files,
    ) -> Vec<super::PatchChange> {
        use super::{ChangeKind, PatchChange};
        observed
            .iter()
            .filter_map(|(path, file)| {
                let kind = match (file, self.0.get(path)) {
                    (Some(before), Some(saved))
                        if before.blob != hex::encode(saved.digest)
                            || before.executable != (saved.mode == 0o755) =>
                    {
                        ChangeKind::Modified
                    }
                    (Some(_), None) => ChangeKind::Deleted,
                    (None, Some(_)) => ChangeKind::Added,
                    _ => return None,
                };
                Some(PatchChange {
                    path: path.clone(),
                    kind,
                })
            })
            .collect()
    }

    /// Read only the organizer's validated starter. A team's candidate ZIP
    /// never changes which running files the organizer has registered.
    pub(crate) fn ssh_sources(&self, starter: &[u8]) -> Result<Vec<SshSource>, String> {
        if !self.0.contains_key(REGISTRATION) {
            return Ok(Vec::new());
        }
        let mut zip = zip::ZipArchive::new(Cursor::new(starter)).map_err(|e| e.to_string())?;
        let member = zip.by_name(REGISTRATION).map_err(|e| e.to_string())?;
        if member.size() > MAX_BYTES {
            return Err(".rsctf-ssh.json exceeds 16 KiB".into());
        }
        let mut bytes = Vec::new();
        member
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(".rsctf-ssh.json exceeds 16 KiB".into());
        }
        let mut sources: Vec<SshSource> =
            serde_json::from_slice(&bytes).map_err(|e| format!("Invalid .rsctf-ssh.json: {e}"))?;
        if sources.len() > MAX_FILES {
            return Err(".rsctf-ssh.json may register at most 32 files".into());
        }
        sources.sort_by(|a, b| a.source_path.cmp(&b.source_path));
        let mut targets = BTreeSet::new();
        let mut previous = None;
        for entry in &sources {
            let source = entry.source_path.as_str();
            let path = entry.container_path.as_str();
            if source == REGISTRATION
                || !self.0.contains_key(source)
                || source.len() > 240
                || source.chars().any(char::is_control)
            {
                return Err("Each SSH source must name a file in the starter ZIP".into());
            }
            let relative = path.strip_prefix('/').unwrap_or("");
            if relative.is_empty()
                || path.len() > 240
                || path.contains('\\')
                || path.chars().any(char::is_control)
                || relative.split('/').any(|part| {
                    matches!(part, "" | "." | "..") || part.eq_ignore_ascii_case(".git")
                })
                || matches!(relative.split('/').next(), Some("dev" | "proc" | "sys"))
            {
                return Err(
                    "Each SSH container path must be a normalized absolute application file path"
                        .into(),
                );
            }
            if previous == Some(source) || !targets.insert(path) {
                return Err("SSH source and container paths must each be unique".into());
            }
            previous = Some(source);
        }
        Ok(sources)
    }
}
