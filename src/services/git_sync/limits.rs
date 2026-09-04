use std::path::Path;

pub(super) fn is_git_object_pack(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    components
        .next()
        .is_some_and(|part| part.as_os_str() == ".git")
        && components
            .next()
            .is_some_and(|part| part.as_os_str() == "objects")
        && components
            .next()
            .is_some_and(|part| part.as_os_str() == "pack")
        && components.next().is_some()
}
