//! The only place in moji where `unsafe` is allowed to appear.
//!
//! Every Apple framework call lives under this module and is exposed to the rest of the crate as a
//! safe type. Text Input Sources is documented as main-thread-only, so everything here runs on the
//! main thread's run loop.

pub mod harness;
pub mod signals;
pub mod tap;
pub mod timer;
pub mod tis;
