use std::time::Duration;

use bollard::exec::ResizeExecOptions;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use super::{ByocExec, LiveExec};

const RESIZE_TIMEOUT: Duration = Duration::from_secs(1);

/// Per-terminal state kept by one SignalR connection.
pub(super) struct Session {
    input_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// A watch channel retains only the latest dimensions, so resize floods use
    /// fixed memory and at most one timeout-bounded Docker request per session.
    resize_tx: Option<watch::Sender<Option<(u16, u16)>>>,
    resize_task: Option<JoinHandle<()>>,
    pump: Option<JoinHandle<()>>,
    input_task: Option<JoinHandle<()>>,
    #[allow(dead_code)]
    byoc_guard: Option<crate::services::byoc_tunnel::ExecGuard>,
    #[allow(dead_code)]
    active_permit: OwnedSemaphorePermit,
}

impl Session {
    pub(super) fn docker(
        exec: LiveExec,
        sid: String,
        out_tx: mpsc::Sender<String>,
        active_permit: OwnedSemaphorePermit,
    ) -> Self {
        let (resize_tx, mut resize_rx) = watch::channel(None);
        let docker = exec.docker.clone();
        let exec_id = exec.exec_id.clone();
        let resize_task = tokio::spawn(async move {
            while resize_rx.changed().await.is_ok() {
                let dimensions = *resize_rx.borrow_and_update();
                let Some((width, height)) = dimensions else {
                    continue;
                };
                let _ = timeout(
                    RESIZE_TIMEOUT,
                    docker.resize_exec(&exec_id, ResizeExecOptions { width, height }),
                )
                .await;
            }
        });
        Self {
            input_tx: Some(exec.input_tx),
            resize_tx: Some(resize_tx),
            resize_task: Some(resize_task),
            pump: Some(super::spawn_pump(exec.output, sid, out_tx)),
            input_task: Some(exec.input_task),
            byoc_guard: None,
            active_permit,
        }
    }

    pub(super) fn byoc(
        exec: ByocExec,
        sid: String,
        out_tx: mpsc::Sender<String>,
        active_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            input_tx: Some(exec.input_tx),
            resize_tx: None,
            resize_task: None,
            pump: Some(super::spawn_pump_reader(exec.read, sid, out_tx)),
            input_task: Some(exec.input_task),
            byoc_guard: Some(exec.guard),
            active_permit,
        }
    }

    pub(super) fn idle(active_permit: OwnedSemaphorePermit) -> Self {
        Self {
            input_tx: None,
            resize_tx: None,
            resize_task: None,
            pump: None,
            input_task: None,
            byoc_guard: None,
            active_permit,
        }
    }

    pub(super) fn input_rejected(&self, bytes: Vec<u8>) -> bool {
        self.input_tx
            .as_ref()
            .is_some_and(|tx| tx.try_send(bytes).is_err())
    }

    pub(super) fn resize(&self, cols: u16, rows: u16) {
        if let Some(resize_tx) = &self.resize_tx {
            resize_tx.send_replace(Some((cols, rows)));
        }
    }

    /// Cancel every background task; dropping senders also closes stdin.
    pub(super) fn shutdown(self) {
        for task in [self.pump, self.input_task, self.resize_task]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}
