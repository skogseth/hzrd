/*!
This crate provides a safe API for shared mutability using hazard pointers for memory reclamation.
*/

mod core;
mod linked_list;
mod utils;

pub mod cell;
pub mod pair;

pub use crate::cell::HzrdCell;
pub use crate::core::RefHandle;
