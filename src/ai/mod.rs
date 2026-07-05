//! BYOK (bring-your-own-key) AI assistant.
//!
//! The assistant runs a tool-calling agent loop on a background thread
//! (`session`) against the immutable `JsonIndex` (`tools`), talking to the
//! user's chosen LLM provider over HTTP (`provider`). Edits proposed by the
//! model are never applied directly — they surface in the UI as a reviewable
//! changeset that flows through the existing `edit_overlay`/undo machinery.
//! API keys live in the macOS Keychain (`keystore`), not in settings.

pub mod keystore;
pub mod markdown;
pub mod panel;
pub mod provider;
pub mod session;
pub mod tools;

pub use tools::{EditAction, ProposedEdit};
