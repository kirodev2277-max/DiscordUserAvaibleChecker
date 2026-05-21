//! Discord unique-username availability checker (library surface).
//!
//! This crate exposes:
//!
//! - [`checker`] — async HTTP client for Discord's unauthenticated
//!   `unique-username/username-attempt-unauthed` endpoint.
//! - [`generator`] — exhaustive and random generation of 3- and
//!   4-letter handles.
//! - [`result`] — [`CheckResult`] / [`Status`] domain types.
//! - [`persistence`] — appending plain-text and JSON-lines result logs.
//!
//! Both the CLI (`dua`) and the GUI (`dua-gui`) consume this same API,
//! so behavior is consistent across surfaces.

pub mod checker;
pub mod generator;
pub mod persistence;
pub mod result;

pub use checker::{BatchProgress, Checker, CheckerConfig};
pub use generator::{
    generate, Charset, GenMode, GenerateConfig, Length, SPACE_3_LETTERS, SPACE_4_LETTERS,
};
pub use persistence::{
    append_jsonl_log, append_text_log, load_usernames_file, parse_usernames_text,
};
pub use result::{CheckResult, Status};
