//! TIFF IFD walking for structural inventories (`zencodec::inventory`).
//!
//! One walker shared by zentiff (TIFF) and zenraw (DNG and other TIFF-based RAW
//! formats), so TIFF structure is parsed and placed in one place. Each caller supplies
//! its decoder's policy (which pointers it follows, which tags it reads, how it
//! sizes values) through [`walk::Rules`]; the walker owns the mechanics: bounded
//! IFD traversal, value and extent placement, overlap resolution and work budgets.
//!
//! The API follows what those two crates need; it is not a general TIFF parser.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod tags;
pub mod walk;
