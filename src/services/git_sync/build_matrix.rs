//! Deterministic CI build discovery for repository-owned Docker contexts.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::package::find_dockerfile_context;

const MAX_DOCKER_TAG_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RepositoryContainerKind {
    Service,
    Generator,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryContainerBuild {
    pub name: String,
    pub context: String,
    pub tag: String,
    pub kind: RepositoryContainerKind,
}

fn relative_utf8(root: &Path, path: &Path, label: &str) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{label} must remain within the repository"))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!("{label} contains a non-normal path component"));
        };
        let component = component
            .to_str()
            .ok_or_else(|| format!("{label} must use UTF-8 path components"))?;
        if component.is_empty() || component.chars().any(char::is_control) {
            return Err(format!("{label} contains an unsafe path component"));
        }
        parts.push(component);
    }
    if parts.is_empty() {
        return Err(format!("{label} cannot be the repository root"));
    }
    Ok(parts.join("/"))
}

fn tag_for(package: &str, kind: RepositoryContainerKind) -> Result<String, String> {
    let package = package.strip_prefix("challenges/").unwrap_or(package);
    let mut tag = String::with_capacity(package.len() + 10);
    let mut separator = false;
    for character in package.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            tag.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator {
            tag.push('-');
            separator = true;
        }
    }
    if kind == RepositoryContainerKind::Generator {
        if !tag.ends_with('-') {
            tag.push('-');
        }
        tag.push_str("generator");
    }
    let tag = tag.trim_matches(['-', '.']).to_string();
    if tag.is_empty()
        || tag.len() > MAX_DOCKER_TAG_BYTES
        || !tag
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return Err(format!(
            "challenge package {package:?} does not produce a safe Docker tag"
        ));
    }
    Ok(tag)
}

fn real_generator_context(package: &Path) -> Option<PathBuf> {
    let context = package.join("generator");
    let context_is_real =
        std::fs::symlink_metadata(&context).is_ok_and(|metadata| metadata.file_type().is_dir());
    let dockerfile_is_real = std::fs::symlink_metadata(context.join("Dockerfile"))
        .is_ok_and(|metadata| metadata.file_type().is_file());
    (context_is_real && dockerfile_is_real).then_some(context)
}

fn push_build(
    builds: &mut Vec<RepositoryContainerBuild>,
    tags: &mut BTreeMap<String, String>,
    root: &Path,
    package: &Path,
    context: PathBuf,
    kind: RepositoryContainerKind,
) -> Result<(), String> {
    let package_path = relative_utf8(root, package, "challenge package")?;
    let package_name = package_path
        .strip_prefix("challenges/")
        .unwrap_or(&package_path);
    let context = relative_utf8(root, &context, "Docker build context")?;
    let tag = tag_for(&package_path, kind)?;
    if let Some(first) = tags.insert(tag.clone(), context.clone()) {
        return Err(format!(
            "Docker tag {tag:?} collides between {first} and {context}"
        ));
    }
    builds.push(RepositoryContainerBuild {
        name: match kind {
            RepositoryContainerKind::Service => package_name.to_string(),
            RepositoryContainerKind::Generator => format!("{package_name} (generator)"),
        },
        context,
        tag,
        kind,
    });
    Ok(())
}

pub(super) fn collect_container_builds(
    root: &Path,
    manifests: &[PathBuf],
) -> Result<Vec<RepositoryContainerBuild>, String> {
    let mut builds = Vec::new();
    let mut tags = BTreeMap::new();
    for manifest in manifests {
        let package = manifest.parent().unwrap_or(root);
        if let Some(context) = find_dockerfile_context(package) {
            push_build(
                &mut builds,
                &mut tags,
                root,
                package,
                context,
                RepositoryContainerKind::Service,
            )?;
        }
        if let Some(context) = real_generator_context(package) {
            push_build(
                &mut builds,
                &mut tags,
                root,
                package,
                context,
                RepositoryContainerKind::Generator,
            )?;
        }
    }
    Ok(builds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rsctf-build-matrix-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn discovers_service_and_generator_without_a_repository_script() {
        let root = root("valid");
        let manifest = root.join("challenges/Jeopardy/Misc/example/challenge.yaml");
        write(&manifest, "name: example\n");
        write(
            &manifest.parent().unwrap().join("src/Dockerfile"),
            "FROM scratch\n",
        );
        write(
            &manifest.parent().unwrap().join("generator/Dockerfile"),
            "FROM scratch\n",
        );

        let builds = collect_container_builds(&root, &[manifest]).unwrap();
        assert_eq!(builds.len(), 2);
        assert_eq!(builds[0].name, "Jeopardy/Misc/example");
        assert_eq!(builds[0].tag, "jeopardy-misc-example");
        assert_eq!(builds[0].kind, RepositoryContainerKind::Service);
        assert_eq!(builds[1].name, "Jeopardy/Misc/example (generator)");
        assert_eq!(builds[1].tag, "jeopardy-misc-example-generator");
        assert_eq!(builds[1].kind, RepositoryContainerKind::Generator);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalized_tag_collisions_fail_closed() {
        let root = root("collision");
        let first = root.join("challenges/Jeopardy/Web/a b/challenge.yaml");
        let second = root.join("challenges/Jeopardy/Web/a-b/challenge.yaml");
        for manifest in [&first, &second] {
            write(manifest, "name: example\n");
            write(
                &manifest.parent().unwrap().join("src/Dockerfile"),
                "FROM scratch\n",
            );
        }
        let error = collect_container_builds(&root, &[first, second]).unwrap_err();
        assert!(error.contains("collides"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
