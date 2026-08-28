use axum::response::Response;

use super::{check_async, too_many_requests, Policy};

/// Charge proxy-open churn before target resolution. Capability-authenticated
/// WSRX requests pass their verified account subject here too; they never share
/// an anonymous pool.
pub(crate) async fn admit_proxy_open(
    subject: uuid::Uuid,
    source: &str,
    workload: uuid::Uuid,
    participation_id: Option<i32>,
) -> Option<Response> {
    let subject_check = check_async(Policy::ProxyOpen, format!("subject:{subject}"));
    let workload_check = check_async(Policy::ProxyOpen, format!("workload:{workload}"));
    let source_check = check_async(Policy::ProxySourceOpen, format!("source:{source}"));
    let participation_check = async {
        match participation_id {
            Some(participation_id) => {
                check_async(
                    Policy::ProxyOpen,
                    format!("participation:{participation_id}"),
                )
                .await
            }
            None => Ok(()),
        }
    };
    let (subject, workload, source, participation) = tokio::join!(
        subject_check,
        workload_check,
        source_check,
        participation_check
    );
    [subject, workload, source, participation]
        .into_iter()
        .filter_map(Result::err)
        .max()
        .map(too_many_requests)
}

pub(crate) async fn admit_proxy_participation(participation_id: i32) -> Option<Response> {
    check_async(
        Policy::ProxyOpen,
        format!("participation:{participation_id}"),
    )
    .await
    .err()
    .map(too_many_requests)
}
