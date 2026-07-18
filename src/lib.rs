//! rsctf — a from-scratch Rust rewrite of RSCTF, mirroring the original
//! project's folder structure for a one-to-one port.
//!
//! This library crate exposes the modules so they can be exercised by
//! integration tests; the runnable entry point is the `rsctf` binary
//! (`src/main.rs`, ported from RSCTF `Program.cs`).

pub mod app_state;
pub mod controllers;
pub mod extensions;
pub mod hubs;
pub mod middlewares;
pub mod migrations;
pub mod models;
pub mod server;
pub mod services;
pub mod storage;
pub mod utils;
