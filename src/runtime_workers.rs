//! Composition-root ownership of workers in the supported runtime topologies.
//!
//! Keep registration and role eligibility together: declaring a role healthy
//! must not leave a durable queue without a consumer in that topology.

use tokio::{sync::watch, task::JoinHandle};

use crate::services;
use crate::{app_state::SharedState, models::internal::configs::RuntimeRole};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableWorker {
    ChallengeImport,
    RevisionEffects,
    MutationRetention,
    RepositoryPush,
    RepositoryScan,
    MailDelivery,
    ControlJobs,
}

const CONTROL_WORKERS: &[DurableWorker] = &[
    DurableWorker::ChallengeImport,
    DurableWorker::RevisionEffects,
    DurableWorker::MutationRetention,
    DurableWorker::RepositoryPush,
    DurableWorker::RepositoryScan,
    DurableWorker::MailDelivery,
    DurableWorker::ControlJobs,
];

pub const fn for_role(role: RuntimeRole) -> &'static [DurableWorker] {
    match role {
        RuntimeRole::All
        | RuntimeRole::Control
        | RuntimeRole::Engine
        | RuntimeRole::Development => CONTROL_WORKERS,
        RuntimeRole::Web | RuntimeRole::Network | RuntimeRole::Migrate => &[],
    }
}

impl DurableWorker {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ChallengeImport => "challenge import worker",
            Self::RevisionEffects => "challenge revision effect worker",
            Self::MutationRetention => "mutation retention worker",
            Self::RepositoryPush => "repository push worker",
            Self::RepositoryScan => "repository binding scheduler",
            Self::MailDelivery => "mail outbox reconciler",
            Self::ControlJobs => "control-job supervisor",
        }
    }

    fn start(self, state: SharedState, shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
        match self {
            Self::ChallengeImport => {
                crate::controllers::edit::start_import_job_worker(state, shutdown)
            }
            Self::RevisionEffects => {
                crate::controllers::edit::start_challenge_revision_effect_worker(state, shutdown)
            }
            Self::MutationRetention => services::mutation_retention::start(state, shutdown),
            Self::RepositoryPush => {
                crate::controllers::edit::start_repo_push_worker(state, shutdown)
            }
            Self::RepositoryScan => services::repo_binding_scheduler::start(state, shutdown),
            Self::MailDelivery => services::mail_outbox::start_reconciler(state, shutdown),
            Self::ControlJobs => services::control_jobs::start(state, shutdown),
        }
    }
}

pub fn start(
    state: &SharedState,
    shutdown: watch::Receiver<bool>,
) -> Vec<(&'static str, JoinHandle<()>)> {
    for_role(state.config.runtime_role)
        .iter()
        .map(|worker| (worker.name(), worker.start(state.clone(), shutdown.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_serving_topology_has_all_durable_consumers() {
        for topology in [
            vec![RuntimeRole::All],
            vec![RuntimeRole::Web, RuntimeRole::Control],
            vec![RuntimeRole::Web, RuntimeRole::Engine, RuntimeRole::Network],
            vec![RuntimeRole::Development],
        ] {
            for worker in CONTROL_WORKERS {
                assert_eq!(
                    topology
                        .iter()
                        .filter(|role| for_role(**role).contains(worker))
                        .count(),
                    1,
                    "{worker:?} has no unique consumer role in {topology:?}"
                );
            }
        }
        for role in [RuntimeRole::Web, RuntimeRole::Network, RuntimeRole::Migrate] {
            assert!(for_role(role).is_empty());
        }
    }
}
