//! Exercise the real importer while its own Python child is stopped. No fake
//! preparation hook or shared process environment is needed to force contention.
use super::super::{checker, checker_gc};
use super::import_with_game_lock;
use crate::app_state::SharedState;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct PausedPreparation(OwnedFd);

impl PausedPreparation {
    fn signal(&self, signal: i32) {
        // pidfds prevent a quickly reaped child PID from addressing a different
        // process during cancellation cleanup.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal,
                std::ptr::null::<nix::libc::siginfo_t>(),
                0,
            )
        };
        assert!(
            result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::ESRCH)
        );
    }
}

impl Drop for PausedPreparation {
    fn drop(&mut self) {
        self.signal(nix::libc::SIGCONT);
    }
}

async fn pause_owned_python(root: &Path) -> (PausedPreparation, PathBuf) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            for entry in std::fs::read_dir("/proc").unwrap().flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                    continue;
                };
                let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                    continue;
                };
                let args: Vec<_> = cmdline
                    .split(|byte| *byte == 0)
                    .filter(|value| !value.is_empty())
                    .collect();
                if args.len() != 4 || args[1] != b"-m" || args[2] != b"venv" {
                    continue;
                }
                let Ok(venv) = std::str::from_utf8(args[3]) else {
                    continue;
                };
                let venv = PathBuf::from(venv);
                if !venv.starts_with(root) {
                    continue;
                }
                let fd = unsafe { nix::libc::syscall(nix::libc::SYS_pidfd_open, pid, 0) };
                if fd < 0 {
                    continue;
                }
                let child = PausedPreparation(unsafe { OwnedFd::from_raw_fd(fd as i32) });
                child.signal(nix::libc::SIGSTOP);
                return (child, venv.parent().unwrap().to_path_buf());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("import must prepare Python while the publication guard is held")
}

async fn collect(state: &SharedState, root: &Path) {
    // First pass marks unreachable artifacts, second applies the test's zero
    // grace. Production keeps the full fifteen-minute crash-recovery grace.
    for _ in 0..3 {
        checker_gc::collect_stale_checker_revisions_at(state.pg(), root, Duration::ZERO, 128)
            .await
            .unwrap();
    }
}

pub(super) async fn assert_preparation_publication_and_cancellation(
    state: &SharedState,
    root: &Path,
    binding_id: i32,
    game_id: i32,
    reference_id: i32,
) {
    for cancel in [false, true] {
        let name = format!("Publication {}", uuid::Uuid::new_v4().simple());
        let package = root
            .join("repos")
            .join(binding_id.to_string())
            .join("event")
            .join(&name);
        let source = package.join("checker");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(source.join("run.py"), b"print('prepared')\n")
            .await
            .unwrap();
        let manifest = package.join("challenge.yaml");
        tokio::fs::write(
            &manifest,
            format!("name: {name}\ntype: AttackDefense\ncategory: Pwn\n"),
        )
        .await
        .unwrap();

        let quick = checker::checker_dest_dir(root, game_id, "ready-peer");
        let prepared = checker::stage_checker_venv(&quick, &source).await.unwrap();
        let held = checker_gc::acquire_checker_artifact_guard(state.pg())
            .await
            .unwrap();
        let task_state = state.clone();
        let task_manifest = manifest.clone();
        let import = tokio::spawn(async move {
            import_with_game_lock(&task_state, game_id, &task_manifest).await
        });
        let (paused, staging) = pause_owned_python(&root.join("checkers")).await;
        assert!(staging.join("src/run.py").is_file());
        assert!(!import.is_finished());
        assert_eq!(
            state.pg().size() as usize - state.pg().num_idle(),
            1,
            "only the test's publication guard may occupy a connection during preparation"
        );
        held.release().await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            let mut guard = checker_gc::acquire_checker_artifact_guard(state.pg())
                .await
                .unwrap();
            prepared.publish().await.unwrap();
            sqlx::query(r#"UPDATE "GameChallenges" SET ad_checker_image = $2 WHERE id = $1"#)
                .bind(reference_id)
                .bind(&quick)
                .execute(&mut **guard.transaction_mut())
                .await
                .unwrap();
            guard.release().await.unwrap();
        })
        .await
        .expect("a stopped preparation must not block unrelated publication");
        collect(state, root).await;
        assert!(
            staging.exists(),
            "preparing artifact retains its execution lease"
        );
        assert!(
            Path::new(&quick).exists(),
            "committed peer remains reachable"
        );

        if cancel {
            import.abort();
            assert!(import.await.unwrap_err().is_cancelled());
            drop(paused);
            collect(state, root).await;
            assert!(
                !staging.exists(),
                "GC recovers abandoned private preparation"
            );
        } else {
            drop(paused);
            let raced = tokio::time::timeout(Duration::from_secs(35), import)
                .await
                .unwrap()
                .unwrap();
            // Publishing another challenge in this event advances its definition
            // revision. The slow importer must reject its stale snapshot and
            // then succeed when retried against the new committed revision.
            assert!(matches!(
                raced,
                Err(crate::utils::error::AppError::Conflict(_))
            ));
            let imported = import_with_game_lock(state, game_id, &manifest)
                .await
                .unwrap();
            let revision: String = sqlx::query_scalar(
                r#"SELECT ad_checker_image FROM "GameChallenges" WHERE id = $1"#,
            )
            .bind(imported.challenge_id)
            .fetch_one(state.pg())
            .await
            .unwrap();
            let execution =
                checker_gc::acquire_checker_execution_lease(Path::new(&revision)).unwrap();
            let output =
                tokio::process::Command::new(Path::new(&revision).join("venv/bin/python3"))
                    .arg(Path::new(&revision).join("src/run.py"))
                    .kill_on_drop(true)
                    .output()
                    .await
                    .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"prepared\n");
            sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id = $1"#)
                .bind(imported.challenge_id)
                .execute(state.pg())
                .await
                .unwrap();
            collect(state, root).await;
            assert!(
                Path::new(&revision).exists(),
                "executing revision survives loss of its DB owner"
            );
            drop(execution);
            collect(state, root).await;
            assert!(!Path::new(&revision).exists());
        }
        sqlx::query(r#"UPDATE "GameChallenges" SET ad_checker_image = NULL WHERE id = $1"#)
            .bind(reference_id)
            .execute(state.pg())
            .await
            .unwrap();
        drop(prepared);
        collect(state, root).await;
        assert!(!Path::new(&quick).exists());
        tokio::fs::remove_dir_all(package).await.unwrap();
    }
}
