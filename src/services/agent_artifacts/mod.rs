//! Agent-artifact signatures: traces that AI agent tooling leaves in files a
//! team submits (a coding agent's session scratchpad path, its commit trailers,
//! its local project store). Uploaded solvers and writeups are scanned as data
//! and never executed. A match is review evidence for organizers, not proof on
//! its own: an event may allow AI, and a trace only shows which tool touched
//! the file.

mod pdf;
mod pdf_filters;
mod record;
mod scan;

pub use record::*;
pub use scan::{scan_file, ArtifactHit, ArtifactLocation};

use regex::bytes::{Regex, RegexBuilder};
use sqlx::PgPool;

use crate::utils::error::{AppError, AppResult};

pub const MAX_CUSTOM_SIGNATURES: i64 = 32;
pub const MAX_PATTERN_LENGTH: usize = 512;
pub const MAX_LABEL_LENGTH: usize = 64;
const REGEX_SIZE_LIMIT: usize = 256 * 1024;

/// Ordinary solver and writeup text. A custom signature that matches any of it
/// would flag innocent teams, so it is rejected.
const BENIGN_SAMPLES: &[&str] = &[
    "",
    "#!/usr/bin/env python3\nimport os, sys\nfrom pwn import *\n",
    "p = remote('127.0.0.1', 1337)\np.sendline(b'hello world')\nprint(p.recvall())\n",
    "/tmp/solve.py /home/ubuntu/ctf/ /root/solver/ C:\\Users\\player\\Desktop\\ ~/Downloads/\n",
    "# Writeup\n\nThe flag is FLAG{example}. We used Ghidra and gdb.\n",
    "curl http://localhost:8080/api/flag -H 'Content-Type: application/json'\n",
];

/// A signature shipped with the platform. Every pattern was checked against
/// traces produced by the named tool on a real machine; path patterns also
/// accept Windows and JSON-escaped separators for the same layout.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSignature {
    pub key: &'static str,
    pub label: &'static str,
    pub pattern: &'static str,
    pub examples: &'static [&'static str],
}

pub const BUILTIN_SIGNATURES: &[BuiltinSignature] = &[
    BuiltinSignature {
        key: "claude-code-scratchpad",
        label: "Claude Code scratchpad path",
        pattern: r"/tmp/claude-[0-9]+/(?-u:[^/\s]){1,200}/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/scratchpad",
        examples: &[
            "/tmp/claude-0/-root-homelab-rsctf/1459d0ed-f350-410d-ab89-9500453ffedc/scratchpad/grind_key.py",
        ],
    },
    BuiltinSignature {
        key: "claude-code-trailer",
        label: "Claude Code commit trailer",
        pattern: r"Co-Authored-By: Claude [^<\r\n]{1,40}<noreply@anthropic\.com>|Claude-Session: https://claude\.ai/code/session_[A-Za-z0-9]{8,}|Generated with \[Claude Code\]\(https://claude\.com/claude-code\)",
        examples: &[
            "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
            "Claude-Session: https://claude.ai/code/session_01Pk7Sh3xaxjT6C44iEQhKHC",
            "Generated with [Claude Code](https://claude.com/claude-code)",
        ],
    },
    BuiltinSignature {
        key: "claude-code-projects",
        label: "Claude Code project store",
        // Project folders are the working path with every separator replaced
        // by '-': `/root/ctf` is `-root-ctf`, `C:\ctf` is `C--ctf`.
        pattern: r"\.claude[/\\]{1,2}projects[/\\]{1,2}(?:-|[A-Za-z]--)[A-Za-z0-9._-]{2,200}",
        examples: &[
            "/root/.claude/projects/-root-ctf-mevbot/0f3c2a1e.jsonl",
            r"C:\Users\player\.claude\projects\C--Users-player-ctf\0f3c2a1e.jsonl",
            r#"{"path":"C:\\Users\\player\\.claude\\projects\\C--Users-player-ctf"}"#,
        ],
    },
    BuiltinSignature {
        key: "codex-home",
        label: "Codex agent home directory",
        pattern: r"\.codex[/\\]{1,2}(?:sessions|shell_snapshots|worktrees)[/\\]",
        examples: &[
            "/root/.codex/shell_snapshots/01a0a836-3789-7c21-935c-498fe0e56e9d.1790661776803146567.sh",
            "/home/player/.codex/sessions/2026/09/28/",
            r"C:\Users\player\.codex\sessions\2026\09\28\",
        ],
    },
    BuiltinSignature {
        key: "cursor-projects",
        label: "Cursor agent project store",
        pattern: r"\.cursor[/\\]{1,2}projects[/\\]{1,2}[A-Za-z0-9._-]{2,200}",
        examples: &[
            "/root/.cursor/projects/root-writeup-ua-Task-6/terminals/1.txt",
            r"C:\Users\player\.cursor\projects\c-Users-player-ctf\terminals\1.txt",
        ],
    },
];

pub fn builtin(key: &str) -> Option<&'static BuiltinSignature> {
    BUILTIN_SIGNATURES
        .iter()
        .find(|signature| signature.key == key)
}

fn compile(pattern: &str) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|error| error.to_string())
}

pub fn validate_custom_key(key: &str) -> AppResult<()> {
    let valid = (1..=40).contains(&key.len())
        && key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid {
        return Err(AppError::bad_request(
            "Signature keys use 1-40 lowercase letters, digits, or hyphens",
        ));
    }
    Ok(())
}

pub fn validate_custom_label(label: &str) -> AppResult<String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_LENGTH {
        return Err(AppError::bad_request(format!(
            "Signature labels need 1-{MAX_LABEL_LENGTH} characters"
        )));
    }
    if label.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "Signature labels cannot contain control characters",
        ));
    }
    Ok(label.to_string())
}

/// A custom pattern must compile within the size limits and stay specific:
/// it may not match an empty string or ordinary solver/writeup text.
pub fn validate_custom_pattern(pattern: &str) -> AppResult<String> {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.len() > MAX_PATTERN_LENGTH {
        return Err(AppError::bad_request(format!(
            "Signature patterns need 1-{MAX_PATTERN_LENGTH} characters"
        )));
    }
    let regex = compile(pattern)
        .map_err(|error| AppError::bad_request(format!("Invalid signature pattern: {error}")))?;
    if BENIGN_SAMPLES
        .iter()
        .any(|sample| regex.is_match(sample.as_bytes()))
    {
        return Err(AppError::bad_request(
            "This pattern also matches ordinary solver or writeup text; make it more specific",
        ));
    }
    Ok(pattern.to_string())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SignatureRow {
    pub signature_key: String,
    pub builtin: bool,
    pub label: Option<String>,
    pub pattern: Option<String>,
    pub enabled: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub async fn load_rows(pool: &PgPool) -> AppResult<Vec<SignatureRow>> {
    sqlx::query_as::<_, SignatureRow>(
        r#"SELECT signature_key, builtin, label, pattern, enabled, updated_at
             FROM "AgentArtifactSignatures"
            ORDER BY builtin DESC, signature_key"#,
    )
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

#[derive(Debug, Clone)]
pub struct CompiledSignature {
    pub key: String,
    pub label: String,
    pub regex: Regex,
}

/// Enabled built-ins (unless switched off) followed by enabled custom
/// signatures. A stored custom pattern that no longer compiles is skipped.
pub fn enabled_signatures(rows: &[SignatureRow]) -> Vec<CompiledSignature> {
    let mut signatures = BUILTIN_SIGNATURES
        .iter()
        .filter(|signature| {
            rows.iter()
                .find(|row| row.builtin && row.signature_key == signature.key)
                .is_none_or(|row| row.enabled)
        })
        .filter_map(|signature| {
            compile(signature.pattern)
                .ok()
                .map(|regex| CompiledSignature {
                    key: signature.key.to_string(),
                    label: signature.label.to_string(),
                    regex,
                })
        })
        .collect::<Vec<_>>();
    signatures.extend(
        rows.iter()
            .filter(|row| !row.builtin && row.enabled)
            .filter_map(|row| {
                let regex = compile(row.pattern.as_deref()?).ok()?;
                Some(CompiledSignature {
                    key: row.signature_key.clone(),
                    label: row.label.clone()?,
                    regex,
                })
            }),
    );
    signatures
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
