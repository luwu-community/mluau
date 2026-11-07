//! Builds a Lua value straight from a serde [`Deserializer`] while it parses.
//!
//! [`LuaSerdeExt::to_value`] needs a Rust value first, so decoding text with it (`serde_json::Value`, then
//! `to_value`) builds the document twice and holds both copies at once. The visitor here pushes each value onto the
//! Lua stack as the deserializer reports it: strings are copied once from the input into Lua, and nothing else is
//! allocated outside Lua.
//!
//! The whole parse runs inside one `lua_rawpcall`, which costs a C++ `try` and nothing per value. The visitor only makes
//! raw API calls (allocations and raw sets), so no Lua call frame is entered and `nCcalls` and `ci` never change. A Lua
//! error (out of memory, a `nil` or NaN key) is a `lua_exception` that unwinds through the deserializer's Rust frames,
//! which `C-unwind` allows. A Rust panic in the deserializer is neither a `lua_exception` nor a `std::exception`, so
//! `luaD_rawrunprotected` lets it pass and the caller's `StackGuard` resets the stack. Nothing between the two may call
//! `catch_unwind`: catching a foreign exception aborts.
//!
//! [`Deserializer`]: serde::Deserializer
//! [`LuaSerdeExt::to_value`]: crate::LuaSerdeExt::to_value

use std::fmt;
use std::os::raw::c_int;
use std::result::Result as StdResult;
use std::string::String as StdString;

use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};

use super::ser::Options;
use crate::error::{Error, Result};
use crate::state::RawLua;
use crate::value::Value;

/// Pushes one deserialized value onto the stack of `state`
#[derive(Clone, Copy)]
struct Push {
    state: *mut ffi::lua_State,
    array_metatable_ref: c_int,
    options: Options,
}

impl Push {
    /// Makes room for `slots` more values. The deserializer's own nesting limit (128 for serde_json) stops runaway
    /// depth first in practice.
    fn reserve<E: de::Error>(self, slots: c_int) -> StdResult<(), E> {
        if unsafe { ffi::lua_checkstack(self.state, slots) } == 0 {
            return Err(E::custom("too deeply nested to fit on the Lua stack"));
        }
        Ok(())
    }

    fn push_null(self, to_null: bool) {
        unsafe {
            if to_null {
                ffi::lua_pushsymnone(self.state);
            } else {
                ffi::lua_pushnil(self.state);
            }
        }
    }

    /// Pushes an integer the way `IntoLua` converts one: a Lua integer when it fits, else a number
    fn push_integer<T: TryInto<ffi::lua_Integer>>(self, value: T, as_number: f64) {
        unsafe {
            match value.try_into() {
                Ok(i) => ffi::lua_pushinteger(self.state, i),
                Err(_) => ffi::lua_pushnumber(self.state, as_number),
            }
        }
    }
}

impl<'de> DeserializeSeed<'de> for Push {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> StdResult<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Push {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> StdResult<(), E> {
        self.reserve(1)?;
        unsafe { ffi::lua_pushboolean(self.state, value as c_int) };
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> StdResult<(), E> {
        self.reserve(1)?;
        self.push_integer(value, value as f64);
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> StdResult<(), E> {
        self.reserve(1)?;
        self.push_integer(value, value as f64);
        Ok(())
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> StdResult<(), E> {
        self.reserve(1)?;
        self.push_integer(value, value as f64);
        Ok(())
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> StdResult<(), E> {
        self.reserve(1)?;
        self.push_integer(value, value as f64);
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> StdResult<(), E> {
        self.reserve(1)?;
        unsafe { ffi::lua_pushnumber(self.state, value) };
        Ok(())
    }

    fn visit_str<E: de::Error>(self, value: &str) -> StdResult<(), E> {
        self.visit_bytes(value.as_bytes())
    }

    fn visit_bytes<E: de::Error>(self, value: &[u8]) -> StdResult<(), E> {
        self.reserve(1)?;
        unsafe { ffi::lua_pushlstring_(self.state, value.as_ptr() as *const _, value.len()) };
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> StdResult<(), E> {
        self.reserve(1)?;
        self.push_null(self.options.serialize_unit_to_null);
        Ok(())
    }

    fn visit_none<E: de::Error>(self) -> StdResult<(), E> {
        self.reserve(1)?;
        self.push_null(self.options.serialize_none_to_null);
        Ok(())
    }

    fn visit_some<D: de::Deserializer<'de>>(self, deserializer: D) -> StdResult<(), D::Error> {
        self.deserialize(deserializer)
    }

    fn visit_newtype_struct<D: de::Deserializer<'de>>(self, deserializer: D) -> StdResult<(), D::Error> {
        self.deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> StdResult<(), A::Error> {
        let capacity = seq.size_hint().unwrap_or(0).min(c_int::MAX as usize) as c_int;
        self.reserve(2)?;
        unsafe {
            ffi::lua_createtable(self.state, capacity, 0);
            if self.options.set_array_metatable {
                ffi::lua_getrefpool(self.state, self.array_metatable_ref);
                ffi::lua_setmetatable(self.state, -2);
            }
        }

        let mut index: c_int = 0;
        while seq.next_element_seed(self)?.is_some() {
            index += 1;
            unsafe { ffi::lua_rawseti_(self.state, -2, index) };
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> StdResult<(), A::Error> {
        let capacity = map.size_hint().unwrap_or(0).min(c_int::MAX as usize) as c_int;
        self.reserve(1)?;
        unsafe { ffi::lua_createtable(self.state, 0, capacity) };

        if self.options.detect_serde_json_arbitrary_precision {
            match map.next_key_seed(FirstKey(self))? {
                None => return Ok(()),
                Some(FirstKeyKind::Number) => {
                    let digits: StdString = map.next_value()?;
                    let number = digits.parse::<f64>().map_err(de::Error::custom)?;
                    unsafe {
                        ffi::lua_pop(self.state, 1);
                        ffi::lua_pushnumber(self.state, number);
                    }
                    return Ok(());
                }
                Some(FirstKeyKind::Pushed) => {
                    map.next_value_seed(self)?;
                    unsafe { ffi::lua_rawset(self.state, -3) };
                }
            }
        }

        while map.next_key_seed(self)?.is_some() {
            map.next_value_seed(self)?;
            unsafe { ffi::lua_rawset(self.state, -3) };
        }
        Ok(())
    }

    /// An enum variant with data becomes `{ [variant] = data }`, as `to_value` writes a newtype variant
    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> StdResult<(), A::Error> {
        self.reserve(1)?;
        unsafe { ffi::lua_createtable(self.state, 0, 1) };
        let ((), variant) = data.variant_seed(self)?;
        variant.newtype_variant_seed(self)?;
        unsafe { ffi::lua_rawset(self.state, -3) };
        Ok(())
    }
}

/// The token key of the one-key map serde_json reports a number as with `arbitrary_precision`
const SERDE_JSON_NUMBER_TOKEN: &str = "$serde_json::private::Number";

enum FirstKeyKind {
    /// The key was pushed like any other
    Pushed,
    /// The key is serde_json's number token, so the map is a number and nothing was pushed
    Number,
}

/// Reads the first key of a map when `detect_serde_json_arbitrary_precision` is set
#[derive(Clone, Copy)]
struct FirstKey(Push);

impl<'de> DeserializeSeed<'de> for FirstKey {
    type Value = FirstKeyKind;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> StdResult<FirstKeyKind, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for FirstKey {
    type Value = FirstKeyKind;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a map key")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> StdResult<FirstKeyKind, E> {
        if value == SERDE_JSON_NUMBER_TOKEN {
            return Ok(FirstKeyKind::Number);
        }
        self.0.visit_str(value).map(|()| FirstKeyKind::Pushed)
    }

    fn visit_bytes<E: de::Error>(self, value: &[u8]) -> StdResult<FirstKeyKind, E> {
        self.0.visit_bytes(value).map(|()| FirstKeyKind::Pushed)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> StdResult<FirstKeyKind, E> {
        self.0.visit_bool(value).map(|()| FirstKeyKind::Pushed)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> StdResult<FirstKeyKind, E> {
        self.0.visit_i64(value).map(|()| FirstKeyKind::Pushed)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> StdResult<FirstKeyKind, E> {
        self.0.visit_u64(value).map(|()| FirstKeyKind::Pushed)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> StdResult<FirstKeyKind, E> {
        self.0.visit_f64(value).map(|()| FirstKeyKind::Pushed)
    }

    fn visit_unit<E: de::Error>(self) -> StdResult<FirstKeyKind, E> {
        self.0.visit_unit().map(|()| FirstKeyKind::Pushed)
    }
}

/// Deserializes one value from `deserializer` and returns it. The caller holds a `StackGuard`, which removes whatever
/// a failed parse left on the stack.
pub(crate) fn push_deserialized<'de, D: de::Deserializer<'de>>(
    lua: &RawLua,
    deserializer: D,
    options: Options,
) -> Result<Value> {
    let state = lua.state();
    let push = Push {
        state,
        array_metatable_ref: unsafe { (*lua.extra()).array_metatable_ref },
        options,
    };

    // A deserializer error is a Rust value, so it's stored here and the protected call itself succeeds
    let mut parse_error: Option<StdString> = None;
    unsafe {
        protect_lua_raw!(state, 0, ffi::LUA_MULTRET, |_| {
            if let Err(err) = push.deserialize(deserializer) {
                parse_error = Some(err.to_string());
            }
        })?;
    }

    match parse_error {
        Some(message) => Err(Error::DeserializeError(message)),
        None => Ok(unsafe { lua.pop_value_at(state) }),
    }
}
