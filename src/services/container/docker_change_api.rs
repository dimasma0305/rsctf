//! Bounded Docker Engine client for the container filesystem-changes endpoint.
//!
//! Bollard's `container_changes` API deserializes the complete daemon response
//! into a `Vec` before returning. A participant controls that response through
//! its writable layer, so the admin forensics endpoint uses this narrow client
//! instead and cancels the daemon body as soon as the declared byte ceiling is
//! crossed.

use std::path::{Path, PathBuf};

use bollard::models::FilesystemChange;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::StatusCode;
use hyper_util::client::legacy::Client;
use hyperlocal::{UnixClientExt, UnixConnector};

use crate::utils::error::{AppError, AppResult};

/// The public response is capped at 448 KiB. Allow JSON escaping and field
/// names some headroom while keeping the only daemon-controlled allocation at
/// a small, explicit ceiling.
const MAX_DOCKER_CHANGE_BODY_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_DOCKER_SOCKET: &str = "/var/run/docker.sock";

#[derive(Debug, PartialEq, Eq)]
enum DockerChangesEndpoint {
    Unix(PathBuf),
    Http(String),
}

fn endpoint(raw: Option<&str>) -> AppResult<DockerChangesEndpoint> {
    let raw = raw.filter(|value| !value.trim().is_empty());
    let Some(raw) = raw else {
        return Ok(DockerChangesEndpoint::Unix(PathBuf::from(
            DEFAULT_DOCKER_SOCKET,
        )));
    };
    if let Some(path) = raw.strip_prefix("unix://") {
        if path.is_empty() {
            return Err(AppError::internal(
                "Docker Unix endpoint has no socket path",
            ));
        }
        return Ok(DockerChangesEndpoint::Unix(PathBuf::from(path)));
    }
    if raw.starts_with("npipe://") || raw.starts_with("https://") {
        return Err(AppError::unavailable(
            "Bounded filesystem inspection is unavailable for this Docker transport",
        ));
    }
    let authority = raw
        .strip_prefix("tcp://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw)
        .trim_end_matches('/');
    if authority.is_empty() || authority.contains('/') {
        return Err(AppError::internal("Docker HTTP endpoint is invalid"));
    }
    Ok(DockerChangesEndpoint::Http(format!("http://{authority}")))
}

fn append_body(bytes: &mut Vec<u8>, chunk: &[u8]) -> AppResult<()> {
    let next = bytes
        .len()
        .checked_add(chunk.len())
        .filter(|size| *size <= MAX_DOCKER_CHANGE_BODY_BYTES)
        .ok_or_else(|| {
            AppError::payload_too_large(
                "Container filesystem change set exceeds the inspection limit",
            )
        })?;
    bytes
        .try_reserve(next - bytes.len())
        .map_err(|_| AppError::internal("reserve Docker change response"))?;
    bytes.extend_from_slice(chunk);
    Ok(())
}

async fn collect_body(
    mut response: hyper::Response<hyper::body::Incoming>,
    container_id: &str,
) -> AppResult<Vec<u8>> {
    if response.status() == StatusCode::NOT_FOUND {
        return Err(AppError::not_found(format!(
            "container not found: {container_id}"
        )));
    }
    if !response.status().is_success() {
        return Err(AppError::internal(format!(
            "Docker changes endpoint returned {}",
            response.status()
        )));
    }
    if response
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|size| size > MAX_DOCKER_CHANGE_BODY_BYTES)
    {
        return Err(AppError::payload_too_large(
            "Container filesystem change set exceeds the inspection limit",
        ));
    }

    let mut bytes = Vec::new();
    while let Some(frame) = response.body_mut().frame().await {
        let frame = frame
            .map_err(|error| AppError::internal(format!("read Docker change response: {error}")))?;
        if let Some(chunk) = frame.data_ref() {
            append_body(&mut bytes, chunk)?;
        }
    }
    Ok(bytes)
}

async fn unix_request(socket: &Path, path: &str, container_id: &str) -> AppResult<Vec<u8>> {
    let client: Client<UnixConnector, Full<Bytes>> = Client::unix();
    let uri = hyperlocal::Uri::new(socket, path).into();
    let response = client
        .get(uri)
        .await
        .map_err(|error| AppError::internal(format!("request Docker changes: {error}")))?;
    collect_body(response, container_id).await
}

async fn http_request(base: &str, path: &str, container_id: &str) -> AppResult<Vec<u8>> {
    let client = Client::builder(hyper_util::rt::TokioExecutor::new()).build_http::<Full<Bytes>>();
    let uri = format!("{base}{path}")
        .parse()
        .map_err(|_| AppError::internal("Docker changes URI is invalid"))?;
    let response = client
        .get(uri)
        .await
        .map_err(|error| AppError::internal(format!("request Docker changes: {error}")))?;
    collect_body(response, container_id).await
}

pub(super) async fn container_changes(
    configured_endpoint: Option<&str>,
    container_id: &str,
) -> AppResult<Vec<FilesystemChange>> {
    if container_id.is_empty()
        || !container_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::internal("Docker container identity is invalid"));
    }
    let path = format!("/containers/{container_id}/changes");
    let bytes = match endpoint(configured_endpoint)? {
        DockerChangesEndpoint::Unix(socket) => unix_request(&socket, &path, container_id).await?,
        DockerChangesEndpoint::Http(base) => http_request(&base, &path, container_id).await?,
    };
    serde_json::from_slice::<Option<Vec<FilesystemChange>>>(&bytes)
        .map(Option::unwrap_or_default)
        .map_err(|error| AppError::internal(format!("decode Docker change response: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_normalized_without_accepting_unbounded_transports() {
        assert_eq!(
            endpoint(None).unwrap(),
            DockerChangesEndpoint::Unix(PathBuf::from(DEFAULT_DOCKER_SOCKET))
        );
        assert_eq!(
            endpoint(Some("unix:///run/docker.sock")).unwrap(),
            DockerChangesEndpoint::Unix(PathBuf::from("/run/docker.sock"))
        );
        assert_eq!(
            endpoint(Some("tcp://docker.internal:2375")).unwrap(),
            DockerChangesEndpoint::Http("http://docker.internal:2375".into())
        );
        assert!(endpoint(Some("https://docker.internal:2376")).is_err());
        assert!(endpoint(Some("npipe:////./pipe/docker_engine")).is_err());
    }

    #[test]
    fn daemon_body_is_rejected_before_crossing_the_memory_ceiling() {
        let mut bytes = Vec::new();
        append_body(&mut bytes, &[0; 32]).unwrap();
        assert!(append_body(&mut bytes, &vec![0; MAX_DOCKER_CHANGE_BODY_BYTES]).is_err());
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn bounded_json_keeps_the_docker_wire_contract() {
        let changes: Option<Vec<FilesystemChange>> =
            serde_json::from_slice(br#"[{"Path":"/tmp/result","Kind":1}]"#).unwrap();
        let changes = changes.unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "/tmp/result");
    }

    #[tokio::test]
    #[ignore = "requires RSCTF_TEST_CONTAINER_ID on a disposable Docker container"]
    async fn live_daemon_response_is_streamed_and_decoded() {
        let container_id = std::env::var("RSCTF_TEST_CONTAINER_ID")
            .expect("RSCTF_TEST_CONTAINER_ID must name a disposable container");
        let changes =
            container_changes(std::env::var("DOCKER_HOST").ok().as_deref(), &container_id)
                .await
                .unwrap();
        assert!(changes
            .iter()
            .any(|change| change.path == "/tmp/rsctf-change"));
    }
}
