//! PostgreSQL source of truth for trusted outbound worker agents.
//!
//! This module deliberately contains no sockets or runtime-specific code. It
//! owns enrollment identity, session fencing, exact resource reservations and
//! workload desired/observed transitions so every network-owner replica can
//! recover state after a restart.

mod maintenance;
mod nodes;
mod status;
mod types;
mod workloads;

pub use types::*;

use sqlx::PgPool;

/// Cheap cloneable handle around the SeaORM-owned PostgreSQL pool.
#[derive(Clone)]
pub struct WorkerStore {
    pool: PgPool,
}

impl WorkerStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn database_error(error: sqlx::Error) -> WorkerStoreError {
    WorkerStoreError::Database(error)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.code().as_deref() == Some("23505"))
}

#[cfg(test)]
mod tests;
