use std::os::raw::c_int;
use std::string::String as StdString;
use std::sync::Arc;

use either::Either;

use crate::state::util::push_callback_error;
use crate::types::ErrorWithTraceback;
use crate::{CallbackFinalizeAction, CallbackResult, CustomError, Ok as LuaOk, Yield};
use crate::error::{Error, Result};
use crate::multi::MultiValue;
use crate::state::{ExtraData, Lua, RawLua};
use crate::util::{check_stack, get_error, short_type_name};
use crate::value::Value;

#[inline]
unsafe fn finalize_error(state: *mut ffi::lua_State, extra: *mut ExtraData, err: impl std::fmt::Display) -> CallbackFinalizeAction {
    push_callback_error(state, extra, err.to_string());
    CallbackFinalizeAction::Error
}

pub trait IntoCallbackResult: Sized {
    /// Converts the type into a CallbackResult
    fn into_callback_result(self, lua: &Lua) -> CallbackResult;

    /// Pushes the result to the stack cleaning it out on yields if needed
    /// 
    /// Can be implemented as a fast-path but in general, best not to
    #[doc(hidden)]
    #[inline(always)]
    unsafe fn finalize(self, lua: &RawLua, state: *mut ffi::lua_State) -> CallbackFinalizeAction {
        match self.into_callback_result(lua.lua()) {
            CallbackResult::Ok(v) => LuaOk(v).finalize(lua, state),
            CallbackResult::OkSingle(v) => LuaOk(v).finalize(lua, state),
            CallbackResult::Yield(v) => Yield(v).finalize(lua, state),
            CallbackResult::Error(v) => CustomError(v).finalize(lua, state),
            CallbackResult::LuaError(le) => le.finalize(lua, state)
        }
    }
}

/// Passthrough for users who explicitly return `CallbackResult`
impl IntoCallbackResult for CallbackResult {
    fn into_callback_result(self, _lua: &Lua) -> CallbackResult { self }
}

impl<T: IntoLuaMulti> IntoCallbackResult for LuaOk<T> {
    fn into_callback_result(self, lua: &Lua) -> CallbackResult {
        match self.0.into_lua_multi(lua) {
            Ok(mv) => CallbackResult::Ok(mv),
            Err(e) => CallbackResult::LuaError(e)
        }
    }

    unsafe fn finalize(self, lua: &RawLua, state: *mut ffi::lua_State) -> CallbackFinalizeAction {
        match self.0.push_into_specified_stack_multi(lua, state) {
            Ok(nres) => CallbackFinalizeAction::Return(nres),
            Err(e) => finalize_error(state, lua.extra(), e)
        }
    }
} 

impl<T: IntoLuaMulti> IntoCallbackResult for Yield<T> {
    fn into_callback_result(self, lua: &Lua) -> CallbackResult {
        match self.0.into_lua_multi(lua) {
            Ok(mv) => CallbackResult::Yield(mv),
            Err(e) => CallbackResult::LuaError(e)
        }
    }

    unsafe fn finalize(self, lua: &RawLua, state: *mut ffi::lua_State) -> CallbackFinalizeAction {
        ffi::lua_pop(state, -1); // pop everything before yielding to avoid leaks
        match self.0.push_into_specified_stack_multi(lua, state) {
            Ok(nres) => CallbackFinalizeAction::Yield(nres),
            Err(e) => finalize_error(state, lua.extra(), e)
        }
    }
}

impl<T: IntoCallbackResult, U: IntoCallbackResult> IntoCallbackResult for Either<T, U> {
    fn into_callback_result(self, lua: &Lua) -> CallbackResult {
        match self {
            Self::Left(l) => l.into_callback_result(lua),
            Self::Right(r) => r.into_callback_result(lua)
        }
    }

    unsafe fn finalize(self, lua: &RawLua, state: *mut ffi::lua_State) -> CallbackFinalizeAction {
        match self {
            Self::Left(l) => l.finalize(lua, state),
            Self::Right(r) => r.finalize(lua, state)
        }
    }
}

impl<T: IntoLua> IntoCallbackResult for CustomError<T> {
    fn into_callback_result(self, lua: &Lua) -> CallbackResult {
        match self.0.into_lua(lua) {
            Ok(mv) => CallbackResult::Error(mv),
            Err(e) => CallbackResult::LuaError(e)
        }
    }

    unsafe fn finalize(self, lua: &RawLua, state: *mut ffi::lua_State) -> CallbackFinalizeAction {
        match self.0.push_into_specified_stack(lua, state) {
            Ok(_) => CallbackFinalizeAction::Error,
            Err(e) => finalize_error(state, lua.extra(), e)
        }
    }
}

impl IntoCallbackResult for crate::Error {
    fn into_callback_result(self, _lua: &Lua) -> CallbackResult { CallbackResult::LuaError(self) }

    unsafe fn finalize(self, lua: &RawLua, state: *mut ffi::lua_State) -> CallbackFinalizeAction { finalize_error(state, lua.extra(), self) }
}

impl<T> IntoCallbackResult for std::result::Result<T, crate::Error>
where
    T: IntoLuaMulti,
{
    fn into_callback_result(self, lua: &Lua) -> CallbackResult {
        match self {
            Ok(success) => match success.into_lua_multi(lua) {
                Ok(mv) => CallbackResult::Ok(mv),
                Err(err) => CallbackResult::LuaError(err)
            },
            Err(error) => CallbackResult::LuaError(error)
        }
    }
}

/// Trait for types convertible to [`Value`].
pub trait IntoLua: Sized {
    /// Performs the conversion.
    fn into_lua(self, lua: &Lua) -> Result<Value>;

    /// Pushes the value directly into a Lua stack
    ///
    /// # Safety
    /// This method does not check Lua stack space.
    #[doc(hidden)]
    #[inline]
    unsafe fn push_into_specified_stack(self, lua: &RawLua, state: *mut ffi::lua_State) -> Result<()> {
        lua.push_value_at(&self.into_lua(lua.lua())?, state);
        Ok(())
    }
}

/// Trait for types convertible from [`Value`].
pub trait FromLua: Sized {
    /// Performs the conversion.
    fn from_lua(value: Value, lua: &Lua) -> Result<Self>;

    /// Performs the conversion for an argument (eg. function argument).
    ///
    /// `i` is the argument index (position),
    /// `to` is a function name that received the argument.
    #[doc(hidden)]
    #[inline]
    fn from_lua_arg(arg: Value, i: usize, to: Option<&str>, lua: &Lua) -> Result<Self> {
        Self::from_lua(arg, lua).map_err(|err| Error::BadArgument {
            to: to.map(|s| s.to_string()),
            pos: i,
            name: None,
            cause: Arc::new(err),
        })
    }

    /// Performs the conversion for a value in the Lua stack at index `idx`.
    #[doc(hidden)]
    #[inline]
    unsafe fn from_specified_stack(idx: c_int, lua: &RawLua, state: *mut ffi::lua_State) -> Result<Self> {
        Self::from_lua(lua.stack_value_at(idx, None, state), lua.lua())
    }

    /// Same as `from_lua_arg` but for a value in the Lua stack at index `idx`.
    #[doc(hidden)]
    #[inline]
    unsafe fn from_specified_stack_arg(
        idx: c_int,
        i: usize,
        to: Option<&str>,
        lua: &RawLua,
        state: *mut ffi::lua_State,
    ) -> Result<Self> {
        Self::from_specified_stack(idx, lua, state).map_err(|err| Error::BadArgument {
            to: to.map(|s| s.to_string()),
            pos: i,
            name: None,
            cause: Arc::new(err),
        })
    }
}

/// Trait for types convertible from error Value + tb
pub trait FromLuaErr: Sized {
    /// Whether or not we need the traceback string at all
    const NEEDS_TRACEBACK: bool = true;

    /// Performs the conversion using a safe Lua Value and an traceback.
    fn from_lua_err(lua: &Lua, err: Value, errcode: c_int, tb: String) -> Self;

    /// Converts from a Error type generated by mluau (due to safety invariants being violated etc)
    /// into the error type
    fn from_rust_err(error: crate::Error) -> Self;
}

impl FromLuaErr for crate::Error {
    const NEEDS_TRACEBACK: bool = false;
    fn from_lua_err(_lua: &Lua, err: Value, errcode: c_int, _tb: String) -> Self {
        let error_string = match err {
            Value::Nil => "<nil>".to_string(),
            Value::Boolean(b) => b.to_string(),            
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.to_string_lossy(),
            Value::Table(t) => format!("<table {:?}>", t.to_pointer()),
            Value::Function(f) => format!("<function {:?}>", f.to_pointer()),
            Value::UserData(u) => format!("<userdata {:?}>", u.to_pointer()),
            Value::Thread(t) => format!("<thread {:?}>", t.to_pointer()),
            Value::Buffer(b) => format!("<buffer {:?}>", b.to_pointer()),
            Value::LightUserData(l) => format!("<lightuserdata {:?}>", l.0),
            #[cfg(not(feature = "luau-vector4"))]
            Value::Vector(v) => format!("vector({}, {}, {})", v.x(), v.y(), v.z()),
            #[cfg(feature = "luau-vector4")]
            Value::Vector(v) => format!("vector({}, {}, {}, {})", v.x(), v.y(), v.z(), v.w()),
            _ => "<unknown>".to_string(),
        };

        get_error(error_string, errcode)
    }

    fn from_rust_err(error: crate::Error) -> Self { error }
}

impl FromLuaErr for ErrorWithTraceback {
    fn from_lua_err(_lua: &Lua, err: Value, errcode: c_int, tb: String) -> Self {
        Self::Error {
            traceback: tb,
            value: err,
            err_code: errcode
        }
    }

    fn from_rust_err(error: crate::Error) -> Self { ErrorWithTraceback::BaseError(error) }
}

/// Trait for types convertible to any number of Lua values.
///
/// This is a generalization of [`IntoLua`], allowing any number of resulting Lua values instead of
/// just one. Any type that implements [`IntoLua`] will automatically implement this trait.
pub trait IntoLuaMulti: Sized {
    /// Performs the conversion.
    fn into_lua_multi(self, lua: &Lua) -> Result<MultiValue>;

    /// Pushes the values directly into a Lua stack
    ///
    /// Returns number of pushed values.
    #[inline]
    unsafe fn push_into_specified_stack_multi(
        self,
        lua: &RawLua,
        state: *mut ffi::lua_State,
    ) -> Result<c_int> {
        let values = self.into_lua_multi(lua.lua())?;
        let len: c_int = values.len().try_into().unwrap();
        unsafe {
            check_stack(state, len + 1)?;
            for val in &values {
                lua.push_value_at(val, state);
            }
        }
        Ok(len)
    }
}

/// Trait for types that can be created from an arbitrary number of Lua values.
///
/// This is a generalization of [`FromLua`], allowing an arbitrary number of Lua values to
/// participate in the conversion. Any type that implements [`FromLua`] will automatically
/// implement this trait.
pub trait FromLuaMulti: Sized {
    /// Performs the conversion.
    ///
    /// In case `values` contains more values than needed to perform the conversion, the excess
    /// values should be ignored. This reflects the semantics of Lua when calling a function or
    /// assigning values. Similarly, if not enough values are given, conversions should assume that
    /// any missing values are nil.
    fn from_lua_multi(values: MultiValue, lua: &Lua) -> Result<Self>;

    /// Performs the conversion for a list of arguments.
    ///
    /// `i` is an index (position) of the first argument,
    /// `to` is a function name that received the arguments.
    #[doc(hidden)]
    #[inline]
    fn from_lua_args(args: MultiValue, i: usize, to: Option<&str>, lua: &Lua) -> Result<Self> {
        let _ = (i, to);
        Self::from_lua_multi(args, lua)
    }

    /// Performs the conversion for a number of values in the specified Lua stack.
    #[doc(hidden)]
    #[inline]
    unsafe fn from_specified_stack_multi(
        nvals: c_int,
        lua: &RawLua,
        state: *mut ffi::lua_State,
    ) -> Result<Self> {
        let mut values = MultiValue::with_capacity(nvals as usize);
        for idx in 0..nvals {
            values.push_back(lua.stack_value_at(-nvals + idx, None, state));
        }
        Self::from_lua_multi(values, lua.lua())
    }

    /// Same as `from_lua_args` but for a number of values in the specified Lua stack.
    #[doc(hidden)]
    #[inline]
    unsafe fn from_specified_stack_args(
        nvals: c_int,
        i: usize,
        to: Option<&str>,
        lua: &RawLua,
        state: *mut ffi::lua_State,
    ) -> Result<Self> {
        let _ = (i, to);
        Self::from_specified_stack_multi(nvals, lua, state).map_err(|err| Error::BadArgument {
            to: to.map(|s| s.to_string()),
            pos: i,
            name: None,
            cause: Arc::new(err),
        })
    }
}

pub(crate) trait ShortTypeName {
    #[inline(always)]
    fn type_name() -> StdString {
        short_type_name::<Self>()
    }
}

impl<T> ShortTypeName for T {}
