use bollard::models::{BuildInfo, BuildInfoAux};

/// Normalizes Docker's legacy text stream and BuildKit protobuf progress into
/// the same bounded build log and terminal failure used by the build seam.
/// BuildKit reports Dockerfile failures on vertices rather than `BuildInfo.error`.
#[derive(Default)]
pub(super) struct BuildProgress {
    log: String,
    failed: Option<String>,
}

impl BuildProgress {
    pub(super) fn record(&mut self, info: BuildInfo) {
        if let Some(stream) = info.stream {
            self.log.push_str(&stream);
        }
        if let Some(error) = info.error {
            self.fail(error);
        }

        let Some(BuildInfoAux::BuildKit(status)) = info.aux else {
            return;
        };
        for entry in status.logs {
            self.log.push_str(&String::from_utf8_lossy(&entry.msg));
        }
        for warning in status.warnings {
            if warning.short.is_empty() {
                continue;
            }
            self.log.push_str("warning: ");
            self.log.push_str(&String::from_utf8_lossy(&warning.short));
            if !self.log.ends_with('\n') {
                self.log.push('\n');
            }
        }
        for vertex in status.vertexes {
            if !vertex.error.trim().is_empty() {
                self.fail(vertex.error);
            }
        }
    }

    pub(super) fn fail(&mut self, error: String) {
        if self.failed.is_none() {
            self.failed = Some(error);
        }
    }

    pub(super) fn into_parts(self) -> (String, Option<String>) {
        (self.log, self.failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_buildkit_aux_frames_decode_as_progress() {
        // Captured from Docker 28's `/build?version=2` response. Without
        // Bollard's BuildKit feature this is incorrectly decoded as ImageId.
        let encoded = "Cm8KR3NoYTI1Njo2ZDAwZTM2MTdjZWVmNWIxNjAxYWY1OTZlNDljYmExMzQ2NjIyZThiZDUxZTI0YmUzY2VhZDlkZTlhZGUwMjI4GiRbaW50ZXJuYWxdIGxvYWQgcmVtb3RlIGJ1aWxkIGNvbnRleHQKfQpHc2hhMjU2OjZkMDBlMzYxN2NlZWY1YjE2MDFhZjU5NmU0OWNiYTEzNDY2MjJlOGJkNTFlMjRiZTNjZWFkOWRlOWFkZTAyMjgaJFtpbnRlcm5hbF0gbG9hZCByZW1vdGUgYnVpbGQgY29udGV4dCoMCLXlotQGEKG708EC";
        let info: BuildInfo = serde_json::from_value(serde_json::json!({ "aux": encoded }))
            .expect("BuildKit aux protobuf must decode");
        assert!(matches!(info.aux, Some(BuildInfoAux::BuildKit(_))));
    }

    #[test]
    fn buildkit_logs_and_vertex_errors_are_preserved() {
        let mut status = bollard::moby::buildkit::v1::StatusResponse::default();
        status.logs.push(bollard::moby::buildkit::v1::VertexLog {
            msg: b"compiler output\n".to_vec(),
            ..Default::default()
        });
        status.vertexes.push(bollard::moby::buildkit::v1::Vertex {
            error: "Dockerfile step failed".to_string(),
            ..Default::default()
        });
        let mut progress = BuildProgress::default();
        progress.record(BuildInfo {
            aux: Some(BuildInfoAux::BuildKit(status)),
            ..Default::default()
        });
        let (log, failed) = progress.into_parts();
        assert_eq!(log, "compiler output\n");
        assert_eq!(failed.as_deref(), Some("Dockerfile step failed"));
    }
}
