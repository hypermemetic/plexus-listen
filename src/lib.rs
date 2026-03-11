//! plexus-listen — Audio capture Substrate plugin
//!
//! Provides audio capture, WAV recording, real-time metering, and live audio
//! streaming from USB microphones, exposed as Plexus RPC methods.

pub mod capture;
pub mod device;
pub mod hub;
pub mod meter;
pub mod recorder;
#[cfg(all(target_os = "macos", feature = "app-capture"))]
pub mod sck;
pub mod types;

// Re-export serde_helpers from plexus_core (required by plexus_macros generated code)
pub use plexus_core::serde_helpers;

// Re-exports for convenience
pub use hub::MicHub;
pub use types::MicEvent;
