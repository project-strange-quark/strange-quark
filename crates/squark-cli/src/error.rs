// SPDX-License-Identifier: MIT
//! Error types for `squark-cli`.

use std::path::PathBuf;

use squark_journal::{JournalError, WriteError};
use squark_types::pack::PackFormatError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "invalid pack path {path:?}: must start with '/' and have no empty, '.', '..', '\\', or NUL components"
    )]
    InvalidPackPath { path: String },

    #[error("duplicate pack entry path {path:?}")]
    DuplicatePackPath { path: String },

    #[error("pack entry exceeds u32 limits: {reason}")]
    SizeOverflow { reason: &'static str },

    #[error("invalid pack format: {0}")]
    PackFormat(#[from] PackFormatError),

    #[error("invalid pack structure: {0}")]
    PackValidation(String),

    #[error("journal write error: {0}")]
    JournalWrite(#[from] WriteError),

    #[error("invalid journal batch {batch_index}: {source}")]
    JournalBatch {
        batch_index: usize,
        #[source]
        source: JournalError,
    },

    #[error("HTML entry file not found: {path}")]
    MissingEntryHtml { path: PathBuf },

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("command {command:?} failed: {detail}")]
    CommandFailed { command: String, detail: String },

    #[error("{0}")]
    Usage(String),
}
