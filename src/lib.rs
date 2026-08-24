//! # High-level bindings to Lua
//!
//! The `mlua` crate provides safe high-level bindings to the [Lua programming language].
//!
//! # The `Lua` object
//!
//! The main type exported by this library is the [`Lua`] struct. In addition to methods for
//! [executing] Lua chunks or [evaluating] Lua expressions, it provides methods for creating Lua
//! values and accessing the table of [globals].
//!
//! # Converting data
//!
//! The [`IntoLua`] and [`FromLua`] traits allow conversion from Rust types to Lua values and vice
//! versa. They are implemented for many data structures found in Rust's standard library.
//!
//! For more general conversions, the [`IntoLuaMulti`] and [`FromLuaMulti`] traits allow converting
//! between Rust types and *any number* of Lua values.
//!
//! Most code in `mlua` is generic over implementors of those traits, so in most places the normal
//! Rust data structures are accepted without having to write any boilerplate.
//!
//! # Custom Userdata
//!
//! The [`UserData`] trait can be implemented by user-defined types to make them available to Lua.
//! Methods and operators to be used from Lua can be added using the [`UserDataMethods`] API.
//! Fields are supported using the [`UserDataFields`] API.
//!
//! # Serde support
//!
//! The [`LuaSerdeExt`] trait implemented for [`Lua`] allows conversion from Rust types to Lua
//! values and vice versa using serde. Any user defined data type that implements
//! [`serde::Serialize`] or [`serde::Deserialize`] can be converted.
//! For convenience, additional functionality to handle `NULL` values and arrays is provided.
//!
//! The [`Value`] enum and other types implement [`serde::Serialize`] trait to support serializing
//! Lua values into Rust values.
//!
//! Requires `feature = "serde"`.
//!
//! # `Send` and `Sync` support
//!
//! By default `mlua` is `!Send`. This can be changed by enabling `feature = "send"` that adds
//! `Send` requirement to Rust functions and [`UserData`] types.
//!
//! In this case [`Lua`] object and their types can be send or used from other threads. Internally
//! access to Lua VM is synchronized using a reentrant mutex that can be locked many times within
//! the same thread.
//!
//! [Lua programming language]: https://www.lua.org/
//! [executing]: crate::Chunk::exec
//! [evaluating]: crate::Chunk::eval
//! [globals]: crate::Lua::globals
//! [`Future`]: std::future::Future
//! [`serde::Serialize`]: https://docs.serde.rs/serde/ser/trait.Serialize.html
//! [`serde::Deserialize`]: https://docs.serde.rs/serde/de/trait.Deserialize.html

// Deny warnings inside doc tests / examples. When this isn't present, rustdoc doesn't show *any*
// warnings at all.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(send), allow(clippy::arc_with_non_send_sync))]
#![allow(clippy::ptr_eq)]
#![allow(unsafe_op_in_unsafe_fn)]

#[macro_use]
mod macros;

mod buffer;
mod chunk;
#[cfg(any(feature = "luau-classes", doc))]
mod class;
mod conversion;
mod debug;
mod error;
mod function;
mod luau;
mod memory;
mod multi;
mod state;
mod stdlib;
mod string;
mod table;
mod thread;
mod traits;
mod types;
mod userdata;
mod util;
mod value;
mod vector;
pub mod auxlib;
pub mod prelude;

pub use bstr::BString;
pub use ffi::{self, lua_CFunction, lua_State};

pub use crate::chunk::{AsChunk, Chunk, ChunkMode, ChunkSource};
pub use crate::debug::{Debug, DebugEvent, DebugNames, DebugSource, DebugStack};
pub use crate::error::{Error, Result};
pub use crate::function::{Function, FunctionInfo};
pub use crate::multi::{MultiValue, Variadic};
pub use crate::state::{GCMode, Lua, WeakLua};
pub use crate::stdlib::StdLib;
pub use crate::string::{BorrowedBytes, BorrowedStr, String};
pub use crate::table::{Table, TablePairs, TablePairsOwned, TableSequence};
pub use crate::thread::{ContinuationStatus, Thread, ThreadStatus};
pub use crate::traits::{
    FromLua, FromLuaMulti, IntoLua, IntoLuaMulti, IntoCallbackResult,
    FromLuaErr
};
pub use crate::types::{
    AppDataRef, AppDataRefMut, Either, Integer, LightUserData, TypedRef, UnbackedTypedRef, Number, VmState, CallbackResult, CallbackFinalizeAction, Yield, CustomError, Ok,
    ErrorWithTraceback, DirectUserdataGetField
};
pub use crate::state::extra::USERDATA2_TAG; // embedders should not use this tag
pub use crate::auxlib::*;
pub use crate::userdata::{AnyUserData, TypedUserData, TypedUserData as UserDataRef, UntypedUserDataPtr, UserDataDirectFieldGet};
pub use crate::value::{Nil, Value};
pub use crate::memory::{DefaultAllocator, LuaAllocator};
#[cfg(feature = "bumpalo")]
pub use crate::memory::BumpAllocator;
pub use crate::{
    buffer::{Buffer, ExternalBuffer, ExternalBufferMut},
    string::ExternalString,
    chunk::{CompileConstant, Compiler},
    function::CoverageInfo,
    luau::{HeapDump, NavigateError, Require, TextRequirer},
    vector::Vector,
};

#[cfg(any(feature = "luau-classes", doc))]
#[cfg_attr(docsrs, doc(cfg(feature = "luau-classes")))]
pub use crate::class::{Class, Object};

#[cfg(feature = "serde")]
#[doc(inline)]
pub use crate::{
    serde::{de::Options as DeserializeOptions, ser::Options as SerializeOptions, LuaSerdeExt},
    value::SerializableValue,
};

#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub mod serde;

pub(crate) mod private {
    use super::*;

    pub trait Sealed {}

    impl Sealed for Error {}
    impl<T> Sealed for std::result::Result<T, Error> {}
    impl Sealed for Lua {}
    impl Sealed for Table {}
    impl Sealed for AnyUserData {}
}
