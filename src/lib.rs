//! Leafmask — stateless CLI for logical MongoDB dumping, deterministic
//! anonymization, referentially-intact subsetting, and restoration.
//!
//! The crate is organized so that the transformation engine, configuration,
//! and storage abstraction are pure and unit-testable without any external
//! service. Access to MongoDB and to cloud storage backends sits behind traits
//! and optional cargo features (`mongo`, `s3`, `azure`, `ssh`).

pub mod error;

pub mod bench;
pub mod catalog;
pub mod cli;
pub mod config;
pub mod dump;
pub mod hash;
pub mod mongo;
pub mod restore;
pub mod storage;
pub mod subset;
pub mod toolkit;
pub mod transform;
pub mod validate;

pub use error::{Error, Result};
