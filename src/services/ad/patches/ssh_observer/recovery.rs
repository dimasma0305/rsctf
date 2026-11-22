use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

// Exec uses the same image user as the SSH shell. Archive inspection can read
// as the daemon, so never use its payload to populate a downloadable blob.
const READ: &str = r#"
export PATH=/usr/bin:/bin
p=$1
[ ! -L "$p" ] && [ -f "$p" ] && [ -r "$p" ] || exit 1
for flag in /flag "${RSCTF_FLAG_FILE:-/flag}"; do
    [ ! "$p" -ef "$flag" ] || exit 1
done
timeout 2 head -c "$2" "$p" | base64
printf '\nRSCTF-END\n'
"#;

pub(super) struct Recovery {
    files: Files,
    blobs: BTreeMap<String, Vec<u8>>,
}

/// Repeated polls of an unchanged shell do not need another workload exec.
/// Only skip after a complete capture of the most recent observation.
pub(super) async fn already_retained(
    db: &mut sqlx::PgConnection,
    service: i32,
    tree: &str,
    observed: &Files,
) -> AppResult<bool> {
    let row = sqlx::query_scalar::<_, sqlx::types::Json<Files>>(r#"SELECT h.files::jsonb FROM
        (SELECT revision,files FROM "AdHostedSshSourceHistory" WHERE service_id=$1 ORDER BY revision DESC LIMIT 1) h
        JOIN "AdSshRecovery" r ON r.service_id=$1 AND r.revision=h.revision AND r.starter_tree=$2"#)
        .bind(service).bind(tree).fetch_optional(db).await.map_err(db_error)?;
    Ok(row.is_some_and(|saved| &saved.0 == observed))
}

pub(super) async fn collect(
    session: &Session,
    paths: &[SshSource],
    observed: &Files,
) -> Option<Recovery> {
    let mut recovery = Recovery {
        files: Files::new(),
        blobs: BTreeMap::new(),
    };
    for source in paths.iter().filter(|source| source.retain_content) {
        let blob = observed.get(&source.source_path)?.clone();
        if let Some(file) = &blob {
            let output = session
                .st
                .containers
                .exec(
                    &session.backend,
                    vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        READ.into(),
                        "rsctf-source".into(),
                        source.container_path.clone(),
                        (forensics::MAX_FILE_PREVIEW_BYTES + 1).to_string(),
                    ],
                )
                .await
                .ok()?;
            let encoded = output.trim_end().strip_suffix("RSCTF-END")?;
            let encoded: Vec<_> = encoded
                .bytes()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            if encoded.len() > forensics::MAX_FILE_PREVIEW_BYTES.div_ceil(3) * 4 {
                return None;
            }
            let data = STANDARD.decode(encoded).ok()?;
            if data.len() > forensics::MAX_FILE_PREVIEW_BYTES
                || hex::encode(git_object("blob", &data)) != file.blob
            {
                return None;
            }
            recovery.blobs.insert(file.blob.clone(), data);
        }
        recovery.files.insert(source.source_path.clone(), blob);
    }
    Some(recovery)
}

impl Recovery {
    pub(super) async fn save(
        self,
        db: &mut sqlx::PgConnection,
        service: i32,
        revision: i64,
        tree: &str,
    ) -> AppResult<()> {
        let created = sqlx::query(r#"INSERT INTO "AdSshRecovery"(service_id,revision,id,starter_tree) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING"#)
            .bind(service).bind(revision).bind(uuid::Uuid::new_v4()).bind(tree).execute(&mut *db).await.map_err(db_error)?;
        if created.rows_affected() == 0 {
            return Ok(());
        }
        for (oid, data) in self.blobs {
            sqlx::query(r#"INSERT INTO "AdSshRecoveryBlobs"(service_id,oid,payload) VALUES($1,$2,$3) ON CONFLICT DO NOTHING"#)
                .bind(service).bind(oid).bind(data).execute(&mut *db).await.map_err(db_error)?;
        }
        for (path, entry) in self.files {
            let (oid, executable) =
                entry.map_or((None, false), |file| (Some(file.blob), file.executable));
            sqlx::query(r#"INSERT INTO "AdSshRecoveryFiles"(service_id,revision,path,oid,executable) VALUES($1,$2,$3,$4,$5)"#)
                .bind(service).bind(revision).bind(path).bind(oid).bind(executable).execute(&mut *db).await.map_err(db_error)?;
        }
        sqlx::query(r#"DELETE FROM "AdSshRecovery" WHERE service_id=$1 AND revision IN
            (SELECT revision FROM "AdSshRecovery" WHERE service_id=$1 ORDER BY revision DESC OFFSET 8)"#)
            .bind(service).execute(&mut *db).await.map_err(db_error)?;
        Ok(())
    }
}
