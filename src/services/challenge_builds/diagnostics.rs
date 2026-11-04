//! Actionable build failures without treating a failed pull as a runnable image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailureKind {
    RegistryUnauthorized,
    ManifestMissing,
    DockerfileError,
    ResourceExhausted,
    BuildFailure,
}

pub(super) fn classify(message: &str) -> FailureKind {
    let message = message.to_ascii_lowercase();
    if [
        "insufficient_scope",
        "unauthorized",
        "authentication required",
        "pull access denied",
    ]
    .iter()
    .any(|text| message.contains(text))
    {
        FailureKind::RegistryUnauthorized
    } else if [
        "manifest unknown",
        "manifest not found",
        "no matching manifest",
    ]
    .iter()
    .any(|text| message.contains(text))
    {
        FailureKind::ManifestMissing
    } else if [
        "no space left on device",
        "out of memory",
        "cannot allocate memory",
    ]
    .iter()
    .any(|text| message.contains(text))
    {
        FailureKind::ResourceExhausted
    } else if [
        "dockerfile parse error",
        "unknown instruction",
        "dockerfile syntax",
    ]
    .iter()
    .any(|text| message.contains(text))
    {
        FailureKind::DockerfileError
    } else {
        FailureKind::BuildFailure
    }
}

pub(super) fn explain(message: &str) -> String {
    let kind = classify(message);
    let advice = match kind {
        FailureKind::RegistryUnauthorized => "The build worker cannot access an image. Check the image name and tag, grant that worker registry access, or use an accessible base image. Retrying unchanged credentials will not fix this.",
        FailureKind::ManifestMissing => "Check that the image tag exists and supports the worker's CPU architecture.",
        FailureKind::DockerfileError => "Correct the Dockerfile instruction reported above, then rebuild.",
        FailureKind::ResourceExhausted => "Check the build worker's available disk and memory before rebuilding.",
        FailureKind::BuildFailure => "Check the failing build step above before rebuilding.",
    };
    format!("{message}\n[{kind:?}] {advice}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_android_base_image_is_an_access_failure() {
        let error = "[internal] load metadata for docker.io/budtmo2/docker-android-pro:emulator_headless_10.0: insufficient_scope: authorization failed";
        assert_eq!(classify(error), FailureKind::RegistryUnauthorized);
        let explained = explain(error);
        assert!(explained.contains("budtmo2/docker-android-pro:emulator_headless_10.0"));
        assert!(explained.contains("Retrying unchanged credentials"));
        assert_eq!(classify("manifest unknown"), FailureKind::ManifestMissing);
        assert_eq!(
            classify("no space left on device"),
            FailureKind::ResourceExhausted
        );
        assert_eq!(
            classify("Dockerfile parse error line 4"),
            FailureKind::DockerfileError
        );
        assert_eq!(
            classify("connection reset by peer"),
            FailureKind::BuildFailure
        );
    }
}
