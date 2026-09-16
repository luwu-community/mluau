use std::os::raw::c_void;

use crate::error::Result;
use crate::traits::{FromLua, IntoLua};
use crate::types::ValueRef;
use crate::util::{check_stack, StackGuard};

/// A Luau class, i.e. the nominal type produced by a Luau `class ... end` declaration.
///
/// This represents the class itself (the "blueprint"), not an instance of it -- see [`Object`]
/// for that. Luau's user-defined classes are meant to be used from Luau as a way of getting
/// nominal types with structural checks handled by the Luau type solver, so there's very little
/// reason to interact with a [`Class`] from Rust beyond passing it through: it's treated as an
/// opaque reference, the same way [`Thread`](crate::Thread) or [`Function`](crate::Function) are.
///
/// This is part of Luau's (experimental) user-defined classes support, gated behind the
/// `luwu-classes` feature.
#[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
#[derive(Clone, Debug, PartialEq)]
pub struct Class(pub(crate) ValueRef);

impl Class {
    /// Gets a static member of this class by name.
    ///
    /// Unlike [`Table::get`](crate::Table::get), indexing a class is not nil-safe: Luau throws
    /// a hard error for a missing member (or for indexing an instance-only member on the class
    /// itself, e.g. `Box.item` instead of `someBox.item`), rather than returning `nil`. This is
    /// caught and returned as `Err`, same as any other Lua error raised through a protected call.
    pub fn get<V: FromLua>(&self, key: impl IntoLua) -> Result<V> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 4)?;

            lua.push_ref_at(&self.0, state);
            key.push_into_specified_stack(&lua, state)?;
            protect_lua!(state, 2, 1, fn(state) ffi::lua_gettable(state, -2))?;

            V::from_specified_stack(-1, &lua, state)
        }
    }

    /// Converts this class to a generic C pointer.
    ///
    /// There is no way to convert the pointer back to its original value.
    ///
    /// Typically this function is used only for hashing and debug information.
    #[inline]
    pub fn to_pointer(&self) -> *const c_void {
        self.0.to_pointer()
    }
}

#[cfg(feature = "luwu-classes")]
impl crate::types::LuaType for Class {
    const TYPE_ID: std::os::raw::c_int = ffi::LUA_TCLASS;
}

/// An instance of a Luau [`Class`].
///
/// Like [`Class`], this is treated as an opaque reference from Rust: classes exist primarily so
/// Luau code gets nominal types with structural checks from the type solver, not so that Rust
/// code inspects or constructs instances directly.
///
/// This is part of Luau's (experimental) user-defined classes support, gated behind the
/// `luwu-classes` feature.
#[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
#[derive(Clone, Debug, PartialEq)]
pub struct Object(pub(crate) ValueRef);

impl Object {
    /// Gets an instance member of this object by name.
    ///
    /// Unlike [`Table::get`](crate::Table::get), indexing an object is not nil-safe: Luau throws
    /// a hard error for a missing member rather than returning `nil`. This is caught and
    /// returned as `Err`, same as any other Lua error raised through a protected call.
    pub fn get<V: FromLua>(&self, key: impl IntoLua) -> Result<V> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 4)?;

            lua.push_ref_at(&self.0, state);
            key.push_into_specified_stack(&lua, state)?;
            protect_lua!(state, 2, 1, fn(state) ffi::lua_gettable(state, -2))?;

            V::from_specified_stack(-1, &lua, state)
        }
    }

    /// Sets an instance member of this object by name.
    ///
    /// As with [`get`](Object::get), Luau throws a hard error (rather than silently creating a
    /// new field, as tables do) if the member isn't declared on the object's class. This is
    /// caught and returned as `Err`.
    ///
    /// Note: Luau has no private/const member attributes yet, so this cannot currently fail due
    /// to visibility -- and even once those exist, this bypasses Luau's own access checks
    /// (which are enforced at the script/compiler level, not in the C API), so it will still be
    /// able to read or write members a Luau script itself couldn't reach.
    pub fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 5)?;

            lua.push_ref_at(&self.0, state);
            key.push_into_specified_stack(&lua, state)?;
            value.push_into_specified_stack(&lua, state)?;
            protect_lua!(state, 3, 0, fn(state) ffi::lua_settable(state, -3))
        }
    }

    /// Converts this object to a generic C pointer.
    ///
    /// There is no way to convert the pointer back to its original value.
    ///
    /// Typically this function is used only for hashing and debug information.
    #[inline]
    pub fn to_pointer(&self) -> *const c_void {
        self.0.to_pointer()
    }
}

#[cfg(feature = "luwu-classes")]
impl crate::types::LuaType for Object {
    const TYPE_ID: std::os::raw::c_int = ffi::LUA_TOBJECT;
}

#[cfg(test)]
mod assertions {
    use super::*;

    static_assertions::assert_not_impl_any!(Class: Send);
    static_assertions::assert_not_impl_any!(Object: Send);
}
