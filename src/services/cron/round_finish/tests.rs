use super::{
    cap_flag_publication_deadlines, deliver_initial_round_flags, delivery_order_key,
    managed_flag_command, run_delivery_attempts, DeliveryAttempt, DeliveryAttemptSummary,
    DeliveryAttemptTracker,
};

fn failed_summary(
    participant_attempts: usize,
    saw_platform_failure: bool,
) -> DeliveryAttemptSummary {
    DeliveryAttemptSummary {
        delivered: false,
        participant_attempts,
        saw_platform_failure,
    }
}

#[tokio::test]
async fn publication_deadline_never_starts_a_shortened_attempt() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let concurrency = tokio::sync::Semaphore::new(1);
    let result = run_delivery_attempts(
        policy,
        tokio::time::Instant::now() + std::time::Duration::from_millis(10),
        &concurrency,
        |_| async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            DeliveryAttempt::ParticipantFailure
        },
    )
    .await;
    assert_eq!(result, failed_summary(0, false));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn backend_failures_do_not_become_participant_attempts() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let concurrency = tokio::sync::Semaphore::new(1);
    let result = run_delivery_attempts(
        policy,
        tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        &concurrency,
        |_| async { DeliveryAttempt::PlatformFailure },
    )
    .await;
    assert_eq!(result, failed_summary(0, true));
}

#[tokio::test]
async fn participant_failures_remain_offline_attempts() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let concurrency = tokio::sync::Semaphore::new(1);
    let result = run_delivery_attempts(
        policy,
        tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        &concurrency,
        |_| async { DeliveryAttempt::ParticipantFailure },
    )
    .await;
    assert_eq!(result, failed_summary(3, false));
}

#[tokio::test]
async fn admitted_command_timeout_remains_a_participant_attempt() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let concurrency = tokio::sync::Semaphore::new(1);
    let result = run_delivery_attempts(
        policy,
        tokio::time::Instant::now() + std::time::Duration::from_millis(2_100),
        &concurrency,
        |admission| async move {
            admission.mark_admitted();
            std::future::pending().await
        },
    )
    .await;
    assert_eq!(result, failed_summary(1, false));
}

#[tokio::test]
async fn pre_admission_backend_stall_is_platform_attributed() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let concurrency = tokio::sync::Semaphore::new(1);
    let result = run_delivery_attempts(
        policy,
        tokio::time::Instant::now() + std::time::Duration::from_millis(2_100),
        &concurrency,
        |_| std::future::pending(),
    )
    .await;
    assert_eq!(result, failed_summary(0, true));
}

#[tokio::test]
async fn participant_failure_survives_later_platform_failures() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let concurrency = tokio::sync::Semaphore::new(1);
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    let result = run_delivery_attempts(
        policy,
        tokio::time::Instant::now() + std::time::Duration::from_secs(10),
        &concurrency,
        |_| async {
            if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                DeliveryAttempt::ParticipantFailure
            } else {
                DeliveryAttempt::PlatformFailure
            }
        },
    )
    .await;
    assert_eq!(result, failed_summary(1, true));
}

#[test]
fn tracker_removes_completed_platform_work_but_keeps_admitted_or_participant_work() {
    let tracker = DeliveryAttemptTracker::default();
    let pre_admission = tracker.begin(1, Default::default());
    assert_eq!(tracker.service_ids(), vec![1]);
    drop(pre_admission);
    assert!(tracker.service_ids().is_empty());

    let admission = crate::services::container::ContainerExecAdmission::default();
    let post_admission = tracker.begin(2, admission.clone());
    admission.mark_admitted();
    drop(post_admission);
    assert_eq!(tracker.service_ids(), vec![2]);

    let later_platform = tracker.begin(2, Default::default());
    later_platform.platform();
    assert_eq!(tracker.service_ids(), vec![2]);
}

#[test]
fn publication_cap_preserves_checker_runway_after_transition_delay() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let start = tokio::time::Instant::now();
    let transition_complete = start + std::time::Duration::from_secs(2);
    let pipeline_deadline = start + std::time::Duration::from_secs(71);
    let (publication, delivery) = cap_flag_publication_deadlines(
        policy,
        transition_complete + policy.publication_reserve(),
        transition_complete + policy.worst_case_attempt_window(),
        pipeline_deadline,
        60,
    );
    assert_eq!(publication, start + policy.publication_reserve());
    assert_eq!(delivery, publication);
    assert_eq!(
        pipeline_deadline.duration_since(publication),
        std::time::Duration::from_secs(
            60 + crate::services::ad_engine::CHECKER_MINIMUM_RUNWAY_SECONDS,
        )
    );
}

#[test]
fn saturated_publication_order_is_not_persistently_service_id_ordered() {
    let original: Vec<i32> = (1..=500).collect();
    let mut first = original.clone();
    let mut second = original.clone();
    first.sort_unstable_by_key(|id| delivery_order_key(11, *id));
    second.sort_unstable_by_key(|id| delivery_order_key(12, *id));
    assert_ne!(first, original);
    assert_ne!(first, second);
    first.sort_unstable();
    assert_eq!(first, original, "fair ordering must retain every service");
}

#[tokio::test]
async fn retry_releases_capacity_for_a_waiting_services_first_attempt() {
    let policy = crate::services::ad_engine::FlagDeliveryPolicy::default();
    let concurrency = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let first_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let first = tokio::spawn({
        let concurrency = concurrency.clone();
        let events = events.clone();
        let first_started = first_started.clone();
        async move {
            let mut attempt = 0;
            run_delivery_attempts(policy, deadline, &concurrency, |_| {
                attempt += 1;
                events.lock().unwrap().push(format!("a{attempt}"));
                first_started.notify_one();
                async { DeliveryAttempt::ParticipantFailure }
            })
            .await
        }
    });
    first_started.notified().await;
    let second = tokio::spawn({
        let concurrency = concurrency.clone();
        let events = events.clone();
        async move {
            let mut attempt = 0;
            run_delivery_attempts(policy, deadline, &concurrency, |_| {
                attempt += 1;
                events.lock().unwrap().push(format!("b{attempt}"));
                async { DeliveryAttempt::ParticipantFailure }
            })
            .await
        }
    });
    assert_eq!(first.await.unwrap(), failed_summary(3, false));
    assert_eq!(second.await.unwrap(), failed_summary(3, false));
    let events = events.lock().unwrap();
    let b1 = events.iter().position(|event| event == "b1").unwrap();
    let a2 = events.iter().position(|event| event == "a2").unwrap();
    assert!(
        b1 < a2,
        "waiting first attempt must run before retry: {events:?}"
    );
}

#[test]
fn managed_flag_is_an_argument_not_shell_source() {
    let flag = "flag{$(touch /tmp/owned);'\\\"}";
    let command = managed_flag_command(flag);
    assert_eq!(command[4], flag);
    assert!(!command[2].contains(flag));
    assert!(command[2].contains("$1"));
}

#[tokio::test]
async fn settled_publication_performs_zero_network_deliveries() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let flag = crate::services::ad_engine::AdvancedRoundFlag {
        team_service_id: 1,
        participation_id: 2,
        challenge_id: 3,
        managed: true,
        container_id: Some("old-container".to_string()),
        flag: "flag{replay}".to_string(),
    };
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    deliver_initial_round_flags(
        vec![flag],
        true,
        {
            let calls = calls.clone();
            move |flag| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    crate::services::ad_engine::FlagDeliveryOutcome::succeeded(
                        flag.team_service_id,
                        crate::services::ad_engine::FlagDeliveryKind::Managed,
                        flag.container_id,
                        1,
                    )
                }
            }
        },
        sender,
    )
    .await
    .unwrap();
    assert!(receiver.recv().await.is_none());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fast_delivery_is_streamed_before_a_slow_peer_finishes() {
    let flag = |team_service_id| crate::services::ad_engine::AdvancedRoundFlag {
        team_service_id,
        participation_id: team_service_id,
        challenge_id: 3,
        managed: true,
        container_id: Some(format!("container-{team_service_id}")),
        flag: format!("flag{{{team_service_id}}}"),
    };
    let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
    let producer = tokio::spawn(deliver_initial_round_flags(
        vec![flag(1), flag(2)],
        false,
        |flag| async move {
            if flag.team_service_id == 2 {
                std::future::pending::<()>().await;
            }
            crate::services::ad_engine::FlagDeliveryOutcome::succeeded(
                flag.team_service_id,
                crate::services::ad_engine::FlagDeliveryKind::Managed,
                flag.container_id,
                1,
            )
        },
        sender,
    ));
    let first = tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv())
        .await
        .expect("fast receipt must not wait for the slow peer")
        .expect("producer must emit the fast receipt");
    assert_eq!(first.team_service_id, 1);
    assert!(!producer.is_finished());
    producer.abort();
}
