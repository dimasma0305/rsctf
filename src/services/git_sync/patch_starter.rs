//! Prepare a starter ZIP from the immutable organizer repository snapshot.

use std::io::Read;
use std::path::Path;

use crate::services::ad::patches::starter::Registration;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};
use crate::utils::upload::{reserve_buffered, SOURCE_ARCHIVE_BLOB_BYTES};

use super::package::{find_dockerfile_context, zip_context_dir_bounded};
use super::{AdSection, MAX_REPO_FILE_BYTES};

pub(super) struct PreparedStarter {
    pub(super) archive: Vec<u8>,
    pub(super) registration: Registration,
    _permit: tokio::sync::SemaphorePermit<'static>,
}

pub(super) async fn prepare(
    ad: Option<&AdSection>,
    challenge_type: ChallengeType,
    package: &Path,
) -> AppResult<Option<PreparedStarter>> {
    let Some((section, filename)) =
        ad.and_then(|section| section.starter_zip.as_deref().map(|file| (section, file)))
    else {
        return Ok(None);
    };
    if challenge_type != ChallengeType::AttackDefense || section.self_hosted == Some(true) {
        return Err(AppError::bad_request(
            "ad.starterZip applies only to platform-hosted AttackDefense",
        ));
    }
    if filename == "src" {
        return pack_source(package).await.map(Some);
    }
    if filename.len() > 255
        || !filename.ends_with(".zip")
        || filename.len() == 4
        || filename
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"._-".contains(&byte))
    {
        return Err(AppError::bad_request(
            "ad.starterZip must be src or the name of a .zip file beside challenge.yaml",
        ));
    }
    let path = package.join(filename);
    tokio::task::spawn_blocking(move || read_starter(&path))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .map(Some)
}

async fn pack_source(package: &Path) -> AppResult<PreparedStarter> {
    let src = package.join("src");
    if find_dockerfile_context(package).as_deref() != Some(src.as_path()) {
        return Err(AppError::bad_request(
            "ad.starterZip: src requires a regular src/Dockerfile",
        ));
    }
    let permit = reserve_archive(MAX_REPO_FILE_BYTES)?;
    let archive = zip_context_dir_bounded(&src, MAX_REPO_FILE_BYTES).await?;
    tokio::task::spawn_blocking(move || validate(archive, permit))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
}

fn reserve_archive(size: u64) -> AppResult<tokio::sync::SemaphorePermit<'static>> {
    reserve_buffered(16 * 1024 * 1024 + 3 * size as usize)
}

fn read_starter(path: &Path) -> AppResult<PreparedStarter> {
    let invalid_file =
        || AppError::bad_request("ad.starterZip must refer to a readable regular file");
    let metadata = std::fs::symlink_metadata(path).map_err(|_| invalid_file())?;
    if !metadata.is_file() {
        return Err(invalid_file());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| invalid_file())?;
    let metadata = file.metadata().map_err(|_| invalid_file())?;
    if !metadata.is_file() {
        return Err(invalid_file());
    }
    let size = metadata.len();
    if size > MAX_REPO_FILE_BYTES {
        return Err(AppError::bad_request(
            "ad.starterZip exceeds the 64 MiB repository file limit",
        ));
    }
    let permit = reserve_archive(size)?;
    let mut archive = Vec::with_capacity(size as usize);
    file.take(size + 1)
        .read_to_end(&mut archive)
        .map_err(|_| invalid_file())?;
    if archive.len() as u64 != size {
        return Err(AppError::bad_request(
            "ad.starterZip changed while it was being read",
        ));
    }
    validate(archive, permit)
}

fn validate(
    archive: Vec<u8>,
    permit: tokio::sync::SemaphorePermit<'static>,
) -> AppResult<PreparedStarter> {
    if archive.len() > SOURCE_ARCHIVE_BLOB_BYTES {
        return Err(AppError::bad_request(
            "ad.starterZip exceeds the 72 MiB ZIP limit",
        ));
    }
    let registration = Registration::from_zip(&archive)
        .map_err(|error| AppError::bad_request(format!("ad.starterZip: {error}")))?;
    Ok(PreparedStarter {
        archive,
        registration,
        _permit: permit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    async fn prepare_hosted(ad: &AdSection, package: &Path) -> AppResult<Option<PreparedStarter>> {
        prepare(Some(ad), ChallengeType::AttackDefense, package).await
    }

    #[tokio::test]
    async fn validates_repository_starter_zip_and_rejects_wrong_scope_or_path() {
        let package =
            std::env::temp_dir().join(format!("rsctf-patch-starter-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&package).unwrap();
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("Dockerfile", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"FROM scratch\n").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let archive = package.join("starter.zip");
        std::fs::write(&archive, &bytes).unwrap();
        let mut ad = AdSection {
            starter_zip: Some("starter.zip".into()),
            ..Default::default()
        };
        assert_eq!(
            prepare_hosted(&ad, &package)
                .await
                .unwrap()
                .unwrap()
                .archive,
            bytes
        );
        assert!(prepare(Some(&ad), ChallengeType::KingOfTheHill, &package)
            .await
            .is_err());
        ad.self_hosted = Some(true);
        assert!(prepare_hosted(&ad, &package).await.is_err());
        ad.self_hosted = Some(false);
        ad.starter_zip = Some("../starter.zip".into());
        assert!(prepare_hosted(&ad, &package).await.is_err());
        ad.starter_zip = Some("starter.zip".into());
        std::fs::write(&archive, b"not a zip").unwrap();
        assert!(prepare_hosted(&ad, &package).await.is_err());
        std::fs::remove_file(&archive).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(package.join("different.zip"), &archive).unwrap();
            std::fs::write(package.join("different.zip"), &bytes).unwrap();
            assert!(prepare_hosted(&ad, &package).await.is_err());
        }
        std::fs::remove_dir_all(package).unwrap();
    }

    #[tokio::test]
    async fn packs_src_with_root_dockerfile_and_excludes_package_files() {
        let package =
            std::env::temp_dir().join(format!("rsctf-patch-src-{}", uuid::Uuid::new_v4()));
        let src = package.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let ad = AdSection {
            starter_zip: Some("src".into()),
            ..Default::default()
        };
        assert!(prepare_hosted(&ad, &package).await.is_err());
        std::fs::write(src.join("Dockerfile"), b"FROM scratch\n").unwrap();
        std::fs::write(src.join("banner.txt"), b"Starter\n").unwrap();
        std::fs::write(package.join("challenge.yaml"), b"").unwrap();
        let starter = prepare_hosted(&ad, &package).await.unwrap().unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(starter.archive)).unwrap();
        assert_eq!(zip.len(), 2);
        assert!(zip.by_name("Dockerfile").is_ok());
        let mut banner = String::new();
        zip.by_name("banner.txt")
            .unwrap()
            .read_to_string(&mut banner)
            .unwrap();
        assert_eq!(banner, "Starter\n");
        std::fs::remove_dir_all(package).unwrap();
    }
}
