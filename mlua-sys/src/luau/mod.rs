//! Low level bindings to Luau.

pub use compat::*;
pub use lauxlib::*;
pub use lua::*;
#[cfg(feature = "luwu-analysis")]
pub use luaanalysis::*;
pub use luacode::*;
pub use luacodegen::*;
pub use lualib::*;
pub use luarequire::*;

pub mod compat;
pub mod lauxlib;
pub mod lua;
#[cfg(feature = "luwu-analysis")]
pub mod luaanalysis;
pub mod luacode;
pub mod luacodegen;
pub mod lualib;
pub mod luarequire;

/// Short hash of the Luwu integration commit that `luwu/` was vendored from, as `update_luwu.luau` records it in
/// `luwu/LUWU_COMMIT`
pub const LUWU_COMMIT: &str = include_str!("../../../luwu/LUWU_COMMIT").trim_ascii();
