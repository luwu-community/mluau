use std::os::raw::c_int;

use crate::error::{Error, Result};
use crate::state::{Lua, RawLua};
use crate::traits::{FromLua, IntoLua};
use crate::util::{check_stack, StackGuard};
use crate::value::Value;

/// A Luwu `collections.option`, converted to and from Rust's [`Option`].
///
/// `Some(v)` becomes `option.some(v)` and `None` becomes the `none` primitive (the empty option). A plain
/// [`Option`] keeps converting `None` to `nil`.
///
/// Converting `LuwuOption(Some(v))` fails when `v` converts to `nil` or `none`, as `option.some(nil)` does in Luwu.
/// It also fails when the state's `collections` library isn't open.
#[cfg_attr(docsrs, doc(cfg(all(feature = "luwu-classes", feature = "none-primitive"))))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct LuwuOption<T>(pub Option<T>);

/// A Luwu `collections.result`, converted to and from Rust's [`Result`](std::result::Result).
///
/// `Ok(v)` becomes `result.ok(v)` and `Err(e)` becomes `result.err(e)`. A plain `Result` returned from a Rust function
/// keeps raising its error instead.
///
/// Converting it fails when the state's `collections` library isn't open.
#[cfg_attr(docsrs, doc(cfg(all(feature = "luwu-classes", feature = "none-primitive"))))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LuwuResult<T, E>(pub std::result::Result<T, E>);

impl<T> From<Option<T>> for LuwuOption<T> {
    #[inline]
    fn from(option: Option<T>) -> Self {
        LuwuOption(option)
    }
}

impl<T> From<LuwuOption<T>> for Option<T> {
    #[inline]
    fn from(option: LuwuOption<T>) -> Self {
        option.0
    }
}

impl<T, E> From<std::result::Result<T, E>> for LuwuResult<T, E> {
    #[inline]
    fn from(result: std::result::Result<T, E>) -> Self {
        LuwuResult(result)
    }
}

impl<T, E> From<LuwuResult<T, E>> for std::result::Result<T, E> {
    #[inline]
    fn from(result: LuwuResult<T, E>) -> Self {
        result.0
    }
}

type PushPayload = unsafe extern "C-unwind" fn(*mut ffi::lua_State, c_int);

// Pushes `value`, then replaces it with the `some`, `ok` or `err` that `push` builds around it
unsafe fn push_payload(
    value: impl IntoLua,
    push: PushPayload,
    lua: &RawLua,
    state: *mut ffi::lua_State,
) -> Result<()> {
    check_stack(state, 2)?;
    value.push_into_specified_stack(lua, state)?;
    protect_lua_raw!(state, 1, 1, |state| push(state, -1))
}

// Converts a value by pushing it and reading it back off the stack
fn from_pushed_value<R>(value: Value, lua: &Lua) -> Result<R>
where
    R: FromLua,
{
    let lua = lua.lock();
    let state = lua.state();
    unsafe {
        let _sg = StackGuard::new(state);
        check_stack(state, 1)?;
        lua.push_value_at(&value, state);
        R::from_specified_stack(-1, &lua, state)
    }
}

// Reads the value the `some`, `ok` or `err` at `idx` holds as a `V`
unsafe fn read_payload<V: FromLua>(idx: c_int, lua: &RawLua, state: *mut ffi::lua_State) -> Result<V> {
    check_stack(state, 1)?;
    let idx = ffi::lua_absindex(state, idx);
    let _sg = StackGuard::new(state);
    ffi::lua_getpayload(state, idx);
    V::from_specified_stack(-1, lua, state)
}

unsafe fn conversion_error(to: &str, idx: c_int, lua: &RawLua, state: *mut ffi::lua_State) -> Error {
    Error::FromLuaConversionError {
        from: lua.stack_value_at(idx, None, state).type_name(),
        to: to.to_string(),
        message: None,
    }
}

impl<T: IntoLua> IntoLua for LuwuOption<T> {
    fn into_lua(self, lua: &Lua) -> Result<Value> {
        let lua = lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            self.push_into_specified_stack(&lua, state)?;
            Ok(lua.pop_value_at(state))
        }
    }

    unsafe fn push_into_specified_stack(self, lua: &RawLua, state: *mut ffi::lua_State) -> Result<()> {
        match self.0 {
            Some(value) => push_payload(value, ffi::lua_pushsome, lua, state),
            None => {
                check_stack(state, 1)?;
                ffi::lua_pushsymnone(state);
                Ok(())
            }
        }
    }
}

impl<T: IntoLua, E: IntoLua> IntoLua for LuwuResult<T, E> {
    fn into_lua(self, lua: &Lua) -> Result<Value> {
        let lua = lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            self.push_into_specified_stack(&lua, state)?;
            Ok(lua.pop_value_at(state))
        }
    }

    unsafe fn push_into_specified_stack(self, lua: &RawLua, state: *mut ffi::lua_State) -> Result<()> {
        match self.0 {
            Ok(value) => push_payload(value, ffi::lua_pushresultok, lua, state),
            Err(value) => push_payload(value, ffi::lua_pushresulterr, lua, state),
        }
    }
}

impl<T: FromLua> FromLua for LuwuOption<T> {
    fn from_lua(value: Value, lua: &Lua) -> Result<Self> {
        from_pushed_value(value, lua)
    }

    unsafe fn from_specified_stack(idx: c_int, lua: &RawLua, state: *mut ffi::lua_State) -> Result<Self> {
        if ffi::lua_type(state, idx) == ffi::LUA_TSYMNONE {
            return Ok(LuwuOption(None));
        }
        if ffi::lua_issome(state, idx) == 0 {
            return Err(conversion_error("option", idx, lua, state));
        }
        Ok(LuwuOption(Some(read_payload(idx, lua, state)?)))
    }
}

impl<T: FromLua, E: FromLua> FromLua for LuwuResult<T, E> {
    fn from_lua(value: Value, lua: &Lua) -> Result<Self> {
        from_pushed_value(value, lua)
    }

    unsafe fn from_specified_stack(idx: c_int, lua: &RawLua, state: *mut ffi::lua_State) -> Result<Self> {
        if ffi::lua_isresultok(state, idx) != 0 {
            return Ok(LuwuResult(Ok(read_payload(idx, lua, state)?)));
        }
        if ffi::lua_isresulterr(state, idx) != 0 {
            return Ok(LuwuResult(Err(read_payload(idx, lua, state)?)));
        }
        Err(conversion_error("result", idx, lua, state))
    }
}
