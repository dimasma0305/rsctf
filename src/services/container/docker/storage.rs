//! Probe actual overlay2 quota support once, including XFS without project quotas.
//! Never turn a daemon/permission failure into an unsupported-storage result.
use bollard::{
    container::{Config, CreateContainerOptions, RemoveContainerOptions},
    image::{BuildImageOptions, RemoveImageOptions},
    models::{HostConfig, SystemInfo},
    Docker,
};
use futures::StreamExt;

use super::{docker_admission, writable_layer_quota_supported, writable_layer_storage_opt};
use crate::utils::error::{AppError, AppResult};

fn quota_unavailable(error: &bollard::errors::Error) -> bool {
    let bollard::errors::Error::DockerResponseServerError {
        status_code: 500 | 400,
        message,
    } = error
    else {
        return false;
    };
    let message = message.to_ascii_lowercase();
    message.contains("storage-opt")
        && (message.contains("project quota")
            || message.contains("pquota")
            || (message.contains("size") && message.contains("not supported")))
}

pub(in crate::services::container) async fn probe(
    docker: &Docker,
    info: &SystemInfo,
) -> AppResult<bool> {
    if !writable_layer_quota_supported(info) {
        return Ok(false);
    }
    if info.driver.as_deref() != Some("overlay2") {
        return Ok(true);
    }
    let name = format!("rsctf-quota-probe-{}", uuid::Uuid::new_v4().simple());
    let image = format!("{name}:local");
    let mut archive = tar::Builder::new(Vec::new());
    let contents = b"FROM scratch\nCMD [\"/quota-probe\"]\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();
    archive
        .append_data(&mut header, "Dockerfile", &contents[..])
        .map_err(|error| AppError::internal(error.to_string()))?;
    let context = archive
        .into_inner()
        .map_err(|error| AppError::internal(error.to_string()))?;
    let result = async {
        let build = async {
            let mut stream = docker.build_image(
                BuildImageOptions::<String> {
                    dockerfile: "Dockerfile".into(),
                    t: image.clone(),
                    networkmode: "none".into(),
                    rm: true,
                    forcerm: true,
                    ..Default::default()
                },
                None,
                Some(context.into()),
            );
            while let Some(item) = stream.next().await {
                let item = item.map_err(|error| AppError::unavailable(error.to_string()))?;
                if let Some(error) = item.error {
                    return Err(AppError::unavailable(error));
                }
            }
            Ok::<_, AppError>(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), build)
            .await
            .map_err(|_| AppError::unavailable("Docker quota probe timed out"))??;
        match docker_admission()
            .lifecycle(
                "quota_probe_create",
                docker.create_container(
                    Some(CreateContainerOptions {
                        name: name.clone(),
                        platform: None,
                    }),
                    Config {
                        image: Some(image.clone()),
                        host_config: Some(HostConfig {
                            network_mode: Some("none".into()),
                            storage_opt: Some(writable_layer_storage_opt(1)),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ),
            )
            .await?
        {
            Ok(_) => Ok(true),
            Err(error) if quota_unavailable(&error) => Ok(false),
            Err(error) => Err(AppError::unavailable(format!(
                "Docker quota probe failed: {error}"
            ))),
        }
    }
    .await;
    // Probe containers never run. Always clean the known names, even on an error.
    let removed = docker_admission()
        .lifecycle(
            "quota_probe_remove",
            docker.remove_container(
                &name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            ),
        )
        .await?;
    let image_removed = docker_admission()
        .lifecycle(
            "quota_probe_image_remove",
            docker.remove_image(
                &image,
                Some(RemoveImageOptions {
                    force: true,
                    ..Default::default()
                }),
                None,
            ),
        )
        .await?;
    for outcome in [removed.map(|_| ()), image_removed.map(|_| ())] {
        if let Err(error) = outcome {
            if !matches!(
                error,
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                }
            ) {
                return Err(AppError::unavailable(format!(
                    "Docker quota probe cleanup failed: {error}"
                )));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn overlay_probe_checks_actual_support_and_cleans_without_starting() {
        use axum::{
            extract::State,
            http::{Method, StatusCode, Uri},
            response::IntoResponse,
            routing::any,
            Router,
        };
        use std::sync::{Arc, Mutex};
        type StateData = (u16, Arc<Mutex<Vec<String>>>);
        async fn daemon(
            State((code, calls)): State<StateData>,
            method: Method,
            uri: Uri,
        ) -> impl IntoResponse {
            calls
                .lock()
                .unwrap()
                .push(format!("{method} {}", uri.path()));
            let (status, body) = if uri.path().ends_with("/build") {
                (200, serde_json::json!({"stream":"probe built"}))
            } else if uri.path().ends_with("/containers/create") {
                if code == 200 {
                    (201, serde_json::json!({"Id":"probe","Warnings":[]}))
                } else {
                    (
                        code,
                        serde_json::json!({"message":if code == 500 { "--storage-opt is supported only for overlay over xfs with 'pquota' mount option" } else { "permission denied" }}),
                    )
                }
            } else if method == Method::DELETE {
                (200, serde_json::json!([]))
            } else {
                (400, serde_json::json!({"message":"unexpected request"}))
            };
            (StatusCode::from_u16(status).unwrap(), axum::Json(body))
        }
        for code in [200, 500, 403] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let app = Router::new()
                .route("/{*path}", any(daemon))
                .with_state((code, calls.clone()));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let docker = Docker::connect_with_http(
                &format!("http://{address}"),
                5,
                bollard::API_DEFAULT_VERSION,
            )
            .unwrap();
            let result = probe(
                &docker,
                &SystemInfo {
                    driver: Some("overlay2".into()),
                    driver_status: Some(vec![vec!["Backing Filesystem".into(), "xfs".into()]]),
                    ..Default::default()
                },
            )
            .await;
            server.abort();
            if code == 403 {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), code == 200);
            }
            let calls = calls.lock().unwrap();
            assert!(calls
                .iter()
                .any(|call| call.starts_with("DELETE ") && call.contains("/containers/")));
            assert!(calls
                .iter()
                .any(|call| call.starts_with("DELETE ") && call.contains("/images/")));
            assert!(!calls.iter().any(|call| call.contains("/start")));
        }
    }
    #[test]
    fn only_a_quota_rejection_allows_fallback() {
        for (status_code, message, expected) in [
            (500, "--storage-opt is supported only for overlay over xfs with 'pquota' mount option", true),
            (500, "--storage-opt size is supported only for overlay over xfs with 'pquota' mount option", true),
            (500, "permission denied", false), (500, "no space left on device", false),
            (403, "storage-opt size not supported", false),
        ] {
            assert_eq!(quota_unavailable(&bollard::errors::Error::DockerResponseServerError {
                status_code, message: message.into()
            }), expected);
        }
    }
}
