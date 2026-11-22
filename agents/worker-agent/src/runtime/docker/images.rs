use bollard::image::CreateImageOptions;
use bollard::models::ImageInspect;
use futures_util::TryStreamExt;
use rsctf_worker_protocol::{CommandErrorCode, ImageIdentity};

use super::{docker_error, is_not_found, DockerRuntime};
use crate::runtime::RuntimeError;

impl DockerRuntime {
    /// Resolve the runnable image and its immutable configuration, pulling
    /// a registry digest that is not yet present.
    pub(super) async fn ensure_image(
        &self,
        image: &ImageIdentity,
    ) -> Result<(String, ImageInspect), RuntimeError> {
        let image_name = match image {
            ImageIdentity::RegistryDigest { repository, digest } => {
                format!("{repository}@{digest}")
            }
            ImageIdentity::WorkerLocal {
                worker_id,
                image_id,
            } => {
                if *worker_id != self.worker_id {
                    return Err(RuntimeError::new(
                        CommandErrorCode::InvalidSpec,
                        "worker-local image belongs to a different worker",
                    ));
                }
                image_id.clone()
            }
        };

        match self
            .admission
            .read("inspect_image", self.docker.inspect_image(&image_name))
            .await?
        {
            Ok(inspected) => return Ok((image_name, inspected)),
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(docker_error("inspect image", error)),
        }
        if matches!(image, ImageIdentity::WorkerLocal { .. }) {
            return Err(RuntimeError::new(
                CommandErrorCode::NotFound,
                "worker-local image is not present on this Docker daemon",
            ));
        }

        let options = CreateImageOptions {
            from_image: image_name.clone(),
            ..Default::default()
        };
        // Progress frames are drained and discarded so a slow registry cannot
        // grow memory; the pull deadline drops the stream on expiry.
        self.admission
            .pull("create_image", async {
                let mut progress = self.docker.create_image(Some(options), None, None);
                while progress.try_next().await?.is_some() {}
                Ok::<(), bollard::errors::Error>(())
            })
            .await?
            .map_err(|error| docker_error("pull image", error))?;
        let inspected = self
            .admission
            .read("inspect_image", self.docker.inspect_image(&image_name))
            .await?
            .map_err(|error| docker_error("inspect pulled image", error))?;
        Ok((image_name, inspected))
    }
}
