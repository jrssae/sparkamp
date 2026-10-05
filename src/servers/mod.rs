//! Navidrome / OpenSubsonic server support.
//!
//! Design: docs/superpowers/specs/2026-09-30-server-support-design.md.
//!
//! Everything here is UI-agnostic. Parsers and request builders are pure
//! functions tested offline against fixtures; the network sits behind the
//! [`transport::Transport`] trait so no test ever talks to a real server.

pub mod api;
pub mod apply;
pub mod auth;
pub mod cache;
pub mod client;
pub mod error;
pub mod indicator;
pub mod export;
#[cfg(test)]
mod fake_http;
pub mod manager;
pub mod matcher;
pub mod merge;
pub mod normalize;
pub mod playback;
pub mod progressive;
pub mod request;
#[cfg(target_os = "linux")]
pub mod secret_service;
pub mod status;
pub mod sync;
pub mod transport;
#[cfg(target_os = "macos")]
pub mod transport_apple;
pub mod uri;
pub mod validate;

#[cfg(test)]
mod multi_server_tests;
