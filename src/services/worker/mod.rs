//! Trusted outbound worker-plane transport.
//!
//! This module owns only live mTLS connections, bounded control queues, and
//! named workload data streams. Durable scheduling and workload state remain
//! behind [`WorkerAuthority`], allowing the singleton network owner to serve
//! connections without coupling the transport to controllers or migrations.

mod authority;
mod config;
mod container_backend;
mod control_admission;
mod data;
mod error;
mod hybrid_backend;
mod listener;
mod postgres;
mod reconciler;
mod registry;

pub use authority::{AuthenticatedPeer, PeerCertificates, WorkerAuthority};
pub use config::{bind_from_env, BoundWorkerPlane};
pub use container_backend::{parse_worker_handle, WorkerContainerManager, WorkerHandle};
pub use data::{DataConfig, WorkerDataStream};
pub use error::{WorkerError, WorkerResult};
pub use hybrid_backend::HybridWorkerContainerManager;
pub use listener::{build_mtls_server_config, WorkerServerConfig, WorkerService};
pub use postgres::PostgresWorkerAuthority;
pub use reconciler::start_reconciler;
pub use registry::{RegistryConfig, SessionContext, WorkerRegistry};
