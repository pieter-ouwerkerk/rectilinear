uniffi::setup_scaffolding!();

pub mod config;
pub mod db;
pub mod embedding;
pub mod linear;
pub mod search;

mod ffi;
pub use ffi::*;
