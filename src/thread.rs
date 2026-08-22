use std::fmt;
use std::os::raw::{c_int, c_void};
use std::rc::Rc;
use std::string::String as StdString;
use std::result::Result as StdResult;
use crate::error::{Error, Result};
use crate::function::Function;
use crate::state::{ExtraData, RawLua, callback_error_ext};
use crate::traits::{FromLuaMulti, IntoLuaMulti};
use crate::types::{LuaType, TypedRef, UnbackedTypedRef, ValueRef};

use crate::util::{StackGuard, check_stack, to_string};
use crate::{FromLuaErr, WeakLua};

/// Continuation thread status. Can either be Ok, Yielded (rare, but can happen) or Error
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ContinuationStatus {
    Ok,
    Yielded,
    Error(c_int),
}

impl ContinuationStatus {
    #[allow(dead_code)]
    pub(crate) fn from_status(status: c_int) -> Self {
        match status {
            ffi::LUA_YIELD => Self::Yielded,
            ffi::LUA_OK => Self::Ok,
            s => Self::Error(s),
        }
    }
}

/// Status of a Lua thread (coroutine).
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ThreadStatus {
    /// The thread was just created or is suspended (yielded).
    ///
    /// If a thread is in this state, it can be resumed by calling [`Thread::resume`].
    Resumable,
    /// The thread is currently running.
    Running,
    /// The thread has finished executing.
    Finished,
    /// The thread has raised a Lua error during execution.
    Error,
}

/// Internal representation of a Lua thread status.
///
/// The number in `New` and `Yielded` variants is the number of arguments pushed
/// to the thread stack.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ThreadStatusInner {
    New(c_int),
    Running,
    Yielded(c_int),
    Finished,
    Error,
}

/// Handle to an internal Lua thread (coroutine).
#[derive(Clone)]
pub struct Thread(pub(crate) ValueRef, pub(crate) *mut ffi::lua_State);

impl Thread {
    /// Returns reference to the Lua state that this thread is associated with.
    #[doc(hidden)]
    #[inline(always)]
    pub fn state(&self) -> *mut ffi::lua_State {
        self.1
    }

    /// Tries converting whatever is on the thread stack to ``R``.
    ///
    /// Useful if you know the thread has something but cannot extract it directly.
    ///
    /// # Safety
    ///
    /// Note that while this method is usually safe to call, the results returned
    /// by this method could be used to induce memory unsafety. Note that all cases
    /// of this happening, however, are bugs in mluau.
    pub fn pop_results<R>(&self) -> Result<R>
    where
        R: FromLuaMulti,
    {
        unsafe {
            let lua = self.0.lua.lock();
            let thread_state = self.state();
            let _sg = StackGuard::new(lua.state());
            let _thread_sg = StackGuard::with_top(thread_state, 0);
            let nresults = ffi::lua_gettop(thread_state);
            R::from_specified_stack_multi(nresults, &lua, thread_state)
        }
    }

    /// Returns the thread data without removing it from the thread
    ///
    /// Returns `None` if no data was set for the current lua thread or if the provided type
    /// does not match the stored data type.
    /// 
    /// # Safety
    /// 
    /// Must not reset thread while holding onto the TypedRef
    pub fn with_data<T: 'static>(self) -> Option<TypedRef<T, Self, 0>> {
        let lua = self.0.lua.lock();
        let thread_state = self.state();
        let ptr = unsafe {
            let current = ffi::lua_getthreaddata(thread_state);
            if current.is_null() {
                return None;
            }
            crate::types::ErasedHeader::downcast_ref(current)
        };
        TypedRef::new_opt(lua.0, ptr, self)
    }

    /// Similar to `with_data` but takes a reference to the underlying data
    /// 
    /// # Safety
    /// 
    /// Must not reset thread while holding onto the TypedRef
    pub fn with_data_ref<T: 'static>(&self) -> Option<UnbackedTypedRef<'_, T>> {
        let lua = self.0.lua.lock();
        let thread_state = self.state();
        let ptr = unsafe {
            let current = ffi::lua_getthreaddata(thread_state);
            if current.is_null() {
                return None;
            }
            crate::types::ErasedHeader::downcast_ref(current)
        };
        UnbackedTypedRef::new_opt(lua.0, ptr, &self.0)
    }

    /// Sets the thread data. The set thread data will automatically be dropped upon Luau GC
    ///
    /// Errors if thread data was already set for the current lua thread.
    ///
    /// This is a Luau specific extension.
    pub fn set_thread_data<T: 'static>(&self, data: T) -> Result<()> {
        let lua = self.0.lua.lock();
        let thread_state = self.state();
        unsafe {
            let current = ffi::lua_getthreaddata(thread_state);
            if !current.is_null() {
                return Err(Error::runtime("thread data was already set for this thread"));
            }
            let raw = crate::types::ErasedHeader::into_raw(data);
            ffi::lua_setthreaddata(thread_state, raw);
            let extra = lua.extra();
            if !(*extra).have_thread_data {
                (*extra).have_thread_data = true;
                (*ffi::lua_callbacks(lua.main_state())).userthread = Some(crate::Lua::userthread_proc);
            }
        }
        Ok(())
    }

    /// Resumes execution of this thread.
    ///
    /// Equivalent to [`coroutine.resume`].
    ///
    /// Passes `args` as arguments to the thread. If the coroutine has called [`coroutine.yield`],
    /// it will return these arguments. Otherwise, the coroutine wasn't yet started, so the
    /// arguments are passed to its main function.
    ///
    /// If the thread is no longer resumable (meaning it has finished execution or encountered an
    /// error), this will return [`Error::CoroutineUnresumable`], otherwise will return `Ok` as
    /// follows:
    ///
    /// If the thread calls [`coroutine.yield`], returns the values passed to `yield`. If the thread
    /// `return`s values from its main function, returns those.
    ///
    /// # Examples
    ///
    /// ```
    /// # use mluau::{Error, Lua, Result, Thread};
    /// # fn main() -> Result<()> {
    /// # let lua = Lua::new();
    /// let thread: Thread = lua.load(r#"
    ///     coroutine.create(function(arg)
    ///         assert(arg == 42)
    ///         local yieldarg = coroutine.yield(123)
    ///         assert(yieldarg == 43)
    ///         return 987
    ///     end)
    /// "#).eval()?;
    ///
    /// assert_eq!(thread.resume::<u32, Error>(42)?, 123);
    /// assert_eq!(thread.resume::<u32, Error>(43)?, 987);
    ///
    /// // The coroutine has now returned, so `resume` will fail
    /// match thread.resume::<u32, Error>(()) {
    ///     Err(Error::CoroutineUnresumable) => {},
    ///     unexpected => panic!("unexpected result {:?}", unexpected),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`coroutine.resume`]: https://www.lua.org/manual/5.4/manual.html#pdf-coroutine.resume
    /// [`coroutine.yield`]: https://www.lua.org/manual/5.4/manual.html#pdf-coroutine.yield
    pub fn resume<R, E>(&self, args: impl IntoLuaMulti) -> StdResult<R, E>
    where
        R: FromLuaMulti,
        E: FromLuaErr
    {
        let lua = self.0.lua.lock();
        let mut has_yielded = false;
        let mut pushed_nargs = match self.status_inner(&lua) {
            ThreadStatusInner::New(nargs) => nargs,
            ThreadStatusInner::Yielded(nargs) => {
                has_yielded = true;
                nargs
            }
            _ => return Err(E::from_rust_err(Error::CoroutineUnresumable)),
        };

        let state = lua.state();
        let thread_state = self.state();
        unsafe {
            let _sg = StackGuard::new(state);

            if has_yielded {
                // We need to use the mainthread here as pcall over a yielded thread is not allowed
                let nargs = args.push_into_specified_stack_multi(&lua, state).map_err(E::from_rust_err)?;
                if nargs > 0 {
                    check_stack(thread_state, nargs).map_err(E::from_rust_err)?;
                    ffi::lua_xmove(state, thread_state, nargs);
                }
                pushed_nargs += nargs;
            } else {
                let nargs = args.push_into_specified_stack_multi(&lua, thread_state).map_err(E::from_rust_err)?;
                pushed_nargs += nargs;
            }

            let _thread_sg = StackGuard::with_top(thread_state, 0);
            let (_, nresults) = self.resume_inner(&lua, pushed_nargs)?;

            R::from_specified_stack_multi(nresults, &lua, thread_state).map_err(E::from_rust_err)
        }
    }

    /// Resumes execution of this thread, immediately raising an error.
    ///
    /// This is a Luau specific extension.
    pub fn resume_error<R, E>(&self, error: impl crate::IntoLua) -> StdResult<R, E>
    where
        R: FromLuaMulti,
        E: FromLuaErr
    {
        let lua = self.0.lua.lock();
        let mut has_yielded = false;
        match self.status_inner(&lua) {
            ThreadStatusInner::New(_) => {}
            ThreadStatusInner::Yielded(_) => has_yielded = true,
            _ => return Err(E::from_rust_err(Error::CoroutineUnresumable)),
        };

        let state = lua.state();
        let thread_state = self.state();
        unsafe {
            let _sg = StackGuard::new(state);

            if has_yielded {
                // We need to use the mainthread here as pcall over a yielded thread is not allowed
                check_stack(state, 1).map_err(E::from_rust_err)?;
                error.push_into_specified_stack(&lua, state).map_err(E::from_rust_err)?;
                ffi::lua_xmove(state, thread_state, 1);
            } else {
                check_stack(thread_state, 1).map_err(E::from_rust_err)?;
                error.push_into_specified_stack(&lua, thread_state).map_err(E::from_rust_err)?;
            }

            let _thread_sg = StackGuard::with_top(thread_state, 0);
            let (_, nresults) = self.resume_inner(&lua, ffi::LUA_RESUMEERROR)?;

            R::from_specified_stack_multi(nresults, &lua, thread_state).map_err(E::from_rust_err)
        }
    }

    /// Resumes execution of this thread.
    ///
    /// It's similar to `resume()` but leaves `nresults` values on the thread stack.
    unsafe fn resume_inner<E: FromLuaErr>(&self, lua: &RawLua, nargs: c_int) -> StdResult<(ThreadStatusInner, c_int), E> {
        let state = lua.state();
        let thread_state = self.state();
        let mut nresults = 0;
        let ret = ffi::lua_resumex(thread_state, state, nargs, &mut nresults as *mut c_int);

        match ret {
            ffi::LUA_OK => Ok((ThreadStatusInner::Finished, nresults)),
            ffi::LUA_YIELD => Ok((ThreadStatusInner::Yielded(0), nresults)),
            ffi::LUA_ERRMEM => {
                let err_value = lua.stack_value_at(-1, None, thread_state);
                ffi::lua_pop(thread_state, 1);
                Err(E::from_lua_err(lua.lua(), err_value, ret, String::with_capacity(0)))
            }
            _ => {
                let tb_string = if E::NEEDS_TRACEBACK {
                    check_stack(state, 3).map_err(E::from_rust_err)?;
                    protect_lua!(state, 0, 1, |state| {
                        if ffi::lua_checkstack(state, ffi::LUA_TRACEBACK_STACK) != 0 {
                            ffi::luaL_traceback(state, thread_state, std::ptr::null(), 0);
                        } else {
                            // Fallback if we can't allocate stack space
                            ffi::lua_pushstring(state, cstr!(""));
                        }
                    }).map_err(E::from_rust_err)?;
                    to_string(state, -1)
                } else {
                    StdString::with_capacity(0)
                };
                let err_value = lua.stack_value_at(-1, None, thread_state);
                Err(E::from_lua_err(lua.lua(), err_value, ret, tb_string))
            }
        }
    }

    /// Gets the status of the thread as the raw Luau state (ffi::LUA_OK, ffi::LUA_YIELD, ffi::LUA_ERR*)
    #[inline]
    pub fn raw_status(&self) -> i32 {
        unsafe { ffi::lua_status(self.state()) }
    }

    /// Gets the size of the threads current stack
    #[inline]
    pub fn get_top(&self) -> i32 {
        unsafe { ffi::lua_gettop(self.state()) }
    }

    /// Gets the status of the thread.
    pub fn status(&self) -> ThreadStatus {
        match self.status_inner(&self.0.lua.lock()) {
            ThreadStatusInner::New(_) | ThreadStatusInner::Yielded(_) => ThreadStatus::Resumable,
            ThreadStatusInner::Running => ThreadStatus::Running,
            ThreadStatusInner::Finished => ThreadStatus::Finished,
            ThreadStatusInner::Error => ThreadStatus::Error,
        }
    }

    /// Gets the status of the thread (internal implementation).
    fn status_inner(&self, lua: &RawLua) -> ThreadStatusInner {
        let thread_state = self.state();
        if thread_state == lua.state() {
            // The thread is currently running
            return ThreadStatusInner::Running;
        }
        let status = unsafe { ffi::lua_status(thread_state) };
        match status {
            ffi::LUA_YIELD => {
                let top = unsafe { ffi::lua_gettop(thread_state) };
                ThreadStatusInner::Yielded(top)
            }
            ffi::LUA_OK => {
                let top = unsafe { ffi::lua_gettop(thread_state) };
                if top > 0 {
                    ThreadStatusInner::New(top - 1)
                } else {
                    ThreadStatusInner::Finished
                }
            }
            _ => ThreadStatusInner::Error,
        }
    }

    /// Resets a thread
    ///
    /// In [Lua 5.4]: cleans its call stack and closes all pending to-be-closed variables.
    /// Returns a error in case of either the original error that stopped the thread or errors
    /// in closing methods.
    ///
    /// In Luau: resets to the initial state of a newly created Lua thread.
    /// Lua threads in arbitrary states (like yielded or errored) can be reset properly.
    ///
    /// Other Lua versions can reset only new or finished threads.
    ///
    /// Sets a Lua function for the thread afterwards.
    ///
    /// [Lua 5.4]: https://www.lua.org/manual/5.4/manual.html#lua_closethread
    pub fn reset(&self, func: Function) -> Result<()> {
        let lua = self.0.lua.lock();
        let thread_state = self.state();
        if thread_state == lua.state() {
            return Err(Error::runtime("cannot reset a running thread"));
        }
        unsafe {
            ffi::lua_resetthread(thread_state);

            // Push function to the top of the thread stack
            lua.push_ref_at(&func.0, thread_state);

            {
                // Inherit `LUA_GLOBALSINDEX` from the main thread
                ffi::lua_xpush(lua.main_state(), thread_state, ffi::LUA_GLOBALSINDEX);
                ffi::lua_replace(thread_state, ffi::LUA_GLOBALSINDEX);
            }

            Ok(())
        }
    }

    /// Closes a thread and marks it as finished.
    ///
    /// In [Lua 5.4]: cleans its call stack and closes all pending to-be-closed variables.
    /// Returns a error in case of either the original error that stopped the thread or errors
    /// in closing methods.
    ///
    /// In Luau: resets to the initial state of a newly created Lua thread.
    /// Lua threads in arbitrary states (like yielded or errored) can be reset properly.
    ///
    pub fn close(&self) -> Result<()> {
        let lua = self.0.lua.lock();
        if self.state() == lua.state() {
            return Err(Error::runtime("cannot reset a running thread"));
        }

        let thread_state = self.state();
        unsafe {
            ffi::lua_resetthread(thread_state);
            Ok(())
        }
    }

    /// Enables sandbox mode on this thread.
    ///
    /// Under the hood replaces the global environment table with a new table,
    /// that performs writes locally and proxies reads to caller's global environment.
    ///
    /// This mode ideally should be used together with the global sandbox mode [`Lua::sandbox`].
    ///
    /// Please note that Luau links environment table with chunk when loading it into Lua state.
    /// Therefore you need to load chunks into a thread to link with the thread environment.
    ///
    /// # Examples
    ///
    /// ```
    /// # use mluau::{Lua, Result, Error};
    /// #
    /// # fn main() -> Result<()> {
    /// let lua = Lua::new();
    /// let thread = lua.create_thread(lua.create_function(|lua2, ()| {
    ///     lua2.load("var = 123").exec()?;
    ///     assert_eq!(lua2.globals().get::<u32>("var")?, 123);
    ///     Ok::<_, mluau::Error>(())
    /// })?)?;
    /// thread.sandbox()?;
    /// thread.resume::<(), Error>(())?;
    ///
    /// // The global environment should be unchanged
    /// assert_eq!(lua.globals().get::<Option<u32>>("var")?, None);
    /// # Ok(())
    /// # }
    ///
    /// ```
    pub fn sandbox(&self) -> Result<()> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        let thread_state = self.state();
        unsafe {
            check_stack(thread_state, 3)?;
            check_stack(state, 3)?;
            protect_lua!(state, 0, 0, |_| ffi::luaL_sandboxthread(thread_state))
        }
    }

    /// Converts this thread to a generic C pointer.
    ///
    /// There is no way to convert the pointer back to its original value.
    ///
    /// Typically this function is used only for hashing and debug information.
    #[inline]
    pub fn to_pointer(&self) -> *const c_void {
        self.0.to_pointer()
    }

    /// Creates a traceback of the given thread
    pub fn traceback(&self) -> Result<StdString> {
        let lua = self.0.lua.lock();
        let thread_state = self.state();
        unsafe { lua.traceback_at(thread_state) }
    }

    #[doc(hidden)]
    pub fn weak_lua(&self) -> WeakLua {
        self.0.lua.clone()
    }

    /// Attach the global user thread state change callback to the thread
    pub fn attach_thread_state_change_callback(&self) {
        unsafe {
            ffi::lua_setthreadstatechangecb(self.state(), Some(Self::userthreadstatechange_proc));
        }
    }

    /// Detach the global user thread state change callback from the thread
    pub fn detach_thread_state_change_callback(&self) {
        unsafe {
            ffi::lua_setthreadstatechangecb(self.state(), None);
        }
    }

    pub(crate) unsafe extern "C-unwind" fn userthreadstatechange_proc(
        #[allow(non_snake_case)]
        L: *mut ffi::lua_State,
        status: c_int,
    ) {
        let extra = ExtraData::get(L);
        let callback = match (*extra).thread_state_change_callback {
            Some(ref cb) => cb.clone(),
            None => return,
        };
        if Rc::strong_count(&callback) > 2 {
            return; // Don't allow recursion
        }
        ffi::lua_pushthread(L);
        let value = Thread((*extra).raw_lua().pop_ref_at(L), L);
        
        let main_th = (*extra).raw_lua().main_state();
        callback_error_ext(main_th, extra, move |extra| {
            let nargs = ffi::lua_gettop(L);
            let args = crate::traits::FromLuaMulti::from_specified_stack_multi(nargs, (*extra).raw_lua(), L)?;
            let thread_status = match status {
                ffi::LUA_YIELD => crate::ThreadStatus::Resumable,
                ffi::LUA_OK => crate::ThreadStatus::Finished,
                _ => crate::ThreadStatus::Error,
            };
            callback((*extra).lua(), value, thread_status, args)
        });
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_tuple("Thread").field(&self.0).finish()
    }
}

impl PartialEq for Thread {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl LuaType for Thread {
    const TYPE_ID: c_int = ffi::LUA_TTHREAD;
}

#[cfg(test)]
mod assertions {
    use super::*;

    static_assertions::assert_not_impl_any!(Thread: Send);
}
