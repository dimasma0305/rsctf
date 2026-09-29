use std::io::Write;

use super::*;

fn builtins() -> Vec<CompiledSignature> {
    enabled_signatures(&[])
}

fn keys(hits: &[ArtifactHit]) -> Vec<&str> {
    hits.iter().map(|hit| hit.signature_key.as_str()).collect()
}

#[test]
fn every_builtin_matches_its_examples_and_no_benign_text() {
    let signatures = builtins();
    assert_eq!(signatures.len(), BUILTIN_SIGNATURES.len());
    for builtin in BUILTIN_SIGNATURES {
        let compiled = signatures
            .iter()
            .find(|signature| signature.key == builtin.key)
            .unwrap();
        assert!(!builtin.examples.is_empty(), "{}", builtin.key);
        for example in builtin.examples {
            assert!(compiled.regex.is_match(example.as_bytes()), "{example}");
        }
        for sample in BENIGN_SAMPLES {
            assert!(
                !compiled.regex.is_match(sample.as_bytes()),
                "{} matched {sample:?}",
                builtin.key
            );
        }
    }
}

#[test]
fn builtins_reject_near_misses() {
    let signatures = builtins();
    let hit = |text: &str| keys(&scan_file(&signatures, text.as_bytes())).len();
    for near_miss in [
        "/tmp/claude/scratchpad/solve.py",
        "/tmp/claude-0/project/not-a-session/scratchpad",
        "/tmp/claude-x/p/0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0/scratchpad",
        "Co-Authored-By: Alice <alice@example.com>",
        "claude.ai/share/abc",
        "/root/.claude/settings.json",
        "/root/.codex/config.toml",
        "/root/.cursor/extensions/",
    ] {
        assert_eq!(hit(near_miss), 0, "{near_miss}");
    }
}

#[test]
fn switched_off_builtins_and_custom_signatures_are_honored() {
    let now = chrono::Utc::now();
    let rows = vec![
        SignatureRow {
            signature_key: "claude-code-scratchpad".into(),
            builtin: true,
            label: None,
            pattern: None,
            enabled: false,
            updated_at: now,
        },
        SignatureRow {
            signature_key: "my-agent".into(),
            builtin: false,
            label: Some("My agent".into()),
            pattern: Some(r"/opt/my-agent/runs/[0-9]+".into()),
            enabled: true,
            updated_at: now,
        },
        SignatureRow {
            signature_key: "disabled-custom".into(),
            builtin: false,
            label: Some("Disabled".into()),
            pattern: Some(r"never-used-[0-9]+".into()),
            enabled: false,
            updated_at: now,
        },
    ];
    let signatures = enabled_signatures(&rows);
    assert!(!signatures.iter().any(|s| s.key == "claude-code-scratchpad"));
    assert!(signatures.iter().any(|s| s.key == "my-agent"));
    assert!(!signatures.iter().any(|s| s.key == "disabled-custom"));
    let hits = scan_file(&signatures, b"log: /opt/my-agent/runs/42/out.txt");
    assert_eq!(keys(&hits), ["my-agent"]);
}

#[test]
fn custom_patterns_must_compile_and_stay_specific() {
    assert!(validate_custom_pattern(r"/opt/my-agent/runs/[0-9]+").is_ok());
    for bad in [
        "",
        "(",
        ".*",
        r"\w+",
        "/tmp/",
        "import",
        r"[a-z]*",
        &"a".repeat(600),
    ] {
        assert!(validate_custom_pattern(bad).is_err(), "{bad:?}");
    }
    assert!(validate_custom_key("my-agent-2").is_ok());
    for bad in ["", "-x", "Upper", "under_score", &"a".repeat(41)] {
        assert!(validate_custom_key(bad).is_err(), "{bad:?}");
    }
    assert_eq!(validate_custom_label("  Agent  ").unwrap(), "Agent");
    assert!(validate_custom_label("bad\nlabel").is_err());
}

#[test]
fn raw_solver_bytes_report_the_first_hit_with_a_bounded_clean_snippet() {
    let solver = b"import os\n# key saved at /tmp/claude-0/-home-p-ctf/0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0/scratchpad/grind_key.txt\r\n\x07done\n";
    let hits = scan_file(&builtins(), solver);
    assert_eq!(keys(&hits), ["claude-code-scratchpad"]);
    let hit = &hits[0];
    assert_eq!(hit.location, ArtifactLocation::Raw);
    assert_eq!(hit.byte_offset, 25);
    assert!(hit.snippet.contains("/scratchpad"));
    assert!(!hit.snippet.chars().any(char::is_control));
    assert!(hit.snippet.chars().count() <= 240);
}

#[test]
fn chrome_printed_pdf_text_is_recovered_from_glyph_ids() {
    let artifact = include_bytes!("fixtures/chrome-writeup-artifact.pdf");
    let clean = include_bytes!("fixtures/chrome-writeup-clean.pdf");
    // The path only exists as compressed glyph IDs, not in the raw bytes.
    assert!(!artifact.windows(10).any(|window| window == b"scratchpad"));
    let hits = scan_file(&builtins(), artifact);
    assert_eq!(keys(&hits), ["claude-code-scratchpad"]);
    assert_eq!(hits[0].location, ArtifactLocation::Text);
    assert!(
        hits[0].snippet.contains("grind_key.txt"),
        "{}",
        hits[0].snippet
    );
    assert!(scan_file(&builtins(), clean).is_empty());
    let text = String::from_utf8(pdf::extract(clean).text).unwrap();
    assert!(text.contains("/tmp/grind_key.txt"), "{text}");
    assert!(text.contains("MEVBot writeup"), "{text}");
}

#[test]
fn zipped_and_gzipped_solvers_are_opened_as_data() {
    let path = b"see /root/.codex/shell_snapshots/01a0.sh\n";
    let mut zip_bytes = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut zip_bytes);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("solve/notes.txt", options).unwrap();
        writer.write_all(path).unwrap();
        writer.finish().unwrap();
    }
    let hits = scan_file(&builtins(), zip_bytes.get_ref());
    assert_eq!(keys(&hits), ["codex-home"]);
    assert_eq!(hits[0].location, ArtifactLocation::Stream);
    assert!(
        hits[0].snippet.starts_with("solve/notes.txt: "),
        "{}",
        hits[0].snippet
    );

    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(path).unwrap();
    let hits = scan_file(&builtins(), &gzip.finish().unwrap());
    assert_eq!(keys(&hits), ["codex-home"]);
}

#[test]
fn decompression_bombs_stay_within_the_caps() {
    // 256 MiB of zeros compresses to about 250 KiB.
    let zeros = vec![0u8; 1024 * 1024];
    let mut deflated = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    for _ in 0..256 {
        deflated.write_all(&zeros).unwrap();
    }
    let stream = deflated.finish().unwrap();
    let mut bomb = b"%PDF-1.4\n1 0 obj\n<< /Filter /FlateDecode >>\nstream\n".to_vec();
    bomb.extend_from_slice(&stream);
    bomb.extend_from_slice(b"\nendstream\nendobj\n");
    let started = std::time::Instant::now();
    let content = pdf::extract(&bomb);
    let inflated: usize = content.streams.iter().map(Vec::len).sum();
    assert!(inflated <= 8 * 1024 * 1024, "{inflated}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));

    let mut zip_bytes = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut zip_bytes);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for index in 0..4 {
            writer
                .start_file(format!("bomb-{index}.bin"), options)
                .unwrap();
            for _ in 0..64 {
                writer.write_all(&zeros).unwrap();
            }
        }
        writer.finish().unwrap();
    }
    let started = std::time::Instant::now();
    assert!(scan_file(&builtins(), zip_bytes.get_ref()).is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn malformed_pdfs_do_not_panic() {
    for input in [
        b"%PDF-1.7\n1 0 obj << /Type /Page /Contents 9 0 R".to_vec(),
        b"%PDF-1.7\n1 0 obj\n<< /Filter /FlateDecode >>\nstream\nnot zlib\nendstream\nendobj"
            .to_vec(),
        b"%PDF-1.7\n2 0 obj << /Type /ObjStm /N 99999 /First 999999 >> stream\n\nendstream endobj"
            .to_vec(),
        b"%PDF-1.7\n3 0 obj (((((( endobj 4 0 obj <<<<<<<< [[[[ endobj".to_vec(),
        b"%PDF-".to_vec(),
    ] {
        let _ = scan_file(&builtins(), &input);
    }
}
