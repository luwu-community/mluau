use std::mem::MaybeUninit;
use std::os::raw::{c_int, c_void};
use std::ptr;

use crate::error::{Error, Result};
use crate::memory::MemoryState;
use crate::util::{check_stack, to_string};

/// Pops an error off of the stack and returns it.
/// 
/// Uses 2 stack spaces
#[inline]
pub(crate) unsafe fn pop_error(state: *mut ffi::lua_State, err_code: c_int) -> Error {
    mlua_debug_assert!(
        err_code != ffi::LUA_OK && err_code != ffi::LUA_YIELD,
        "pop_error called with non-error return code"
    );

    let err_string = to_string(state, -1);

    let err = get_error(err_string, err_code);
    ffi::lua_pop(state, 1);
    err
}

/// Returns the error given err_string and err_code
#[inline]
pub(crate) fn get_error(err_string: String, err_code: c_int) -> Error {
    match err_code {
        ffi::LUA_ERRRUN => Error::RuntimeError(err_string),
        ffi::LUA_ERRSYNTAX => {
            Error::SyntaxError {
                // This seems terrible, but as far as I can tell, this is exactly what the
                // stock Lua REPL does.
                incomplete_input: err_string.ends_with("<eof>") || err_string.ends_with("'<eof>'"),
                message: err_string,
            }
        }
        ffi::LUA_ERRERR => {
            // This error is raised when the error handler raises an error too many times
            // recursively, and continuing to trigger the error handler would cause a stack
            // overflow. It is not very useful to differentiate between this and "ordinary"
            // runtime errors, so we handle them the same way.
            Error::RuntimeError(err_string)
        }
        ffi::LUA_ERRMEM => Error::MemoryError(err_string),
        _ => mlua_panic!("unrecognized lua error code"),
    }
}

struct ErasedParams {
    invoke: unsafe fn(*mut ffi::lua_State, *mut c_void) -> c_int,
    data: *mut c_void,
}

// Trampoile between pcall and the c func to invoke
pub(crate) unsafe extern "C-unwind" fn call_trampoline(state: *mut ffi::lua_State) -> c_int {
    let params = ffi::lua_tolightuserdata(state, -1) as *mut ErasedParams;
    ffi::lua_pop(state, 1);
    ((*params).invoke)(state, (*params).data)
}

// Wraps the inner function in a call to `lua_pcall`, so the inner function only has access to a
// limited lua stack. `nargs` and `nresults` are similar to the parameters of `lua_pcall`, but the
// given function return type is not the return value count, instead the inner function return
// values are assumed to match the `nresults` param. Provided function must *not* panic, and since
// it will generally be longjmping, should not contain any values that implements Drop.
// Internally uses 3 extra stack spaces, and does not call checkstack.
pub(crate) unsafe fn protect_lua_closure<F, R>(
    state: *mut ffi::lua_State,
    nargs: c_int,
    nresults: c_int,
    f: F,
) -> Result<R>
where
    F: FnOnce(*mut ffi::lua_State) -> R,
    R: Copy,
{
    struct Params<F, R> {
        function: Option<F>,
        result: MaybeUninit<R>,
        nresults: c_int,
    }

    // To avoid making constant c closures for every protect case, we use the call_trampoline
    // and then push into invoke_thunk
    unsafe fn invoke_thunk<F, R>(state: *mut ffi::lua_State, data: *mut c_void) -> c_int
    where
        F: FnOnce(*mut ffi::lua_State) -> R,
        R: Copy,
    {
        let params = &mut *(data as *mut Params<F, R>);
        let f = params.function.take().unwrap();
        params.result.write(f(state));
        if params.nresults == ffi::LUA_MULTRET {
            ffi::lua_gettop(state)
        } else {
            params.nresults
        }
    }

    check_stack(state, 2)?;

    let stack_start = ffi::lua_gettop(state) - nargs;

    let extra = crate::state::ExtraData::get(state);
    mlua_debug_assert!(!extra.is_null(), "ExtraData is null in protect_lua_closure");

    ffi::lua_getrefpool(state, (*extra).error_traceback_ref);
    ffi::lua_getrefpool(state, (*extra).call_trampoline_ref);

    if nargs > 0 {
        ffi::lua_rotate(state, stack_start + 1, 2);
    }

    let mut params = Params {
        function: Some(f),
        result: MaybeUninit::uninit(),
        nresults,
    };

    let mut erased = ErasedParams {
        invoke: invoke_thunk::<F, R>,
        data: &mut params as *mut _ as *mut c_void,
    };

    ffi::lua_pushlightuserdata(state, &mut erased as *mut _ as *mut c_void);
    let ret = ffi::lua_pcall(state, nargs + 1, nresults, stack_start + 1);
    ffi::lua_remove(state, stack_start + 1); // remove error handler

    if ret == ffi::LUA_OK {
        // `LUA_OK` is only returned when the `do_call` function has completed successfully, so
        // `params.result` is definitely initialized.
        Ok(params.result.assume_init())
    } else {
        Err(pop_error(state, ret))
    }
}

// Runs `f` on the current stack under Luwu's `lua_rawpcall`, which catches a Lua error without a Lua call frame. `f` gets
// the `nargs` values on top of the stack, and its top `nresults` values replace them (all of them with LUA_MULTRET), as
// with `protect_lua_closure`. On an error the arguments are removed and the error is returned. A protected `lua_pcall` (`protect_lua_closure`) costs a closure call; this costs a C++ `try`, which is free
// unless something throws.
//
// `f` must not panic, and must index from the top: positive indices and `lua_gettop` count from the caller's frame. It
// gets no error handler, so an error raised in it has no traceback. That suits raw API calls, whose only error is out of
// memory.
pub(crate) unsafe fn protect_lua_raw_closure<F, R>(
    state: *mut ffi::lua_State,
    nargs: c_int,
    nresults: c_int,
    f: F,
) -> Result<R>
where
    F: FnOnce(*mut ffi::lua_State) -> R,
    R: Copy,
{
    struct Params<F, R> {
        function: Option<F>,
        result: MaybeUninit<R>,
    }

    unsafe extern "C-unwind" fn run<F, R>(state: *mut ffi::lua_State, data: *mut c_void)
    where
        F: FnOnce(*mut ffi::lua_State) -> R,
        R: Copy,
    {
        let params = &mut *(data as *mut Params<F, R>);
        let f = params.function.take().unwrap();
        params.result.write(f(state));
    }

    let stack_start = ffi::lua_gettop(state) - nargs;
    let mut params = Params {
        function: Some(f),
        result: MaybeUninit::uninit(),
    };
    let ret = ffi::lua_rawpcall(state, nargs, run::<F, R>, &mut params as *mut _ as *mut c_void);
    if ret != ffi::LUA_OK {
        return Err(pop_error(state, ret));
    }

    // Arguments `f` left below its results go, as they would have with a call. LUA_MULTRET keeps everything.
    let leftover = ffi::lua_gettop(state) - stack_start - nresults;
    if nresults != ffi::LUA_MULTRET && leftover > 0 {
        ffi::lua_rotate(state, stack_start + 1, nresults);
        ffi::lua_settop(state, stack_start + nresults);
    }

    // `run` completed, so it wrote the result
    Ok(params.result.assume_init())
}

pub(crate) unsafe extern "C-unwind" fn error_traceback(state: *mut ffi::lua_State) -> c_int {
    // Luau calls error handler for memory allocation errors, skip it
    // See https://github.com/luau-lang/luau/issues/880

    if MemoryState::limit_reached(state) {
        return 0;
    }

    if ffi::lua_checkstack(state, 2) == 0 {
        // If we don't have enough stack space to even check the error type, do
        // nothing so we don't risk shadowing a rust panic.
        return 1;
    }


    let s = ffi::luaL_tolstring(state, -1, ptr::null_mut());
    if ffi::lua_checkstack(state, ffi::LUA_TRACEBACK_STACK) != 0 {
        ffi::luaL_traceback(state, state, s, 0);
        ffi::lua_remove(state, -2);
    }

    1
}

pub(crate) unsafe extern "C-unwind" fn func_call_error_traceback(state: *mut ffi::lua_State) -> c_int {
    // Luau calls error handler for memory allocation errors, skip it
    // See https://github.com/luau-lang/luau/issues/880
    if MemoryState::limit_reached(state) {
        return 0;
    }

    //if ffi::lua_checkstack(state, 3) == 0 { return 1; }

    if ffi::lua_checkstack(state, ffi::LUA_TRACEBACK_STACK) != 0 {
        ffi::luaL_traceback(state, state, std::ptr::null(), 0);
    } else {
        // Fallback if we can't allocate stack space
        ffi::lua_pushstring(state, cstr!(""));
    }
    // Stack consists of [error object, traceback]
    //
    // This works bc of lua_pcallmulti in luwu
    2
}

pub(crate) unsafe extern "C-unwind" fn func_call_error(state: *mut ffi::lua_State) -> c_int {
    // Luau calls error handler for memory allocation errors, skip it
    // See https://github.com/luau-lang/luau/issues/880
    if MemoryState::limit_reached(state) {
        return 0;
    }

    // Stack consists of [error object]
    1
}