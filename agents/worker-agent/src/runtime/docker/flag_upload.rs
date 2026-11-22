use super::{DockerRuntime, RuntimeError};
use bytes::Bytes;

#[cfg(unix)]
use rsctf_worker_protocol::CommandErrorCode;

#[cfg(unix)]
pub(super) fn socket_path(endpoint: &str) -> String {
    // Match Bollard's local defaults, then pass this one path to both clients.
    let address = if endpoint == "local" {
        std::env::var("DOCKER_HOST")
            .ok()
            .filter(|host| host.starts_with("unix://"))
            .unwrap_or_else(|| "/var/run/docker.sock".into())
    } else {
        endpoint.into()
    };
    address
        .strip_prefix("unix://")
        .unwrap_or(&address)
        .to_owned()
}

#[cfg(unix)]
pub(super) fn local_client(socket: &str) -> Result<reqwest::Client, RuntimeError> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .unix_socket(std::path::PathBuf::from(socket))
        .build()
        .map_err(|_| {
            RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "open flag archive client on the Docker Unix socket",
            )
        })
}

impl DockerRuntime {
    // Bollard 0.17 omits copyUIDGID from its upload options. Ask the daemon
    // to resolve the image's user and group so the 0600 file is readable by
    // the service, including images that specify a username instead of a UID.
    #[cfg(unix)]
    pub(super) async fn upload_flag_archive(
        &self,
        id: &str,
        parent: &str,
        bytes: Bytes,
    ) -> Result<(), RuntimeError> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RuntimeError::new(
                CommandErrorCode::Internal,
                "Docker returned an invalid flag container ID",
            ));
        }
        let version = self.docker.client_version();
        let url = format!(
            "http://localhost/v{}.{}/containers/{id}/archive",
            version.major_version, version.minor_version
        );
        let response = self
            .admission
            .lifecycle(
                "upload_to_container",
                self.flag_upload_client
                    .put(url)
                    .query(&[
                        ("path", parent),
                        ("copyUIDGID", "1"),
                        ("noOverwriteDirNonDir", "1"),
                    ])
                    .header(reqwest::header::CONTENT_TYPE, "application/x-tar")
                    .body(bytes)
                    .send(),
            )
            .await?
            .map_err(|_| {
                RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    "upload flag archive to local Docker",
                )
            })?;
        if !response.status().is_success() {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                format!("Docker flag upload returned {}", response.status()),
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    pub(super) async fn upload_flag_archive(
        &self,
        id: &str,
        parent: &str,
        bytes: Bytes,
    ) -> Result<(), RuntimeError> {
        self.admission
            .lifecycle(
                "upload_to_container",
                self.docker.upload_to_container(
                    id,
                    Some(bollard::container::UploadToContainerOptions {
                        path: parent,
                        no_overwrite_dir_non_dir: "1",
                    }),
                    bytes,
                ),
            )
            .await?
            .map_err(|error| super::docker_error("upload flag archive", error))
    }
}
