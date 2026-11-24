use super::*;
use crate::services::worker::registry::{RegistryConfig, SessionContext, SessionRegistration};
use rsctf_worker_protocol::{
    AckDisposition, CommandAck, CommandError, CommandErrorCode, CommandResult, ControlEnvelope,
    FlagTarget, ObservedWorkloadState, WorkloadFence, WorkloadStatus,
};

fn command() -> WriteFlag {
    WriteFlag {
        command_id: Uuid::new_v4(),
        fence: WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 2,
        },
        flag_sequence: 15,
        target: FlagTarget {
            service: "web".into(),
            path: "/run/flag".into(),
        },
        value: "test fixture".into(),
        timeout_ms: 10_000,
    }
}

async fn connect(
    registry: &WorkerRegistry,
    worker: Uuid,
    epoch: u64,
    verified: bool,
) -> SessionRegistration {
    registry
        .register_authenticated_control(
            SessionContext {
                worker_id: worker,
                boot_id: Uuid::new_v4(),
                certificate_fingerprint_sha256: [1; 32],
                fence: SessionFence {
                    session_id: Uuid::new_v4(),
                    session_epoch: epoch,
                },
            },
            1,
            verified,
        )
        .await
        .unwrap()
}

fn response(request: &ControlEnvelope, body: ControlMessage) -> ControlEnvelope {
    ControlEnvelope {
        reply_to: Some(request.message_id),
        ..ControlEnvelope::new(request.session_epoch, body)
    }
}

#[tokio::test]
async fn accepted_and_ready_wait_for_correlated_replica_readback() {
    let registry = WorkerRegistry::new(RegistryConfig::default());
    let worker = Uuid::new_v4();
    let mut connection = connect(&registry, worker, 1, true).await;
    let flag = command();
    let delivery = registry.send(worker, ControlMessage::WriteFlag(flag.clone()));
    tokio::pin!(delivery);
    let request = tokio::select! {
        result = &mut delivery => panic!("early delivery: {result:?}"),
        request = connection.outbound.recv() => request.unwrap(),
    };
    let accepted = ControlMessage::CommandAck(CommandAck {
        command_id: flag.command_id,
        disposition: AckDisposition::Accepted,
        detail: None,
    });
    registry
        .handle_command_feedback(&connection.context, &response(&request, accepted))
        .await
        .unwrap();
    registry
        .observe_workload_status(
            &connection.context,
            &WorkloadStatus {
                fence: flag.fence,
                spec_hash: String::new(),
                state: ObservedWorkloadState::Ready,
                replicas: vec![],
                detail: None,
            },
        )
        .await
        .unwrap();
    let success = ControlMessage::CommandResult(CommandResult {
        command_id: flag.command_id,
        success: true,
        error: None,
    });
    let mut wrong = response(&request, success.clone());
    wrong.reply_to = Some(Uuid::new_v4());
    assert!(registry
        .handle_command_feedback(&connection.context, &wrong)
        .await
        .is_err());
    assert!(futures::poll!(&mut delivery).is_pending());
    assert!(matches!(
        registry.send(worker, ControlMessage::WriteFlag(flag)).await,
        Err(WorkerError::Busy)
    ));
    assert!(registry
        .handle_command_feedback(&connection.context, &response(&request, success))
        .await
        .unwrap()
        .is_none());
    assert_eq!(delivery.await.unwrap(), request.message_id);
}

#[tokio::test]
async fn partial_or_contradictory_results_and_refusal_do_not_fail_the_workload() {
    let registry = WorkerRegistry::new(RegistryConfig::default());
    let worker = Uuid::new_v4();
    let mut connection = connect(&registry, worker, 1, true).await;
    let flag = command();
    let partial = CommandError {
        code: CommandErrorCode::PartialFailure,
        message: "worker-provided detail".into(),
        failed_replicas: vec!["web:1".into()],
    };
    for reply in [
        ControlMessage::CommandResult(CommandResult {
            command_id: flag.command_id,
            success: false,
            error: Some(partial.clone()),
        }),
        ControlMessage::CommandResult(CommandResult {
            command_id: flag.command_id,
            success: true,
            error: Some(partial),
        }),
        ControlMessage::CommandAck(CommandAck {
            command_id: flag.command_id,
            disposition: AckDisposition::Busy,
            detail: None,
        }),
    ] {
        let delivery =
            registry.write_flag_confirmed(worker, &connection.context.fence, flag.clone());
        let peer = async {
            let request = connection.outbound.recv().await.unwrap();
            assert!(registry
                .handle_command_feedback(&connection.context, &response(&request, reply))
                .await
                .unwrap()
                .is_none());
        };
        let (result, ()) = tokio::join!(delivery, peer);
        assert!(matches!(result, Err(WorkerError::FlagUnconfirmed)));
    }
}

#[tokio::test]
async fn expired_flag_is_not_queued_after_waiting_for_admission() {
    let registry = WorkerRegistry::new(RegistryConfig::default());
    let worker = Uuid::new_v4();
    let mut connection = connect(&registry, worker, 1, true).await;
    let session = registry
        .current(worker, &connection.context.fence)
        .await
        .unwrap();
    let held = session.commands.lock().await;
    let mut flag = command();
    flag.timeout_ms = 5;
    let delivery = registry.write_flag_confirmed(worker, &connection.context.fence, flag);
    tokio::pin!(delivery);
    assert!(futures::poll!(&mut delivery).is_pending());
    tokio::time::sleep(Duration::from_millis(10)).await;
    drop(held);
    assert!(matches!(delivery.await, Err(WorkerError::FlagUnconfirmed)));
    assert!(connection.outbound.try_recv().is_err());
}

#[tokio::test]
async fn missing_readback_times_out_and_reconnect_needs_its_own_capability() {
    let registry = WorkerRegistry::new(RegistryConfig::default());
    let worker = Uuid::new_v4();
    let mut first = connect(&registry, worker, 1, true).await;
    let mut flag = command();
    flag.timeout_ms = 5;
    assert!(matches!(
        registry
            .write_flag_confirmed(worker, &first.context.fence, flag.clone())
            .await,
        Err(WorkerError::FlagUnconfirmed)
    ));
    first.outbound.recv().await.unwrap();
    flag.timeout_ms = 10_000;
    let retry = registry.write_flag_confirmed(worker, &first.context.fence, flag.clone());
    let reconnect = async {
        first.outbound.recv().await.unwrap();
        connect(&registry, worker, 2, false).await
    };
    let (result, mut second) = tokio::join!(retry, reconnect);
    assert!(matches!(result, Err(WorkerError::StaleSession)));
    assert!(matches!(
        registry
            .write_flag_confirmed(worker, &first.context.fence, flag.clone())
            .await,
        Err(WorkerError::StaleSession)
    ));
    assert!(matches!(
        registry.send(worker, ControlMessage::WriteFlag(flag)).await,
        Err(WorkerError::Protocol(_))
    ));
    assert!(second.outbound.try_recv().is_err());
}
