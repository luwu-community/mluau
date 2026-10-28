use std::os::raw::c_void;
use std::{mem, ptr};

use crate::error::Result;

#[inline]
pub(crate) unsafe fn push_fat_cclosure<T>(
    state: *mut ffi::lua_State,
    func: T,
    call_callback: ffi::lua_CFunction,
    debugname: *const std::os::raw::c_char,
    cont_callback: Option<ffi::lua_Continuation>,
) -> Result<()> {
    unsafe extern "C" fn dtor<T>(
        state: *mut ffi::lua_State,
        data: *mut c_void,
        _sz: usize,
    ) {
        // Almost none Lua operations are allowed when destructor is running,
        // so we need to set a flag to prevent calling any Lua functions
        let extra = (*ffi::lua_callbacks(state)).userdata as *mut crate::state::ExtraData;
        (*extra).running_gc = true;
        // Luau does not support _any_ panics in destructors (they are declared as "C", NOT as "C-unwind"),
        // so any panics will trigger `abort()`.
        ptr::drop_in_place(data as *mut T);
        (*extra).running_gc = false;
    }

    let ptr = protect_lua_raw!(state, 0, 1, |state| {
        ffi::lua_pushcclosurewithdatak(
            state,
            call_callback,
            debugname,
            cont_callback,
            mem::size_of::<T>(),
            Some(dtor::<T>),
        )
    })? as *mut T;
    
    ptr::write(ptr, func);
    Ok(())
}

// Pushes a fat C closure whose function is a Luwu data function (`lua_pushdatacfunctionk`): the VM passes it `func`, kept
// in the closure, and the argument count, so it calls no C API to find them. `func` is dropped with the closure.
pub(crate) unsafe fn push_fat_data_cfunction<T>(
    state: *mut ffi::lua_State,
    func: T,
    call: ffi::lua_DataCFunction,
    debugname: *const std::os::raw::c_char,
) -> Result<()> {
    unsafe extern "C" fn dtor<T>(state: *mut ffi::lua_State, data: *mut c_void, _sz: usize) {
        // See push_fat_cclosure's destructor
        let extra = (*ffi::lua_callbacks(state)).userdata as *mut crate::state::ExtraData;
        (*extra).running_gc = true;
        ptr::drop_in_place(data as *mut T);
        (*extra).running_gc = false;
    }

    let ptr = protect_lua_raw!(state, 0, 1, |state| {
        ffi::lua_pushdatacfunctionk(state, call, debugname, None, mem::size_of::<T>(), Some(dtor::<T>))
    })? as *mut T;

    ptr::write(ptr, func);
    Ok(())
}
