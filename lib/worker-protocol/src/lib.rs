//! Shared, runtime-independent wire contract for RSCTF trusted workers.
//!
//! Protocol revisioning protects wire compatibility only. It has no relation to
//! Attack/Defense or King-of-the-Hill scoring, both of which remain constant.

mod control;
mod data;
mod enrollment;
mod framing;
mod workload;

pub use control::*;
pub use data::*;
pub use enrollment::*;
pub use framing::*;
pub use workload::*;

/// Second revision of the trusted worker wire protocol. Revision 2 carries a
/// per-service writable-layer limit so the control plane cannot silently lose
/// an author-selected storage boundary at the worker handoff.
pub const PROTOCOL_REVISION: u16 = 2;

/// ALPN selected by the long-lived control connection.
pub const CONTROL_ALPN: &[u8] = b"rsctf-worker-control/1";

/// ALPN selected by the data connection carrying multiplexed streams.
pub const DATA_ALPN: &[u8] = b"rsctf-worker-data/1";
