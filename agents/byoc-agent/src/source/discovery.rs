//! Discover source inside the single challenge directory selected by Compose.
use super::*;

const MAX_ENTRIES: usize = 2048;

fn source_file(name: &str) -> bool {
    matches!(
        name,
        "Dockerfile"
            | "Makefile"
            | "CMakeLists.txt"
            | "Cargo.toml"
            | "go.mod"
            | "package.json"
            | "requirements.txt"
    ) || matches!(
        Path::new(name).extension().and_then(|s| s.to_str()),
        Some(
            "rs" | "py"
                | "go"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "cc"
                | "java"
                | "kt"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "mjs"
                | "cjs"
                | "rb"
                | "php"
                | "sh"
                | "bash"
                | "cs"
                | "lua"
                | "sql"
                | "html"
                | "css"
                | "vue"
                | "svelte"
        )
    )
}

fn open_on_source_mount(root: &File, path: &str, flags: OFlags) -> io::Result<Option<File>> {
    match openat2(
        root,
        path,
        flags | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
    ) {
        Ok(fd) => Ok(Some(File::from(fd))),
        // Files can also disappear between readdir and open during a team edit.
        Err(rustix::io::Errno::XDEV | rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn discover(root: &File) -> io::Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![String::new()];
    let mut inspected = 0;
    while let Some(prefix) = pending.pop() {
        let dir = if prefix.is_empty() {
            root.try_clone()?
        } else {
            let Some(dir) =
                open_on_source_mount(root, &prefix, OFlags::RDONLY | OFlags::DIRECTORY)?
            else {
                continue;
            };
            dir
        };
        for entry in std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))? {
            inspected += 1;
            if inspected > MAX_ENTRIES {
                return Err(invalid());
            }
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.')
                || name.trim() != name
                || name.contains('\\')
                || name.chars().any(char::is_control)
            {
                continue;
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if matches!(
                    name.as_str(),
                    "node_modules"
                        | "vendor"
                        | "target"
                        | "dist"
                        | "build"
                        | "__pycache__"
                        | "venv"
                        | "secrets"
                ) {
                    continue;
                }
                if path.len() > 240 || path.split('/').count() > 16 {
                    return Err(invalid());
                }
                pending.push(path);
            } else if kind.is_file() && source_file(&name) {
                // read_dir reports a bind-mounted file as regular. Check its
                // mount without reading the payload or counting it as source.
                if open_on_source_mount(&dir, &name, OFlags::PATH)?.is_none() {
                    continue;
                }
                if path.len() > 240 || files.len() >= MAX_FILES {
                    return Err(invalid());
                }
                files.insert(path);
            }
        }
    }
    Ok(files)
}
