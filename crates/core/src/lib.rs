//! SnapRSS core: storage, import and feed logic.
//!
//! This crate is deliberately free of any UI dependency. Tauri commands live in
//! the app crate and call into here.

pub mod cleanup;
pub mod db;
pub mod filters;
pub mod import;
pub mod models;

pub use db::{Db, DbError};
