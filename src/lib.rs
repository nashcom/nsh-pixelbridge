//! PixelBridge library crate. main.rs is a thin shell around it, which lets
//! the integration tests in tests/ drive the real router without a socket.

pub mod api;
pub mod config;
pub mod convert;
pub mod docs;
pub mod errors;
