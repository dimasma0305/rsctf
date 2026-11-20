use bollard::models::{BuildInfo, BuildInfoAux};

/// Normalizes Docker's legacy text stream and BuildKit protobuf progress into
/// the same bounded build log and terminal failure used by the build seam.
/// BuildKit reports Dockerfile failures on vertices rather than `BuildInfo.error`.
#[derive(Default)]
pub(super) struct BuildProgress {
    log: String,
    failed: Option<String>,
    image_id: Option<String>,
}

impl BuildProgress {
    pub(super) fn record(&mut self, info: BuildInfo) {
        if let Some(stream) = info.stream {
            self.log.push_str(&stream);
        }
        if let Some(error) = info
            .error
            .or_else(|| info.error_detail.and_then(|e| e.message))
        {
            self.fail(error);
        }

        match info.aux {
            Some(BuildInfoAux::BuildKit(status)) => self.record_status(status),
            Some(BuildInfoAux::Default(image))
                if info.id.as_deref().is_none_or(|id| id == "moby.image.id") =>
            {
                match image.id {
                    Some(id) if crate::services::challenge_images::is_local_image_id(&id) => {
                        if self
                            .image_id
                            .as_ref()
                            .is_some_and(|previous| previous != &id)
                        {
                            self.fail("Docker reported conflicting build image IDs".into());
                        } else {
                            self.image_id = Some(id);
                        }
                    }
                    _ => self.fail("Docker reported an invalid build image ID".into()),
                }
            }
            _ => {}
        }
        self.trim_log();
    }

    fn record_status(&mut self, status: bollard::moby::buildkit::v1::StatusResponse) {
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
                self.fail(if vertex.name.is_empty() {
                    vertex.error
                } else {
                    format!("{}: {}", vertex.name, vertex.error)
                });
            }
        }
    }

    fn trim_log(&mut self) {
        if let Some(start) = super::truncated_tail_start(&self.log, 16 * 1024) {
            self.log.drain(..start);
        }
    }

    pub(super) fn fail(&mut self, error: String) {
        if self.failed.is_none() {
            self.failed = Some(error);
        }
    }

    pub(super) fn into_parts(self) -> (String, Result<String, String>) {
        let result = match self.failed {
            Some(error) => Err(error),
            None => self
                .image_id
                .ok_or_else(|| "Docker build stream ended without an image ID".into()),
        };
        (self.log, result)
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
    fn build_receipt_must_identify_one_output_and_cannot_override_a_failure() {
        let image = format!("sha256:{}", "a".repeat(64));
        let other = format!("sha256:{}", "b".repeat(64));
        let receipt = |id: &str| serde_json::json!({"id":"moby.image.id","aux":{"ID":id}});
        let read = |frames: Vec<serde_json::Value>| {
            let mut progress = BuildProgress::default();
            for frame in frames {
                progress.record(serde_json::from_value(frame).unwrap());
            }
            progress.into_parts().1
        };
        assert!(read(vec![]).is_err());
        assert!(read(vec![
            serde_json::json!({"stream":format!("Successfully built {image}")})
        ])
        .is_err());
        assert!(read(vec![
            serde_json::json!({"id":"different-result","aux":{"ID":image}})
        ])
        .is_err());
        assert!(read(vec![receipt("invalid")]).is_err());
        assert!(read(vec![receipt(&image), receipt(&other)]).is_err());
        assert_eq!(read(vec![receipt(&image), receipt(&image)]).unwrap(), image);
        assert_eq!(
            read(vec![
                receipt(&image),
                serde_json::json!({"errorDetail":{"message":"export failed"}})
            ])
            .unwrap_err(),
            "export failed"
        );
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
        let (log, result) = progress.into_parts();
        assert_eq!(log, "compiler output\n");
        assert_eq!(result.unwrap_err(), "Dockerfile step failed");
    }

    #[test]
    fn chatty_build_logs_are_bounded_while_streaming() {
        let mut progress = BuildProgress::default();
        for _ in 0..100 {
            progress.record(BuildInfo {
                stream: Some("é".repeat(1024)),
                ..Default::default()
            });
        }
        progress.record(BuildInfo {
            stream: Some("last build step".into()),
            ..Default::default()
        });
        let (log, _) = progress.into_parts();
        assert!(log.len() <= 16 * 1024);
        assert!(log.ends_with("last build step"));
    }
}
