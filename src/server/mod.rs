//! The artefacto server: one loopback daemon per repository.
//!
//! # Lock order
//!
//! `Shared` holds a mutation gate, the log, the folded state, and the page
//! registry. The order is **`commit` → `log` → `core` → `sockets`**, and no
//! blocking operation runs while `core` or `sockets` is held.
//!
//! `std::sync::Mutex` is not reentrant. A function holding a guard must never
//! call another that takes the same lock, so lock-taking functions come in
//! pairs: a public one that locks, and an inner `_locked` one that takes the
//! guard.

pub mod daemon;
pub mod delivery;
pub mod event;
pub mod feedback;
pub mod fold;
pub mod http;
pub mod ingress;
pub mod lease;
pub mod log;
pub mod page;
pub mod poll;
pub mod presence;
pub mod push;
pub mod review;
pub mod socket;
pub mod state_dir;
pub mod verbs;
