//! Admitted image inspection and immutable repository-digest pulls.

use bollard::auth::DockerCredentials;
use bollard::image::CreateImageOptions;
use bollard::models::ImageInspect;
use futures::StreamExt;

use super::super::DockerContainerManager;
use crate::services::docker_admission::docker_admission;
use crate::utils::error::{AppError, AppResult};

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

    /// Pull only with credentials for the exact registry selected by the administrator.
    /// The caller inspects the daemon after the progress stream finishes.
    pub(in crate::services::container) async fn pull_repository_digest(
        &self,
        image: &str,
    ) -> AppResult<Option<String>> {
        let docker = self.client()?;
        let credentials = self.registry_credentials(image).await?;
        let authenticated = credentials.is_some();
        let options = CreateImageOptions {
            from_image: image.to_string(),
            ..Default::default()
        };
        Ok(docker_admission()
            .pull("create_image", async {
                let mut pull = docker.create_image(Some(options), None, credentials);
                while let Some(item) = pull.next().await {
                    if let Err(error) = item {
                        // A private registry or auth proxy controls the error payload.
                        return Some(if authenticated {
                            "authenticated registry pull failed".to_owned()
                        } else {
                            error.to_string()
                        });
                    }
                }
                None
            })
            .await?)
    }

    async fn registry_credentials(&self, image: &str) -> AppResult<Option<DockerCredentials>> {
        let Some(pool) = &self.pull_settings else {
            return Ok(None);
        };
        let Some((host, _)) = image.split_once('/') else {
            return Ok(None);
        };
        if !(host == "localhost" || host.contains('.') || host.contains(':'))
            || !crate::services::challenge_images::is_repository_digest(image)
        {
            return Ok(None);
        }
        let credentials = sqlx::query_as::<_, (String, String)>(
            r#"SELECT u.value, p.value FROM "Configs" s
               JOIN "Configs" u ON u.config_key='RegistryConfig:UserName'
               JOIN "Configs" p ON p.config_key='RegistryConfig:Password'
               WHERE s.config_key='RegistryConfig:ServerAddress'
                 AND lower(rtrim(s.value,'/')) IN (lower($1::text), 'https://' || lower($1))
                 AND u.value<>'' AND p.value<>''"#,
        )
        .bind(host)
        .fetch_optional(pool)
        .await
        .map_err(|_| AppError::unavailable("Could not read registry pull settings"))?;
        Ok(credentials.map(|(username, password)| DockerCredentials {
            username: Some(username),
            password: Some(password),
            serveraddress: Some(host.to_owned()),
            ..Default::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::container::ContainerManager;

    #[tokio::test]
    #[ignore = "requires the disposable authenticated registry from test_stage_registry.py"]
    async fn cold_docker_pull_uses_only_the_configured_registry_credential() {
        let image = std::env::var("RSCTF_TEST_PRIVATE_PULL_REF").unwrap();
        let (host, _) = image.split_once('/').unwrap();
        let endpoint: std::net::SocketAddr = host.parse().unwrap();
        assert!(endpoint.ip().is_loopback());
        let schema = std::env::var("RSCTF_TEST_PRIVATE_PULL_SCHEMA").unwrap();
        let options: sqlx::postgres::PgConnectOptions = std::env::var("RSCTF_TEST_DATABASE_URL")
            .unwrap()
            .parse()
            .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.options([("search_path", schema)]))
            .await
            .unwrap();
        let mut manager = DockerContainerManager::connect().unwrap();
        assert!(!manager.image_exists(&image).await);
        assert!(manager.pull_image(&image).await.is_err());
        assert!(!manager.image_exists(&image).await);
        manager.pull_settings = Some(pool);
        let foreign = image.replacen(host, "other.example", 1);
        assert!(manager
            .registry_credentials(&foreign)
            .await
            .unwrap()
            .is_none());
        manager.pull_image(&image).await.unwrap();
        assert!(manager.image_exists(&image).await);
    }
}
