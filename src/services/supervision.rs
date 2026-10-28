//! Own child tasks through completion, panic, and parent cancellation.
use tokio::{sync::watch, task::JoinSet};

pub(crate) async fn join_required_workers(
    mut tasks: JoinSet<()>,
    shutdown: watch::Receiver<bool>,
    name: &'static str,
) {
    while let Some(result) = tasks.join_next().await {
        result.unwrap_or_else(|error| panic!("{name} worker failed: {error}"));
        assert!(*shutdown.borrow(), "{name} worker exited before shutdown");
    }
    // JoinSet aborts every remaining child if this supervisor unwinds or is
    // cancelled by the process drain deadline. No child handle is detached.
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stopped(tokio::sync::oneshot::Sender<()>);
    impl Drop for Stopped {
        fn drop(&mut self) {
            let (placeholder, _) = tokio::sync::oneshot::channel();
            let _ = std::mem::replace(&mut self.0, placeholder).send(());
        }
    }

    async fn sleeping_worker(tasks: &mut JoinSet<()>) -> tokio::sync::oneshot::Receiver<()> {
        let (ready, started) = tokio::sync::oneshot::channel();
        let (stopped, finished) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let _guard = Stopped(stopped);
            let _ = ready.send(());
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        finished
    }

    #[tokio::test]
    async fn parent_cancellation_drops_owned_work_instead_of_detaching_it() {
        let mut tasks = JoinSet::new();
        let finished = sleeping_worker(&mut tasks).await;
        let (_send, shutdown) = watch::channel(false);
        let parent = tokio::spawn(join_required_workers(tasks, shutdown, "test"));
        parent.abort();
        assert!(matches!(parent.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(std::time::Duration::from_secs(1), finished)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn panic_propagates_and_stops_siblings() {
        let mut tasks = JoinSet::new();
        let finished = sleeping_worker(&mut tasks).await;
        tasks.spawn(async { panic!("injected worker failure") });
        let (_send, shutdown) = watch::channel(false);
        let parent = tokio::spawn(join_required_workers(tasks, shutdown, "test"));
        assert!(matches!(parent.await, Err(error) if error.is_panic()));
        tokio::time::timeout(std::time::Duration::from_secs(1), finished)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn clean_exit_requires_the_shutdown_signal() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async {});
        let (_send, shutdown) = watch::channel(true);
        join_required_workers(tasks, shutdown, "test").await;
        let mut tasks = JoinSet::new();
        tasks.spawn(async {});
        let (_send, shutdown) = watch::channel(false);
        assert!(
            matches!(tokio::spawn(join_required_workers(tasks, shutdown, "test")).await,
            Err(error) if error.is_panic())
        );
    }
}
