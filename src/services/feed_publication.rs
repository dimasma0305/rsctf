//! Bounded post-commit handoff for best-effort live-feed publication.
//!
//! Successful writes are authoritative in PostgreSQL and reconnect through the
//! feed cursors. Request handlers therefore only `try_send` committed row ids;
//! one per-process worker owns the optional projection queries and event-bus
//! publication. A stalled database cannot retain unbounded work or delay the
//! completed request.

use std::sync::Mutex;

use tokio::sync::{mpsc, watch};

use crate::app_state::SharedState;

const PUBLICATION_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, PartialEq, Eq)]
struct SubmissionPublication {
    game_id: i32,
    submission_id: i32,
    game_event_ids: Vec<i32>,
}

/// State-owned, bounded handoff from submission requests to one publisher.
pub(crate) struct PublicationQueue {
    sender: mpsc::Sender<SubmissionPublication>,
    receiver: Mutex<Option<mpsc::Receiver<SubmissionPublication>>>,
}

impl PublicationQueue {
    pub(crate) fn new() -> Self {
        Self::with_capacity(PUBLICATION_QUEUE_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
        }
    }

    fn enqueue(&self, publication: SubmissionPublication) -> bool {
        self.sender.try_send(publication).is_ok()
    }

    fn take_receiver(&self) -> Option<mpsc::Receiver<SubmissionPublication>> {
        self.receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

/// Hand committed ids to the best-effort publisher without awaiting SQL pool
/// admission or projection. A full/closed queue is safe: cursor backfill is the
/// correctness path for reconnecting clients.
pub(crate) fn enqueue_submission(
    state: &SharedState,
    game_id: i32,
    submission_id: i32,
    game_event_ids: Vec<i32>,
) {
    if !state.feed_publication.enqueue(SubmissionPublication {
        game_id,
        submission_id,
        game_event_ids,
    }) {
        tracing::debug!(
            game = game_id,
            submission = submission_id,
            "post-commit feed publication handoff was saturated or closed"
        );
    }
}

async fn publish_one(state: &SharedState, publication: SubmissionPublication) {
    // Both projections are individually pool-admission and time bounded. Run
    // them together so one publication occupies the single worker for at most
    // one projection budget while using no more than two idle pool connections.
    let submission = crate::services::submission_feed::publish_committed(
        state,
        publication.game_id,
        publication.submission_id,
    );
    let game_events =
        crate::services::game_event_feed::publish_committed(state, &publication.game_event_ids);
    let (submission, game_events) = tokio::join!(submission, game_events);
    if let Err(error) = submission {
        tracing::warn!(
            game = publication.game_id,
            submission = publication.submission_id,
            %error,
            "submission feed could not be published"
        );
    }
    if let Err(error) = game_events {
        tracing::warn!(
            game = publication.game_id,
            submission = publication.submission_id,
            %error,
            "submission game events could not be published"
        );
    }
}

async fn run_publisher(
    state: SharedState,
    mut receiver: mpsc::Receiver<SubmissionPublication>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            publication = receiver.recv() => {
                let Some(publication) = publication else {
                    break;
                };
                publish_one(&state, publication).await;
            }
        }
    }
}

/// Start the single optional live-feed publisher owned by an API replica.
pub fn start_publisher(
    state: &SharedState,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let receiver = state.feed_publication.take_receiver();
    let state = state.clone();
    tokio::spawn(async move {
        let Some(receiver) = receiver else {
            tracing::warn!("post-commit feed publisher was started more than once");
            return;
        };
        run_publisher(state, receiver, shutdown).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publication(submission_id: i32) -> SubmissionPublication {
        SubmissionPublication {
            game_id: 7,
            submission_id,
            game_event_ids: vec![submission_id + 100],
        }
    }

    #[test]
    fn saturated_handoff_is_nonblocking_and_strictly_bounded() {
        let queue = PublicationQueue::with_capacity(1);
        assert_eq!(queue.sender.max_capacity(), 1);
        assert!(queue.enqueue(publication(1)));
        assert_eq!(queue.sender.capacity(), 0);

        // `enqueue` is synchronous and uses `try_send`: a missing/stalled
        // publisher cannot turn a completed submission into queued request work.
        assert!(!queue.enqueue(publication(2)));
        assert_eq!(queue.sender.capacity(), 0);

        let mut receiver = queue.take_receiver().unwrap();
        assert_eq!(receiver.try_recv().unwrap(), publication(1));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn publisher_receiver_has_one_owner() {
        let queue = PublicationQueue::with_capacity(1);
        assert!(queue.take_receiver().is_some());
        assert!(queue.take_receiver().is_none());
    }
}
