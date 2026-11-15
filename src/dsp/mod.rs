//! The vocal chain's building blocks. Nothing here knows about nih-plug: every processor is
//! plain Rust, so it is unit-tested directly and the chain can be rendered offline.

pub mod crossover;
pub mod dynamics;
pub mod filters;
pub mod limiter;
pub mod loudness;
pub mod master_fx;
pub mod maximizer;
pub mod saturation;
pub mod space;
pub mod tune;
