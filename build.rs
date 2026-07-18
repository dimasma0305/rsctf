use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut files = vec![root.join("Cargo.toml"), root.join("build.rs")];
    let lockfile = root.join("Cargo.lock");
    if lockfile.is_file() {
        files.push(lockfile);
    }
    collect_files(&root.join("src"), &mut files);
    files.sort_unstable();

    let mut digest = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(&root).unwrap();
        println!("cargo:rerun-if-changed={}", relative.display());
        hash_field(&mut digest, relative.as_os_str().as_encoded_bytes());
        hash_field(
            &mut digest,
            &fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "failed to read build fingerprint input {}: {error}",
                    path.display()
                )
            }),
        );
    }

    println!(
        "cargo:rustc-env=RSCTF_BUILD_SOURCE_SHA256={}",
        hex::encode(digest.finalize())
    );
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .map(|entry| entry.expect("failed to read source directory entry").path())
        .collect::<Vec<_>>();
    entries.sort_unstable();

    for path in entries {
        if path.is_dir() {
            collect_files(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}
