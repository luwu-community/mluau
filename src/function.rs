use std::cell::RefCell;
use std::os::raw::{c_int, c_void};
use std::{mem, slice};

use crate::error::Result;

use crate::traits::{FromLuaMulti, IntoLuaMulti};
use crate::types::{LuaType, ValueRef};
use crate::util::{
    StackGuard, assert_stack, check_stack, linenumber_to_usize, ptr_to_lossy_str, ptr_to_str, to_string
};
use crate::{FromLuaErr, WeakLua};

/// Handle to an internal Lua function.
#[derive(Clone, Debug, PartialEq)]
pub struct Function(pub(crate) ValueRef);

/// Contains information about a function.
///
/// Please refer to the [`Lua Debug Interface`] for more information.
///
/// [`Lua Debug Interface`]: https://www.lua.org/manual/5.4/manual.html#4.7
#[derive(Clone, Debug)]
pub struct FunctionInfo {
    /// A (reasonable) name of the function (`None` if the name cannot be found).
    pub name: Option<String>,
    /// Explains the `name` field (can be `global`/`local`/`method`/`field`/`upvalue`/etc).
    ///
    /// Always `None` for Luau.
    pub name_what: Option<&'static str>,
    /// A string `Lua` if the function is a Lua function, `C` if it is a C function, `main` if it is
    /// the main part of a chunk.
    pub what: &'static str,
    /// Source of the chunk that created the function.
    pub source: Option<String>,
    /// A "printable" version of `source`, to be used in error messages.
    pub short_src: Option<String>,
    /// The line number where the definition of the function starts.
    pub line_defined: Option<usize>,
    /// The line number where the definition of the function ends (not set by Luau).
    pub last_line_defined: Option<usize>,
    /// Number of function parameters
    pub num_params: usize,
    /// True if function accepts variable args
    pub is_vararg: bool,
}

/// Luau function coverage snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageInfo {
    pub function: Option<String>,
    pub line_defined: i32,
    pub depth: i32,
    pub hits: Vec<i32>,
}

impl Function {
    /// Calls the function, passing `args` as function arguments.
    ///
    /// The function's return values are converted to the generic type `R`.
    ///
    /// # Examples
    ///
    /// Call Lua's built-in `tostring` function:
    ///
    /// ```
    /// # use mluau::{Function, Lua, Result};
    /// # fn main() -> Result<()> {
    /// # let lua = Lua::new();
    /// let globals = lua.globals();
    ///
    /// let tostring: Function = globals.get("tostring")?;
    ///
    /// assert_eq!(tostring.call::<String>(123)?, "123");
    ///
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Call a function with multiple arguments:
    ///
    /// ```
    /// # use mluau::{Function, Lua, Result};
    /// # fn main() -> Result<()> {
    /// # let lua = Lua::new();
    /// let sum: Function = lua.load(
    ///     r#"
    ///         function(a, b)
    ///             return a + b
    ///         end
    /// "#).eval()?;
    ///
    /// assert_eq!(sum.call::<u32>((3, 4))?, 3 + 4);
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn call<R: FromLuaMulti>(&self, args: impl IntoLuaMulti) -> Result<R> {
        self.call_with_err(args)
    }

    /// Same as call but supports custom error values
    pub fn call_with_err<R: FromLuaMulti, E: FromLuaErr>(&self, args: impl IntoLuaMulti) -> std::result::Result<R, E> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 2).map_err(E::from_rust_err)?;

            // Push error handler
            if E::NEEDS_TRACEBACK {
                ffi::lua_getrefpool(state, (*lua.extra()).func_call_error_traceback);
            } else {
                ffi::lua_getrefpool(state, (*lua.extra()).func_call_error);
            }

            let stack_start = ffi::lua_gettop(state);
            // Push function and the arguments
            lua.push_ref_at(&self.0, state);
            let nargs = args.push_into_specified_stack_multi(&lua, state).map_err(E::from_rust_err)?;
            
            // Call the function
            let ret = ffi::lua_pcallmulti(state, nargs, ffi::LUA_MULTRET, stack_start);
            
            if ret != ffi::LUA_OK {
                let num_err_retvals = ffi::lua_gettop(state) - stack_start;
                if num_err_retvals == 2 {
                    // Stack: [..., error_object, traceback_string]
                    let tb = to_string(state, -1);
                    let err_value = lua.stack_value_at(-2, None, state);            
                    return Err(E::from_lua_err(lua.lua(), err_value, ret, tb));
                } else if num_err_retvals == 1 {
                    // Stack: [..., error_object]
                    let err_value = lua.stack_value_at(-1, None, state);         
                    return Err(E::from_lua_err(lua.lua(), err_value, ret, String::with_capacity(0)));
                } else {
                    // No results, catastrophic failure
                    return Err(E::from_lua_err(lua.lua(), crate::Value::Nil, ret, String::with_capacity(0)));
                }
            }

            // Get the results
            let nresults = ffi::lua_gettop(state) - stack_start;
            R::from_specified_stack_multi(nresults, &lua, state).map_err(E::from_rust_err)
        }
    }

    /// Returns information about the function.
    ///
    /// Corresponds to the `>Sn` what mask for [`lua_getinfo`] when applied to the function.
    ///
    /// [`lua_getinfo`]: https://www.lua.org/manual/5.4/manual.html#lua_getinfo
    pub fn info(&self) -> FunctionInfo {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 1);

            let mut ar: ffi::lua_Debug = mem::zeroed();
            lua.push_ref_at(&self.0, state);
            let res = ffi::lua_getinfo(state, -1, cstr!("sn"), &mut ar);
            mlua_assert!(res != 0, "lua_getinfo failed with `>Snu`");

            FunctionInfo {
                name: ptr_to_lossy_str(ar.name).map(|s| s.into_owned()),
                name_what: None,
                what: ptr_to_str(ar.what).unwrap_or("main"),
                source: ptr_to_lossy_str(ar.source).map(|s| s.into_owned()),
                short_src: ptr_to_lossy_str(ar.short_src).map(|s| s.into_owned()),
                line_defined: linenumber_to_usize(ar.linedefined),
                last_line_defined: None,
                num_params: ar.nparams as usize,
                is_vararg: ar.isvararg != 0,
            }
        }
    }

    /// Retrieves recorded coverage information about this Lua function including inner calls.
    ///
    /// This function takes a callback as an argument and calls it providing [`CoverageInfo`]
    /// snapshot per each executed inner function.
    ///
    /// Recording of coverage information is controlled by [`Compiler::set_coverage_level`] option.
    ///
    /// [`Compiler::set_coverage_level`]: crate::chunk::Compiler::set_coverage_level
    pub fn coverage<F>(&self, func: F)
    where
        F: FnMut(CoverageInfo),
    {
        use std::ffi::CStr;
        use std::os::raw::c_char;

        unsafe extern "C-unwind" fn callback<F: FnMut(CoverageInfo)>(
            data: *mut c_void,
            function: *const c_char,
            line_defined: c_int,
            depth: c_int,
            hits: *const c_int,
            size: usize,
        ) {
            let function = if !function.is_null() {
                Some(CStr::from_ptr(function).to_string_lossy().to_string())
            } else {
                None
            };
            let rust_callback = &*(data as *const RefCell<F>);
            if let Ok(mut rust_callback) = rust_callback.try_borrow_mut() {
                // Call the Rust callback with CoverageInfo
                rust_callback(CoverageInfo {
                    function,
                    line_defined,
                    depth,
                    hits: slice::from_raw_parts(hits, size).to_vec(),
                });
            }
        }

        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 1);

            lua.push_ref_at(&self.0, state);
            let func = RefCell::new(func);
            let func_ptr = &func as *const RefCell<F> as *mut c_void;
            ffi::lua_getcoverage(state, -1, func_ptr, callback::<F>);
        }
    }

    /// Converts this function to a generic C pointer.
    ///
    /// There is no way to convert the pointer back to its original value.
    ///
    /// Typically this function is used only for hashing and debug information.
    #[inline]
    pub fn to_pointer(&self) -> *const c_void {
        self.0.to_pointer()
    }

    /// Creates a deep clone of the Lua function.
    ///
    /// Copies the function prototype and all its upvalues to the
    /// newly created function.
    /// This function returns shallow clone (same handle) for Rust/C functions.
    pub fn deep_clone(&self) -> Result<Self> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 4)?;

            lua.push_ref_at(&self.0, state);

            if ffi::lua_iscfunction(state, -1) != 0 {
                return Ok(self.clone());
            }

            protect_lua!(state, 1, 1, move |state| {
                ffi::lua_clonefunction(state, -1)
            })?;

            Ok(Function(lua.pop_ref_at(state)))
        }
    }

    #[doc(hidden)]
    pub fn weak_lua(&self) -> WeakLua {
        self.0.lua.clone()
    }
}

impl LuaType for Function {
    const TYPE_ID: c_int = ffi::LUA_TFUNCTION;
}

#[cfg(test)]
mod assertions {
    use super::*;

    static_assertions::assert_not_impl_any!(Function: Send);
}
