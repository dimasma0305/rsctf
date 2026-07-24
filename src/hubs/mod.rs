//! Real-time hubs — a SignalR-protocol server so RSCTF's `@microsoft/signalr`
//! client works. Each hub exposes `POST /hub/{name}/negotiate` + the WebSocket
//! `GET /hub/{name}`, authenticates the connection, and streams `AppState`
//! event-bus messages as hub invocations (see `signalr`).

pub mod admin;
mod admission;
pub mod attack;
pub mod container;
pub mod monitor;
pub mod signalr;
pub mod user;

#[cfg(test)]
mod tests;
