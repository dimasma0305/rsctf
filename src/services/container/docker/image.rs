//! Admitted image inspection and immutable repository-digest pulls.

use bollard::image::CreateImageOptions;
use bollard::models::ImageInspect;
use futures::StreamExt;

use super::super::DockerContainerManager;
use crate::services::docker_admission::docker_admission;
use crate::utils::error::AppResult;

impl DockerContainerManager {
    /// Inspect under read admission. The inner result keeps Docker's own
    /// error so callers can still distinguish an absent image.
    pub(in crate::services::container) async fn inspect_image(
        &self,
        image: &str,
    ) -> AppResult<Result<ImageInspect, bollard::errors::Error>> {
        let docker = self.client()?;
        Ok(docker_admission()
            .read("inspect_image", docker.inspect_image(image))
            .await?)
    }

    /// Best-effort pull of an immutable repository digest. A pull failure is
    /// logged and left for the follow-up inspect to report; a pull that
    /// exceeds the pull deadline drops its progress stream and surfaces the
    /// retryable admission error.
    pub(in crate::services::container) async fn pull_repository_digest(
        &self,
        image: &str,
    ) -> AppResult<()> {
        let docker = self.client()?;
        let options = CreateImageOptions {
            from_image: image.to_string(),
            ..Default::default()
        };
        docker_admission()
            .pull("create_image", async {
                let mut pull = docker.create_image(Some(options), None, None);
                while let Some(item) = pull.next().await {
                    if let Err(error) = item {
                        tracing::warn!(%image, %error, "immutable image pull failed");
                        break;
                    }
                }
            })
            .await?;
        Ok(())
    }
}
