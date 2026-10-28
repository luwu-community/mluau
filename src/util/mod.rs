use std::borrow::Cow;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::{slice, str};

use crate::error::{Error, Result};
pub(crate) use error::{
    error_traceback, func_call_error_traceback, 
    func_call_error, protect_lua_closure, protect_lua_raw_closure, call_trampoline, pop_error,
    get_error
};
pub(crate) use short_names::short_type_name;
pub(crate) use userdata::{push_fat_cclosure, push_fat_data_cfunction};

// Checks that Lua has enough free stack space for future stack operations. On failure, this will
// panic with an internal error message.
#[inline]
pub(crate) unsafe fn assert_stack(state: *mut ffi::lua_State, amount: c_int) {
    // TODO: This should only be triggered when there is a logic error in `mlua`. In the future,
    // when there is a way to be confident about stack safety and test it, this could be enabled
    // only when `cfg!(debug_assertions)` is true.
    mlua_assert!(ffi::lua_checkstack(state, amount) != 0, "out of stack space");
}

// Checks that Lua has enough free stack space and returns `Error::StackError` on failure.
#[inline]
pub(crate) unsafe fn check_stack(state: *mut ffi::lua_State, amount: c_int) -> Result<()> {
    if ffi::lua_checkstack(state, amount) == 0 {
        Err(Error::StackError)
    } else {
        Ok(())
    }
}

pub(crate) struct StackGuard {
    state: *mut ffi::lua_State,
    top: c_int,
}

impl StackGuard {
    // Creates a StackGuard instance with record of the stack size, and on Drop will check the
    // stack size and drop any extra elements. If the stack size at the end is *smaller* than at
    // the beginning, this is considered a fatal logic error and will result in a panic.
    #[inline]
    pub(crate) unsafe fn new(state: *mut ffi::lua_State) -> StackGuard {
        StackGuard {
            state,
            top: ffi::lua_gettop(state),
        }
    }

    // Same as `new()`, but allows specifying the expected stack size at the end of the scope.
    #[inline]
    pub(crate) fn with_top(state: *mut ffi::lua_State, top: c_int) -> StackGuard {
        StackGuard { state, top }
    }
}

impl Drop for StackGuard {
    #[track_caller]
    fn drop(&mut self) {
        unsafe {
            let top = ffi::lua_gettop(self.state);
            if top < self.top {
                mlua_panic!("{} too many stack values popped", self.top - top)
            }
            if top > self.top {
                ffi::lua_settop(self.state, self.top);
            }
        }
    }
}

// Uses 3 (or 1 if unprotected) stack spaces, does not call checkstack.
#[inline(always)]
pub(crate) unsafe fn push_string(state: *mut ffi::lua_State, s: &[u8]) -> Result<()> {
    protect_lua_raw!(state, 0, 1, |state| {
        ffi::lua_pushlstring(state, s.as_ptr() as *const c_char, s.len());
    })
}

// Uses 3 stack spaces (when protect), does not call checkstack.

#[inline(always)]
pub(crate) unsafe fn push_buffer(state: *mut ffi::lua_State, size: usize) -> Result<*mut u8> {
    let data = protect_lua_raw!(state, 0, 1, |state| ffi::lua_newbuffer(state, size))?;
    Ok(data as *mut u8)
}

#[inline(always)]
pub(crate) unsafe fn push_external_buffer(
    state: *mut ffi::lua_State,
    size: usize,
    data: *mut u8,
    userdata: *mut std::ffi::c_void,
    free_cb: Option<ffi::lua_BufferFree>,
    mode: std::os::raw::c_int,
) -> Result<*mut u8> {
    let buf_data = protect_lua_raw!(state, 0, 1, |state| ffi::lua_newexternalbuffer(
        state,
        size,
        data as *mut std::ffi::c_void,
        userdata,
        free_cb,
        mode
    ))?;
    Ok(buf_data as *mut u8)
}

#[inline(always)]
pub(crate) unsafe fn push_external_string(
    state: *mut ffi::lua_State,
    s: *const c_char,
    len: usize,
    userdata: *mut std::ffi::c_void,
    free_cb: Option<ffi::lua_StringFree>,
) -> Result<()> {
    protect_lua_raw!(state, 0, 1, |state| ffi::lua_pushexternalstring(
        state, s, len, userdata, free_cb
    ))
}

// Uses 3 stack spaces, does not call checkstack.
#[inline]
pub(crate) unsafe fn push_table(state: *mut ffi::lua_State, narr: usize, nrec: usize) -> Result<()> {
    let narr: c_int = narr.try_into().unwrap_or(c_int::MAX);
    let nrec: c_int = nrec.try_into().unwrap_or(c_int::MAX);
    protect_lua_raw!(state, 0, 1, |state| ffi::lua_createtable(state, narr, nrec))
}

// Returns Lua main thread for Lua >= 5.2 or checks that the passed thread is main for Lua 5.1.
pub(crate) unsafe fn get_main_state(state: *mut ffi::lua_State) -> Option<*mut ffi::lua_State> {
    Some(ffi::lua_mainthread(state))
}

// Converts the given lua value to a string in a reasonable format without causing a Lua error or
// panicking.
pub(crate) unsafe fn to_string(state: *mut ffi::lua_State, index: c_int) -> String {
    match ffi::lua_type(state, index) {
        ffi::LUA_TNONE => "<none>".to_string(),
        ffi::LUA_TNIL => "<nil>".to_string(),
        ffi::LUA_TBOOLEAN => (ffi::lua_toboolean(state, index) != 1).to_string(),
        ffi::LUA_TLIGHTUSERDATA => {
            format!("<lightuserdata {:?}>", ffi::lua_topointer(state, index))
        }
        ffi::LUA_TNUMBER => {
            let mut isint = 0;
            let i = ffi::lua_tointegerx(state, -1, &mut isint);
            if isint == 0 {
                ffi::lua_tonumber(state, index).to_string()
            } else {
                i.to_string()
            }
        }

        ffi::LUA_TVECTOR => {
            let v = ffi::lua_tovector(state, index);
            mlua_debug_assert!(!v.is_null(), "vector is null");
            let (x, y, z) = (*v, *v.add(1), *v.add(2));
            #[cfg(not(feature = "luau-vector4"))]
            return format!("vector({x}, {y}, {z})");
            #[cfg(feature = "luau-vector4")]
            return format!("vector({x}, {y}, {z}, {w})", w = *v.add(3));
        }
        ffi::LUA_TSTRING => {
            let mut size = 0;
            // This will not trigger a 'm' error, because the reference is guaranteed to be of
            // string type
            let data = ffi::lua_tolstring(state, index, &mut size);
            String::from_utf8_lossy(slice::from_raw_parts(data as *const u8, size)).into_owned()
        }
        ffi::LUA_TTABLE => format!("<table {:?}>", ffi::lua_topointer(state, index)),
        ffi::LUA_TFUNCTION => format!("<function {:?}>", ffi::lua_topointer(state, index)),
        ffi::LUA_TUSERDATA => format!("<userdata {:?}>", ffi::lua_topointer(state, index)),
        ffi::LUA_TTHREAD => format!("<thread {:?}>", ffi::lua_topointer(state, index)),

        ffi::LUA_TBUFFER => format!("<buffer {:?}>", ffi::lua_topointer(state, index)),
        type_id => {
            let type_name = CStr::from_ptr(ffi::lua_typename(state, type_id)).to_string_lossy();
            format!("<{type_name} {:?}>", ffi::lua_topointer(state, index))
        }
    }
}

#[inline(always)]
pub(crate) unsafe fn get_metatable_ptr(state: *mut ffi::lua_State, index: c_int) -> *const c_void {
    return ffi::lua_getmetatablepointer(state, index);

}

pub(crate) unsafe fn ptr_to_str<'a>(input: *const c_char) -> Option<&'a str> {
    if input.is_null() {
        return None;
    }
    str::from_utf8(CStr::from_ptr(input).to_bytes()).ok()
}

pub(crate) unsafe fn ptr_to_lossy_str<'a>(input: *const c_char) -> Option<Cow<'a, str>> {
    if input.is_null() {
        return None;
    }
    Some(String::from_utf8_lossy(CStr::from_ptr(input).to_bytes()))
}

pub(crate) fn linenumber_to_usize(n: c_int) -> Option<usize> {
    match n {
        n if n < 0 => None,
        n => Some(n as usize),
    }
}

mod error;
mod short_names;
mod userdata;

#[inline]
pub(crate) fn lua_type_to_str(type_id: c_int) -> &'static str {
    match type_id {
        ffi::LUA_TNIL => "nil",
        ffi::LUA_TBOOLEAN => "boolean",
        ffi::LUA_TLIGHTUSERDATA => "lightuserdata",
        ffi::LUA_TNUMBER => "number",
        ffi::LUA_TSTRING => "string",
        ffi::LUA_TTABLE => "table",
        ffi::LUA_TFUNCTION => "function",
        ffi::LUA_TUSERDATA => "userdata",
        ffi::LUA_TTHREAD => "thread",
        ffi::LUA_TBUFFER => "buffer",
        ffi::LUA_TVECTOR => "vector",
        _ => "<unknown>",
    }
}
