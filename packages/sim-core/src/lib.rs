//! Deterministic simulation core shared by server and client code.
//!
//! `sim` is short for `simulation`. This crate owns the minimal deterministic
//! model boundary for lockstep movement and combat rules. Stage 1 intentionally
//! exposes only the module skeleton and schema version; concrete fixed-point
//! math, state, input, tick, and hash behavior will be added in later stages.

#![forbid(unsafe_code)]

pub mod hash;
pub mod ids;
pub mod input;
pub mod math;
pub mod snapshot;
pub mod state;
pub mod tick;

pub const SIM_CORE_SCHEMA_VERSION: u16 = 1;
