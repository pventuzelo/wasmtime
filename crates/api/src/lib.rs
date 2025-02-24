//! Wasmtime embed API. Based on wasm-c-api.

#![allow(improper_ctypes)]

mod callable;
mod context;
mod data_structures;
mod externals;
mod instance;
mod module;
mod r#ref;
mod runtime;
mod trampoline;
mod trap;
mod types;
mod values;

pub mod wasm;

pub use crate::callable::Callable;
pub use crate::externals::*;
pub use crate::instance::Instance;
pub use crate::module::Module;
pub use crate::r#ref::{AnyRef, HostInfo, HostRef};
pub use crate::runtime::{Config, Engine, Store};
pub use crate::trap::Trap;
pub use crate::types::*;
pub use crate::values::*;
