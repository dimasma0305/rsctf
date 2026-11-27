//! Prepare a starter ZIP from the immutable organizer repository snapshot.

use std::io::Read;
use std::path::Path;

use crate::services::ad::patches::starter::Registration;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};
use crate::utils::upload::reserve_buffered;

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
    if filename.len() > 255
        || !filename.ends_with(".zip")
        || filename.len() == 4
        || filename
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"._-".contains(&byte))
    {
        return Err(AppError::bad_request(
            "ad.starterZip must be the name of a .zip file beside challenge.yaml",
        ));
    }
    let path = package.join(filename);
    tokio::task::spawn_blocking(move || read_starter(&path))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .map(Some)
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
    let permit = reserve_buffered(16 * 1024 * 1024 + 3 * size as usize)?;
    let mut archive = Vec::with_capacity(size as usize);
    file.take(size + 1)
        .read_to_end(&mut archive)
        .map_err(|_| invalid_file())?;
    if archive.len() as u64 != size {
        return Err(AppError::bad_request(
            "ad.starterZip changed while it was being read",
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
}
