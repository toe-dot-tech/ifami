//! # ifami-core
//!
//! The media-fetching engine. No UI, no framework, no global state.
//!
//! This crate is the part of Ifami worth reviewing on its own: it turns a URL
//! into a [`Media`] ([`resolve`]), describes a transfer ([`download::task`]),
//! and executes one resumably ([`download::engine`]). Clients -- the desktop
//! app, the CLI, a script -- are thin.
//!
//! ## What this crate will not do
//!
//! The scope boundary in `docs/SCOPE.md` is enforced here, not merely
//! documented. Two outcomes are first-class and final:
//!
//! * [`ResolveError::AuthRequired`] -- the source gates access behind a login,
//!   a membership, a paywall, a geo-block, or an anti-bot token. There is no
//!   code path in this crate that gets around it, and none will be added.
//! * [`ResolveError::DrmProtected`] -- the media is content-protected. Ifami
//!   ships no content decryption module.
//!
//! Both are returned as ordinary errors, are never retryable, and stop the
//! resolver registry immediately rather than falling through to a worse answer.
//!
//! ## Shape
//!
//! ```text
//! URL --> ResolverSet::resolve --> Media --> plan --> Task --> engine::run
//!        (net/, resolve/)                    (download/)  (download/)
//! ```
//!
//! Everything that touches the network goes through [`net::HttpClient`], which
//! is injected. That is what lets the whole engine be tested against scripted
//! byte-exact responses and a `127.0.0.1` fixture server, never the public
//! internet.
//!
//! ## The clock
//!
//! A download is clock-free: what a transfer does is a function of the bytes and
//! the responses, never of when it happened to run. The one deliberate exception
//! is [`net::speedtest`], whose entire job is to measure elapsed time. It is
//! called out here rather than left to quietly become a general-purpose clock
//! dependency across the crate.
//!
//! ## Example
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use ifami_core::download::{Manager, Store};
//! use ifami_core::net::ReqwestClient;
//!
//! let client = Arc::new(ReqwestClient::new()?);
//! let manager = Manager::load(Store::new("queue.json"), client)?;
//!
//! // Resolve, choose the best self-contained format, and queue it. The manager
//! // schedules it immediately; `wait_idle` blocks until it finishes or pauses.
//! let task = manager.add_url("https://cdn.example.invalid/clip.mp4").await?;
//! manager.wait_idle().await;
//!
//! println!("{} -> {}", task.id, task.output.display());
//! # Ok(())
//! # }
//! ```

#![doc(html_root_url = "https://docs.rs/ifami-core/0.1.0")]

pub mod download;
pub mod error;
pub mod model;
pub mod naming;
pub mod net;
pub mod resolve;

/// Version of the engine, from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The user-agent Ifami identifies itself as.
///
/// Asserted in tests rather than merely documented: claiming to be a browser
/// would be both dishonest and a policy problem on every store we would ship
/// to. See `docs/adr/0005-scope-boundary.md`.
pub const USER_AGENT: &str = concat!(
    "ifami/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/toe-dot-tech/ifami)"
);

/// Digest of `input`, hex-encoded, truncated to `bytes`.
///
/// Used for stable identifiers derived from a URL. Deterministic and pure: the
/// same URL always produces the same id, which is what lets a resumed transfer
/// find the partial file it left behind.
pub fn digest(bytes: usize, input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(&hasher.finalize()[..bytes])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_names_ifami_and_claims_no_browser() {
        assert!(USER_AGENT.starts_with("ifami/"));
        for forbidden in ["Mozilla", "Chrome", "Safari", "Firefox", "Edge"] {
            assert!(
                !USER_AGENT.contains(forbidden),
                "user agent must not impersonate {forbidden}"
            );
        }
    }

    #[test]
    fn digests_are_stable_and_the_requested_length() {
        let a = digest(8, "https://example.invalid/a.mp4");
        assert_eq!(a, digest(8, "https://example.invalid/a.mp4"));
        assert_ne!(a, digest(8, "https://example.invalid/b.mp4"));
        assert_eq!(a.len(), 16, "8 bytes hex-encodes to 16 characters");
    }
}
