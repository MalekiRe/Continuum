//! Experimental bytecode harness scaffold.
//!
//! This module is deliberately not wired into the legacy harness.  It grows
//! from the bytecode/value core outward, while `tests/snowflake_size.rs` keeps
//! the compiler and non-compiler runtime within their separate LOC budgets.
#![allow(dead_code)]

pub mod compile;
pub mod effects;
pub mod image;
pub mod runtime;
pub mod value;
pub mod vm;
pub mod world;

/// Images are readable only by this exact experimental VM ABI.
pub const ABI: u64 = 1;
