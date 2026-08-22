//! Current JSON wire types shared by the UnionC Agent and Server.
//!
//! This crate deliberately contains no collection, validation, persistence, or HTTP logic. The
//! Agent owns construction at platform boundaries and the Server owns trust-boundary validation;
//! both sides use these exact DTOs so their wire representations cannot drift independently.

#![forbid(unsafe_code)]

mod pairing;
mod report;

pub use pairing::*;
pub use report::*;
