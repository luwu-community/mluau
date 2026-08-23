use crate::{CallbackFinalizeAction, IntoCallbackResult};
use crate::buffer::{ExternalBuffer, ExternalBufferMut};
use crate::chunk::{AsChunk, Chunk};
use crate::debug::Debug;
use crate::error::{Error, Result};
use crate::function::Function;
use crate::luau::RESTRICTED_FFLAGS;
use crate::memory::{DefaultAllocator, MemoryState};
use crate::multi::MultiValue;
use crate::state::extra::USERDATA2_TAG;
use crate::state::util::push_callback_error;
use crate::stdlib::StdLib;
use crate::string::String;
use crate::table::Table;
use crate::thread::Thread;
use crate::userdata::{AnyUserData, assert_ud_tag};
use std::cell::{BorrowError, BorrowMutError};
use std::ffi::CStr;

use std::ops::Deref;
use std::os::raw::{c_char, c_int};
use std::panic::Location;
use std::result::Result as StdResult;
use std::string::String as StdString;
use std::{fmt, mem, ptr};

use crate::thread::ContinuationStatus;

use crate::traits::{FromLua, FromLuaMulti, IntoLua, IntoLuaMulti};
use crate::types::{
    AppDataRef, AppDataRefMut, Integer, LuaType, Number,
    VmState
};
use std::rc::Rc as XRc;
use std::rc::Weak as XWeak;
use crate::util::{assert_stack, check_stack, protect_lua_closure, StackGuard};
use crate::value::{Nil, Value};

use crate::types::ErasedHeader;
use crate::{buffer::Buffer, chunk::Compiler};

use std::ffi::c_void;



pub(crate) use extra::ExtraData;
pub use raw::RawLua;
pub(crate) use util::callback_error_ext;

/// Top level Lua struct which represents an instance of Lua VM.
pub struct Lua {
    pub(self) raw: XRc<RawLua>,
    // Controls whether garbage collection should be run on drop
    pub(self) collect_garbage: bool,
}

/// Weak reference to Lua instance.
///
/// This can used to prevent circular references between Lua and Rust objects.
#[derive(Clone)]
pub struct WeakLua(XWeak<RawLua>);

pub(crate) struct LuaGuard(pub(crate) XRc<RawLua>);

/// Mode of the Lua garbage collector (GC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GCMode {
    Incremental,
}

impl Drop for Lua {
    fn drop(&mut self) {
        if self.collect_garbage {
            let _ = self.gc_collect();
        }
    }
}

impl Clone for Lua {
    #[inline]
    fn clone(&self) -> Self {
        Lua {
            raw: XRc::clone(&self.raw),
            collect_garbage: false,
        }
    }
}

impl fmt::Debug for Lua {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Lua({:p})", self.lock().state())
    }
}

impl Default for Lua {
    #[inline]
    fn default() -> Self {
        Lua::new()
    }
}

impl Lua {
    /// Creates a new Lua state and loads the **safe** subset of the standard libraries.
    ///
    /// # Safety
    /// The created Lua state will have _some_ safety guarantees and will not allow to load unsafe
    /// standard libraries or C modules.
    ///
    /// See [`StdLib`] documentation for a list of unsafe modules that cannot be loaded.
    pub fn new() -> Lua {
        mlua_expect!(
            Self::new_with(StdLib::ALL_SAFE),
            "Cannot create a Lua state"
        )
    }

    /// Creates a new Lua state and loads all the standard libraries.
    ///
    /// # Safety
    /// The created Lua state will not have safety guarantees and will allow to load C modules.
    pub unsafe fn unsafe_new() -> Lua {
        Self::unsafe_new_with(StdLib::ALL)
    }

    /// Creates a new Lua state with a custom allocator.
    pub fn new_with_allocator(libs: StdLib, allocator: Box<dyn crate::memory::LuaAllocator>) -> Result<Lua> {
        let lua = unsafe { Self::inner_new_with_allocator(libs, allocator) };
        lua.lock().mark_safe();
        Ok(lua)
    }

    #[inline]
    unsafe fn inner_new_with_allocator(libs: StdLib, allocator: Box<dyn crate::memory::LuaAllocator>) -> Lua {
        let lua = Lua {
            raw: RawLua::new(libs, allocator),
            collect_garbage: true,
        };
        mlua_expect!(lua.configure_luau(), "Error configuring Luau");
        lua
    }

    /// Creates a new Lua state and loads the specified safe subset of the standard libraries.
    ///
    /// Use the [`StdLib`] flags to specify the libraries you want to load.
    ///
    /// # Safety
    /// The created Lua state will have _some_ safety guarantees and will not allow to load unsafe
    /// standard libraries or C modules.
    ///
    /// See [`StdLib`] documentation for a list of unsafe modules that cannot be loaded.
    pub fn new_with(libs: StdLib) -> Result<Lua> {
        let lua = unsafe { Self::inner_new_with_allocator(libs, Box::new(DefaultAllocator::default())) };

        lua.lock().mark_safe();

        Ok(lua)
    }

    /// Same as `new_with` but sets a memory_limit of `size` bytes
    pub fn new_with_memory_limit(libs: StdLib, size: isize) -> Result<Lua> {
        let lua = unsafe { Self::inner_new_with_allocator(libs, Box::new(DefaultAllocator {
            memory_limit: size,
            ..Default::default()
        })) };

        lua.lock().mark_safe();

        Ok(lua)
    }

    /// Creates a new Lua state and loads the specified subset of the standard libraries.
    ///
    /// Use the [`StdLib`] flags to specify the libraries you want to load.
    ///
    /// # Safety
    /// The created Lua state will not have safety guarantees and allow to load C modules.
    pub unsafe fn unsafe_new_with(libs: StdLib) -> Lua {
        // Workaround to avoid stripping a few unused Lua symbols that could be imported
        // by C modules in unsafe mode
        let mut _symbols: Vec<*const extern "C-unwind" fn()> =
            vec![ffi::lua_isuserdata as _, ffi::lua_tocfunction as _];

        Self::inner_new_with_allocator(libs, Box::new(DefaultAllocator::default()))
    }

    /// Sets a memory limit (in bytes) on this Lua state.
    ///
    /// Once an allocation occurs that would pass this memory limit, a `Error::MemoryError` is
    /// generated instead.
    /// Returns previous limit (zero means no limit).
    ///
    /// May be a no-op if the allocator does not support this
    pub fn set_memory_limit(&self, limit: isize) -> Result<isize> {
        let lua = self.lock();
        unsafe {
            match MemoryState::get(lua.state()) {
                mem_state if !mem_state.is_null() => Ok((*mem_state).set_memory_limit(limit)),
                _ => Err(Error::MemoryControlNotAvailable),
            }
        }
    }

    /// Returns or constructs Lua instance from a raw state.
    ///
    /// Once initialized, the returned Lua instance is cached in the registry and can be retrieved
    /// by calling this function again.
    ///
    /// # Safety
    /// The `Lua` must outlive the chosen lifetime `'a`.
    #[inline]
    pub unsafe fn get_or_init_from_ptr<'a>(state: *mut ffi::lua_State) -> &'a Lua {
        debug_assert!(!state.is_null(), "Lua state is null");
        match ExtraData::get(state) {
            extra if !extra.is_null() => (*extra).lua(),
            _ => {
                // The `owned` flag is set to `false` as we don't own the Lua state.
                RawLua::init_from_ptr(state, false);
                (*ExtraData::get(state)).lua()
            }
        }
    }

    /// Calls provided function passing a raw lua state.
    ///
    /// The arguments will be pushed onto the stack before calling the function.
    ///
    /// This method ensures that the Lua instance is locked while the function is called
    /// and restores Lua stack after the function returns.
    ///
    /// # Example
    /// ```
    /// # use mluau::{Lua, Result};
    /// # fn main() -> Result<()> {
    /// let lua = Lua::new();
    /// let n: i32 = unsafe {
    ///     let nums = (3, 4, 5);
    ///     lua.exec_raw(nums, |state| {
    ///         let n = ffi::lua_gettop(state);
    ///         let mut sum = 0;
    ///         for i in 1..=n {
    ///             sum += ffi::lua_tointeger(state, i);
    ///         }
    ///         ffi::lua_pop(state, n);
    ///         ffi::lua_pushinteger(state, sum);
    ///     })
    /// }?;
    /// assert_eq!(n, 12);
    /// # Ok(())
    /// # }
    /// ```
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn exec_raw<R: FromLuaMulti>(
        &self,
        args: impl IntoLuaMulti,
        f: impl FnOnce(*mut ffi::lua_State),
    ) -> Result<R> {
        let lua = self.lock();
        let state = lua.state();
        let _sg = StackGuard::new(state);
        let stack_start = ffi::lua_gettop(state);
        let nargs = args.push_into_specified_stack_multi(&lua, state)?;
        check_stack(state, 3)?;
        protect_lua_closure::<_, ()>(state, nargs, ffi::LUA_MULTRET, f)?;
        let nresults = ffi::lua_gettop(state) - stack_start;
        R::from_specified_stack_multi(nresults, &lua, state)
    }

    /// Loads the specified subset of the standard libraries into an existing Lua state.
    ///
    /// Use the [`StdLib`] flags to specify the libraries you want to load.
    pub fn load_std_libs(&self, libs: StdLib) -> Result<()> {
        unsafe { self.lock().load_std_libs(libs) }
    }

    /// Registers module into an existing Lua state using the specified value.
    ///
    /// After registration, the given value will always be immediately returned when the
    /// given module is [required].
    ///
    /// [required]: https://www.lua.org/manual/5.4/manual.html#pdf-require
    pub fn register_module(&self, modname: &str, value: impl IntoLua) -> Result<()> {
        const LOADED_MODULES_KEY: *const c_char = ffi::LUA_REGISTERED_MODULES_TABLE;

        if !modname.starts_with('@') {
            return Err(Error::runtime("module name must begin with '@'"));
        }

        let modname = modname.to_ascii_lowercase();
        unsafe {
            self.exec_raw::<()>(value, |state| {
                ffi::luaL_getsubtable(state, ffi::LUA_REGISTRYINDEX, LOADED_MODULES_KEY);
                ffi::lua_pushlstring(state, modname.as_ptr() as *const c_char, modname.len() as _);
                ffi::lua_pushvalue(state, -3);
                ffi::lua_rawset(state, -3);
            })
        }
    }

    /// Unloads module `modname`.
    ///
    /// This method does not support unloading binary Lua modules since they are internally cached
    /// and can be unloaded only by closing Lua state.
    ///
    /// This is similar to calling [`Lua::register_module`] with `Nil` value.
    ///
    /// [`package.loaded`]: https://www.lua.org/manual/5.4/manual.html#pdf-package.loaded
    pub fn unload_module(&self, modname: &str) -> Result<()> {
        self.register_module(modname, Nil)
    }

    /// Enables (or disables) sandbox mode on this Lua instance.
    ///
    /// This method, in particular:
    /// - Set all libraries to read-only
    /// - Set all builtin metatables to read-only
    /// - Set globals to read-only (and activates safeenv)
    /// - Setup local environment table that performs writes locally and proxies reads to the global
    ///   environment.
    /// - Allow only `count` mode in `collectgarbage` function.
    ///
    /// # Examples
    ///
    /// ```
    /// # use mluau::{Lua, Result};
    /// #
    /// # fn main() -> Result<()> {
    /// let lua = Lua::new();
    ///
    /// lua.sandbox(true)?;
    /// lua.load("var = 123").call::<()>(())?;
    /// assert_eq!(lua.globals().get::<u32>("var")?, 123);
    ///
    /// // Restore the global environment (clear changes made in sandbox)
    /// lua.sandbox(false)?;
    /// assert_eq!(lua.globals().get::<Option<u32>>("var")?, None);
    /// # Ok(())
    /// # }
    ///
    /// ```
    pub fn sandbox(&self, enabled: bool) -> Result<()> {
        let lua = self.lock();
        unsafe {
            if (*lua.extra.get()).sandboxed != enabled {
                let state = lua.main_state();
                check_stack(state, 3)?;
                protect_lua!(state, 0, 0, |state| {
                    if enabled {
                        ffi::luaL_sandbox(state, 1);
                        ffi::luaL_sandboxthread(state);
                    } else {
                        // Restore original `LUA_GLOBALSINDEX`
                        ffi::lua_getrefpool(state, (*lua.extra.get()).original_globals_ref);
                        ffi::lua_replace(state, ffi::LUA_GLOBALSINDEX);
                        ffi::luaL_sandbox(state, 0);
                    }
                })?;
                (*lua.extra.get()).sandboxed = enabled;
            }
            Ok(())
        }
    }

    /// Sets an interrupt function that will periodically be called by Luau VM.
    ///
    /// Any Luau code is guaranteed to call this handler "eventually"
    /// (in practice this can happen at any function call or at any loop iteration).
    /// This is similar to `Lua::set_hook` but in more simplified form.
    ///
    /// The provided interrupt function can error, and this error will be propagated through
    /// the Luau code that was executing at the time the interrupt was triggered.
    /// Also this can be used to implement continuous execution limits by instructing Luau VM to
    /// yield by returning [`VmState::Yield`]. The yield will happen only at yieldable points
    /// of execution (not across metamethod/C-call boundaries).
    ///
    /// # Example
    ///
    /// Periodically yield Luau VM to suspend execution.
    ///
    /// ```
    /// # use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
    /// # use mluau::{Lua, Result, Error, ThreadStatus, VmState};
    /// #
    /// # fn main() -> Result<()> {
    /// let lua = Lua::new();
    /// let count = Arc::new(AtomicU64::new(0));
    /// lua.set_interrupt(move |_| {
    ///     if count.fetch_add(1, Ordering::Relaxed) % 2 == 0 {
    ///         return Ok(VmState::Yield);
    ///     }
    ///     Ok(VmState::Continue)
    /// });
    ///
    /// let co = lua.create_thread(
    ///     lua.load(r#"
    ///         local b = 0
    ///         for _, x in ipairs({1, 2, 3}) do b += x end
    ///     "#)
    ///     .into_function()?,
    /// )?;
    /// while co.status() == ThreadStatus::Resumable {
    ///     co.resume::<(), Error>(())?;
    /// }
    /// # Ok(())
    /// # }
    ///
    /// ```
    pub fn set_interrupt<F>(&self, callback: F)
    where
        F: Fn(&Lua) -> Result<VmState> + 'static,
    {
        unsafe extern "C-unwind" fn interrupt_proc(state: *mut ffi::lua_State, gc: c_int) {
            if gc >= 0 {
                // GC interrupts cannot survive Lua exceptions and hence abort if they throw
                let extra = ExtraData::get(state);
                if let Some(callback) = &(*extra).gc_interrupt_callback {
                    use std::panic::{catch_unwind, AssertUnwindSafe};
                    use std::process::abort;

                    if XRc::strong_count(&callback) > 2 {
                        return; // Don't allow recursion
                    }
                    (*extra).running_gc = true;
                    match catch_unwind(AssertUnwindSafe(|| (callback)((*extra).lua(), gc))) {
                        Ok(_) => {}
                        Err(_) => abort(),
                    };
                    (*extra).running_gc = false;
                }
                return;
            }
            let result = callback_error_ext(state, ptr::null_mut(), move |extra| {
                let interrupt_cb = (*extra).interrupt_callback.clone();
                let interrupt_cb = mlua_expect!(interrupt_cb, "no interrupt callback set in interrupt_proc");
                if XRc::strong_count(&interrupt_cb) > 2 {
                    return Ok(VmState::Continue); // Don't allow recursion
                }
                interrupt_cb((*extra).lua())
            });
            match result {
                VmState::Continue => {}
                VmState::Yield => {
                    // We can yield only at yieldable points, otherwise ignore and continue
                    if ffi::lua_isyieldable(state) != 0 {
                        ffi::lua_yield(state, 0);
                    }
                }
            }
        }

        // Set interrupt callback
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).interrupt_callback = Some(XRc::new(callback));
            (*ffi::lua_callbacks(lua.main_state())).interrupt = Some(interrupt_proc);
        }
    }

    /// Removes any interrupt function previously set by `set_interrupt`.
    ///
    /// This function has no effect if an 'interrupt' was not previously set.
    pub fn remove_interrupt(&self) {
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).interrupt_callback = None;
            (*ffi::lua_callbacks(lua.main_state())).interrupt = None;
        }
    }

    /// Sets a GC interrupt callback
    ///
    /// Unlike a normal interrupt, a GC interrupt callback cannot panic
    /// and may not perform any VM operations besides inspect_stack (outside of Debug::function)
    /// and setting/removing interrupts, thread callbacks and yield arguments/check if yieldable
    ///
    /// Does not do anything if a normal interrupt callback is not set first
    pub fn set_gc_interrupt<F>(&self, callback: F)
    where
        F: Fn(&Lua, c_int) + 'static,
    {
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).gc_interrupt_callback = Some(XRc::new(callback));
        }
    }

    /// Removes any GC interrupt callback previously set by `set_gc_interrupt`.
    pub fn remove_gc_interrupt(&self) {
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).gc_interrupt_callback = None;
        }
    }

    /// Sets a thread creation callback that will be called when a thread is created.
    pub fn set_thread_creation_callback<F>(&self, callback: F)
    where
        F: Fn(&Lua, Thread) -> Result<()> + 'static,
    {
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).thread_creation_callback = Some(XRc::new(callback));
            (*ffi::lua_callbacks(lua.main_state())).userthread = Some(Self::userthread_proc);
        }
    }

    /// Sets a thread collection callback that will be called when a thread is destroyed.
    ///
    /// Luau GC does not support exceptions during collection, so the callback must be
    /// non-panicking. If the callback panics, the program will be aborted.
    pub fn set_thread_collection_callback<F>(&self, callback: F)
    where
        F: Fn(crate::LightUserData) + 'static,
    {
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).thread_collection_callback = Some(XRc::new(callback));
            (*ffi::lua_callbacks(lua.main_state())).userthread = Some(Self::userthread_proc);
        }
    }
    /// Sets a thread state change callback that will be called when a attached thread suspends or completes.
    pub fn set_thread_state_change_callback<F>(&self, callback: F)
    where
        F: Fn(&Lua, Thread, crate::ThreadStatus, MultiValue) -> Result<()> + 'static,
    {
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).thread_state_change_callback = Some(XRc::new(callback));
        }
    }

    pub(crate) unsafe extern "C-unwind" fn userthread_proc(
        parent: *mut ffi::lua_State,
        child: *mut ffi::lua_State,
    ) {
        let extra = ExtraData::get(child);
        if !parent.is_null() {
            // Thread is created
            let callback = match (*extra).thread_creation_callback {
                Some(ref cb) => cb.clone(),
                None => return,
            };
            if XRc::strong_count(&callback) > 2 {
                return; // Don't allow recursion
            }
            ffi::lua_pushthread(child);
            let value = Thread((*extra).raw_lua().pop_ref_at(child), child);
            callback_error_ext(parent, extra, move |extra| {
                callback((*extra).lua(), value)
            })
        } else {
            // Thread is about to be collected

            let tdp = ffi::lua_getthreaddata(child);
            if !tdp.is_null() {
                ffi::lua_setthreaddata(child, ptr::null_mut());
                // We need to wrap the threaddata drop in non-unwind function as it's not safe to unwind when
                // Luau GC is running.
                // This will trigger `abort()` if dropping ThreadData panics.
                unsafe extern "C" fn drop_threaddata(tdp: *mut c_void) {
                    ErasedHeader::drop(tdp);
                }

                (*extra).running_gc = true;
                drop_threaddata(tdp);
                (*extra).running_gc = false;
            }

            let callback = match (*extra).thread_collection_callback {
                Some(ref cb) => cb.clone(),
                None => return,
            };

            // We need to wrap the callback call in non-unwind function as it's not safe to unwind when
            // Luau GC is running.
            // This will trigger `abort()` if the callback panics.
            unsafe extern "C" fn run_callback(
                callback: *const crate::types::ThreadCollectionCallback,
                value: *mut ffi::lua_State,
            ) {
                (*callback)(crate::LightUserData(value as _));
            }

            (*extra).running_gc = true;
            run_callback(&callback, child);
            (*extra).running_gc = false;
        }
    }

    /// Removes the thread state change callback
    /// [`Lua::set_thread_state_change_callback`].
    pub fn remove_thread_state_change_callback(&self) {
        let lua = self.lock();
        unsafe {
            let extra = lua.extra.get();
            (*extra).thread_state_change_callback = None;
        }
    }

    /// Removes any thread creation, collection, or state change callbacks previously set by
    /// [`Lua::set_thread_creation_callback`], [`Lua::set_thread_collection_callback`], or [`Lua::set_thread_state_change_callback`].
    ///
    /// This function has no effect if a thread callbacks were not previously set or if
    /// thread data was ever used.
    pub fn remove_thread_callbacks(&self) {
        let lua = self.lock();
        unsafe {
            let extra = lua.extra.get();
            (*extra).thread_creation_callback = None;
            (*extra).thread_collection_callback = None;
            (*extra).thread_state_change_callback = None;
            if !(*extra).have_thread_data {
                (*ffi::lua_callbacks(lua.main_state())).userthread = None;
            }
        }
    }

    /// Gets information about the interpreter runtime stack at the given level.
    ///
    /// This function calls callback `f`, passing the [`Debug`] structure that can be used to get
    /// information about the function executing at a given level.
    /// Level `0` is the current running function, whereas level `n+1` is the function that has
    /// called level `n` (except for tail calls, which do not count in the stack).
    pub fn inspect_stack<R>(&self, level: usize, f: impl FnOnce(&Debug) -> R) -> Option<R> {
        let lua = &self.raw;
        unsafe {
            let mut ar = mem::zeroed::<ffi::lua_Debug>();
            let level = level as c_int;

            if ffi::lua_getinfo(lua.state(), level, cstr!(""), &mut ar) == 0 {
                return None;
            }

            Some(f(&Debug::new(&lua, level, &mut ar)))
        }
    }

    /// Creates a traceback of the call stack at the given level.
    ///
    /// The `msg` parameter, if provided, is added at the beginning of the traceback.
    /// The `level` parameter works the same way as in [`Lua::inspect_stack`].
    pub fn traceback(&self, msg: Option<&str>, level: usize) -> Result<String> {
        let lua = self.lock();
        unsafe {
            check_stack(lua.state(), 3)?;
            protect_lua!(lua.state(), 0, 1, |state| {
                let msg = match msg {
                    Some(s) => ffi::lua_pushlstring(state, s.as_ptr() as *const c_char, s.len()),
                    None => ptr::null(),
                };
                // `protect_lua` adds it's own call frame, so we need to increase level by 1
                ffi::luaL_traceback(state, state, msg, (level + 1) as c_int);
            })?;
            Ok(String(lua.pop_ref()))
        }
    }

    /// Returns the amount of memory (in bytes) currently used inside this Lua state.
    pub fn used_memory(&self) -> isize {
        let lua = self.lock();
        let state = lua.main_state();
        unsafe {
            match MemoryState::get(state) {
                mem_state if !mem_state.is_null() => (*mem_state).used_memory(),
                _ => {
                    // Get data from the Lua GC
                    let used_kbytes = ffi::lua_gc(state, ffi::LUA_GCCOUNT, 0);
                    let used_kbytes_rem = ffi::lua_gc(state, ffi::LUA_GCCOUNTB, 0);
                    (used_kbytes as isize) * 1024 + (used_kbytes_rem as isize)
                }
            }
        }
    }

    /// Returns the current memory limit of the Lua VM (zero means no limit)
    pub fn memory_limit(&self) -> isize {
        let lua = self.lock();
        unsafe {
            match MemoryState::get(lua.state()) {
                mem_state if !mem_state.is_null() => (*mem_state).memory_limit(),
                _ => 0,
            }
        }
    }

    /// Returns `true` if the garbage collector is currently running automatically.

    pub fn gc_is_running(&self) -> bool {
        let lua = self.lock();
        unsafe { ffi::lua_gc(lua.main_state(), ffi::LUA_GCISRUNNING, 0) != 0 }
    }

    /// Stop the Lua GC from running
    pub fn gc_stop(&self) {
        let lua = self.lock();
        unsafe { ffi::lua_gc(lua.main_state(), ffi::LUA_GCSTOP, 0) };
    }

    /// Restarts the Lua GC if it is not running
    pub fn gc_restart(&self) {
        let lua = self.lock();
        unsafe { ffi::lua_gc(lua.main_state(), ffi::LUA_GCRESTART, 0) };
    }

    /// Perform a full garbage-collection cycle.
    ///
    /// It may be necessary to call this function twice to collect all currently unreachable
    /// objects. Once to finish the current gc cycle, and once to start and finish the next cycle.
    pub fn gc_collect(&self) -> Result<()> {
        let lua = self.lock();
        let state = lua.main_state();
        unsafe {
            check_stack(state, 2)?;
            protect_lua!(state, 0, 0, fn(state) ffi::lua_gc(state, ffi::LUA_GCCOLLECT, 0))
        }
    }

    /// Steps the garbage collector one indivisible step.
    ///
    /// Returns `true` if this has finished a collection cycle.
    pub fn gc_step(&self) -> Result<bool> {
        self.gc_step_kbytes(0)
    }

    /// Steps the garbage collector as though memory had been allocated.
    ///
    /// if `kbytes` is 0, then this is the same as calling `gc_step`. Returns true if this step has
    /// finished a collection cycle.
    pub fn gc_step_kbytes(&self, kbytes: c_int) -> Result<bool> {
        let lua = self.lock();
        let state = lua.main_state();
        unsafe {
            check_stack(state, 3)?;
            protect_lua!(state, 0, 0, |state| {
                ffi::lua_gc(state, ffi::LUA_GCSTEP, kbytes) != 0
            })
        }
    }

    /// Sets the `pause` value of the collector.
    ///
    /// Returns the previous value of `pause`. More information can be found in the Lua
    /// [documentation].
    ///
    /// For Luau this parameter sets GC goal
    ///
    /// [documentation]: https://www.lua.org/manual/5.4/manual.html#2.5
    pub fn gc_set_pause(&self, pause: c_int) -> c_int {
        let lua = self.lock();
        let state = lua.main_state();
        unsafe {
            return ffi::lua_gc(state, ffi::LUA_GCSETGOAL, pause);
        }
    }

    /// Sets the `step multiplier` value of the collector.
    ///
    /// Returns the previous value of the `step multiplier`. More information can be found in the
    /// Lua [documentation].
    ///
    /// [documentation]: https://www.lua.org/manual/5.4/manual.html#2.5
    pub fn gc_set_step_multiplier(&self, step_multiplier: c_int) -> c_int {
        let lua = self.lock();
        unsafe { ffi::lua_gc(lua.main_state(), ffi::LUA_GCSETSTEPMUL, step_multiplier) }
    }

    /// Changes the collector to incremental mode with the given parameters.
    ///
    /// Returns the previous mode (always `GCMode::Incremental` in Lua < 5.4).
    /// More information can be found in the Lua [documentation].
    ///
    /// [documentation]: https://www.lua.org/manual/5.4/manual.html#2.5.1
    pub fn gc_inc(&self, pause: c_int, step_multiplier: c_int, step_size: c_int) -> GCMode {
        let lua = self.lock();
        let state = lua.main_state();

        unsafe {
            if pause > 0 {
                ffi::lua_gc(state, ffi::LUA_GCSETGOAL, pause);
            }

            if step_multiplier > 0 {
                ffi::lua_gc(state, ffi::LUA_GCSETSTEPMUL, step_multiplier);
            }

            if step_size > 0 {
                ffi::lua_gc(state, ffi::LUA_GCSETSTEPSIZE, step_size);
            }

            GCMode::Incremental
        }
    }

    /// Sets a default Luau compiler (with custom options).
    ///
    /// This compiler will be used by default to load all Lua chunks
    /// including via `require` function.
    ///
    /// See [`Compiler`] for details and possible options.
    pub fn set_compiler(&self, compiler: Compiler) {
        let lua = self.lock();
        unsafe { (*lua.extra.get()).compiler = Some(compiler) };
    }

    /// Toggles JIT compilation mode for new chunks of code.
    ///
    /// By default JIT is enabled. Changing this option does not have any effect on
    /// already loaded functions.
    #[cfg(any(feature = "luau-jit", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "luau-jit")))]
    pub fn enable_jit(&self, enable: bool) {
        let lua = self.lock();
        unsafe { (*lua.extra.get()).enable_jit = enable };
    }

    /// Sets Luau feature flag (global setting).
    ///
    /// See https://github.com/luau-lang/luau/blob/master/CONTRIBUTING.md#feature-flags for details.
    #[allow(clippy::result_unit_err)]
    pub fn set_fflag(name: &str, enabled: bool) -> StdResult<(), ()> {
        if RESTRICTED_FFLAGS.contains(&name) {
            return Err(()); // fflag is restricted and cannot be set for soundness reasons
        }

        Self::set_fflag_inner(name, enabled)
    }

    #[allow(clippy::result_unit_err)]
    #[inline(always)]
    pub(crate) fn set_fflag_inner(name: &str, enabled: bool) -> StdResult<(), ()> {
        if let Ok(cname) = std::ffi::CString::new(name) {
            if unsafe { ffi::luau_setfflag(cname.as_ptr(), enabled as c_int) != 0 } {
                return Ok(());
            }
        }
        Err(())
    }

    /// Returns Luau source code as a `Chunk` builder type.
    ///
    /// `chunk` should be created via [`ChunkSource`] to prevent ambiguity of whether
    /// the passed chunk is Luau sourcecode or Luau bytecode.
    /// Passing a String or similar utf-8 content (obviously source code) as `chunk` is also
    /// allowed.
    ///
    /// In order to actually compile or run the resulting code, you must call [`Chunk::exec`] or
    /// similar on the returned builder. Code is not even parsed until one of these methods is
    /// called.
    ///
    /// [`ChunkSource`]: crate::ChunkSource
    /// [`Chunk::exec`]: crate::Chunk::exec
    #[track_caller]
    pub fn load<'a>(&self, chunk: impl AsChunk + 'a) -> Chunk<'a> {
        self.load_with_location(chunk, Location::caller())
    }

    pub(crate) fn load_with_location<'a>(
        &self,
        chunk: impl AsChunk + 'a,
        location: &'static Location<'static>,
    ) -> Chunk<'a> {
        Chunk {
            lua: self.weak(),
            name: chunk
                .name()
                .unwrap_or_else(|| format!("@{}:{}", location.file(), location.line())),
            env: chunk.environment(self),
            mode: chunk.mode(),
            source: chunk.source(),
            compiler: unsafe { (*self.lock().extra.get()).compiler.clone() },
            trusted_binary: false,
        }
    }

    /// Creates and returns an interned Lua string.
    ///
    /// Lua strings can be arbitrary `[u8]` data including embedded nulls, so in addition to `&str`
    /// and `&String`, you can also pass plain `&[u8]` here.
    #[inline]
    pub fn create_string(&self, s: impl AsRef<[u8]>) -> Result<String> {
        unsafe { self.lock().create_string(s.as_ref()) }
    }

    /// Creates and returns an externally managed Luau string.
    ///
    /// External strings do not allocate memory inside the Luau VM for their contents.
    /// Instead, the data is held in an external backing store like `Vec<u8>` or `bytes::Bytes`.
    /// The string MUST be null-terminated to ensure safety within the Luau VM.
    #[inline]
    pub fn create_external_string<S: crate::string::ExternalString>(&self, s: S) -> Result<String> {
        unsafe { self.lock().create_external_string(s) }
    }

    /// Creates and returns a Luau [buffer] object from a byte slice of data.
    ///
    /// [buffer]: https://luau.org/library#buffer-library
    pub fn create_buffer(&self, data: impl AsRef<[u8]>) -> Result<Buffer> {
        let lua = self.lock();
        let data = data.as_ref();
        unsafe {
            let (ptr, buffer) = lua.create_buffer_with_capacity(data.len())?;
            ptr.copy_from_nonoverlapping(data.as_ptr(), data.len());
            Ok(buffer)
        }
    }

    /// Helper function to create an externally managed buffer with a specific mode.
    ///
    /// # Safety
    /// The caller must ensure that the `mode` parameter accurately reflects the mutability
    /// guarantees of the provided `buffer` type. Specifically, if `mode` is `LUA_BHOST_MUTABLE`,
    /// the `buffer` must actually support safe mutable access to its backing store
    /// (e.g., implementing `ExternalBufferMut`).
    unsafe fn create_external_buffer_with_mode<B: ExternalBuffer>(
        &self,
        buffer: B,
        data: *mut u8,
        mode: std::os::raw::c_int,
    ) -> Result<Buffer> {
        let size = buffer.len();
        let userdata = ErasedHeader::into_raw(buffer);
        unsafe {
            let state = self.lock();
            Ok(state
                .create_external_buffer(size, data, userdata, Some(buffer_free_cb), mode)?
                .1)
        }
    }

    /// Creates and returns an externally managed immutable Luau [buffer] object.
    ///
    /// [buffer]: https://luau.org/library#buffer-library
    pub fn create_external_buffer<B: ExternalBuffer>(&self, buffer: B) -> Result<Buffer> {
        let data = buffer.as_ptr() as *mut u8;
        unsafe { self.create_external_buffer_with_mode(buffer, data, ffi::LUA_BHOST_IMMUTABLE) }
    }

    /// Creates and returns an externally managed mutable Luau [buffer] object.
    ///
    /// [buffer]: https://luau.org/library#buffer-library
    pub fn create_external_buffer_mut<B: ExternalBufferMut>(&self, mut buffer: B) -> Result<Buffer> {
        let data = buffer.as_mut_ptr();
        unsafe { self.create_external_buffer_with_mode(buffer, data, ffi::LUA_BHOST_MUTABLE) }
    }

    /// Creates and returns a Luau [buffer] object with the specified size.
    ///
    /// Size limit is 1GB. All bytes will be initialized to zero.
    ///
    /// [buffer]: https://luau.org/library#buffer-library
    pub fn create_buffer_with_capacity(&self, size: usize) -> Result<Buffer> {
        unsafe { Ok(self.lock().create_buffer_with_capacity(size)?.1) }
    }

    /// Creates and returns a new empty table.
    #[inline]
    pub fn create_table(&self) -> Result<Table> {
        self.create_table_with_capacity(0, 0)
    }

    /// Creates and returns a new empty table, with the specified capacity.
    ///
    /// - `narr` is a hint for how many elements the table will have as a sequence.
    /// - `nrec` is a hint for how many other elements the table will have.
    ///
    /// Lua may use these hints to preallocate memory for the new table.
    pub fn create_table_with_capacity(&self, narr: usize, nrec: usize) -> Result<Table> {
        unsafe { self.lock().create_table_with_capacity(narr, nrec) }
    }

    /// Creates a table and fills it with values from an iterator.
    pub fn create_table_from<K, V>(&self, iter: impl IntoIterator<Item = (K, V)>) -> Result<Table>
    where
        K: IntoLua,
        V: IntoLua,
    {
        unsafe { self.lock().create_table_from(iter) }
    }

    /// Creates a table from an iterator of values, using `1..` as the keys.
    pub fn create_sequence_from<T>(&self, iter: impl IntoIterator<Item = T>) -> Result<Table>
    where
        T: IntoLua,
    {
        unsafe { self.lock().create_sequence_from(iter) }
    }

    /// Wraps a Rust function or closure, creating a callable Lua function handle to it.
    ///
    /// The function's return value is always a `Result`: If the function returns `Err`, the error
    /// is raised as a Lua error, which can be caught using `(x)pcall` or bubble up to the Rust code
    /// that invoked the Lua code. This allows using the `?` operator to propagate errors through
    /// intermediate Lua code.
    ///
    /// If the function returns `Ok`, the contained value will be converted to one or more Lua
    /// values. For details on Rust-to-Lua conversions, refer to the [`IntoLua`] and
    /// [`IntoLuaMulti`] traits.
    ///
    /// # Examples
    ///
    /// Create a function which prints its argument:
    ///
    /// ```
    /// # use mluau::{Lua, Result};
    /// # fn main() -> Result<()> {
    /// # let lua = Lua::new();
    /// let greet = lua.create_function(|_, name: String| {
    ///     println!("Hello, {}!", name);
    ///     Ok::<_, mluau::Error>(())
    /// });
    /// # let _ = greet;    // used
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Use tuples to accept multiple arguments:
    ///
    /// ```
    /// # use mluau::{Lua, Result};
    /// # fn main() -> Result<()> {
    /// # let lua = Lua::new();
    /// let print_person = lua.create_function(|_, (name, age): (String, u8)| {
    ///     println!("{} is {} years old!", name, age);
    ///     Ok::<_, mluau::Error>(())
    /// });
    /// # let _ = print_person;    // used
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_function<F, A, R>(&self, func: F) -> Result<Function>
    where
        F: Fn(&Lua, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult,
    {
        (self.lock()).create_callback(
            Box::new(move |rawlua, nargs| unsafe {
                let state = rawlua.state();
                let args = match A::from_specified_stack_args(nargs, 1, None, rawlua, state) {
                    Ok(a) => a,
                    Err(e) => {
                        push_callback_error(state, rawlua.extra(), e.to_string());
                        return CallbackFinalizeAction::Error
                    }
                };
                func(rawlua.lua(), args).finalize(rawlua, state)
            }),
            std::ptr::null(),
        )
    }

    /// Same as ``create_function`` but with an added continuation function.
    ///
    /// The values passed to the continuation will be the yielded arguments
    /// from the function for the initial continuation call. If yielding from a
    /// continuation, the yielded results will be returned to the ``Thread::resume`` caller. The
    /// arguments passed in the next ``Thread::resume`` call will then be the arguments passed
    /// to the yielding continuation upon resumption.
    ///
    /// Returning a value from a continuation without setting yield
    /// arguments will then be returned as the final return value of the Lua function call.
    /// Values returned in a function in which there is also yielding will be ignored
    pub fn create_function_with_continuation<F, FC, A, AC, R, RC>(
        &self,
        func: F,
        cont: FC,
        debugname: Option<&'static CStr>,
    ) -> Result<Function>
    where
        F: Fn(&Lua, A) -> R + 'static,
        FC: Fn(&Lua, ContinuationStatus, AC) -> RC + 'static,
        A: FromLuaMulti,
        AC: FromLuaMulti,
        R: IntoCallbackResult,
        RC: IntoCallbackResult,
    {
        (self.lock()).create_callback_with_continuation(
            Box::new(move |rawlua, nargs| unsafe {
                let state = rawlua.state();
                let args = match A::from_specified_stack_args(nargs, 1, None, rawlua, state) {
                    Ok(a) => a,
                    Err(e) => {
                        push_callback_error(state, rawlua.extra(), e.to_string());
                        return CallbackFinalizeAction::Error
                    }
                };
                func(rawlua.lua(), args).finalize(rawlua, state)
            }),
            Box::new(move |rawlua, nargs, status| unsafe {
                let state = rawlua.state();
                let args = match AC::from_specified_stack_args(nargs, 1, None, rawlua, state) {
                    Ok(a) => a,
                    Err(e) => {
                        push_callback_error(state, rawlua.extra(), e.to_string());
                        return CallbackFinalizeAction::Error
                    }
                };
                let status = ContinuationStatus::from_status(status);
                cont(rawlua.lua(), status, args).finalize(rawlua, state)
            }),
            match debugname {
                Some(s) => s.as_ptr(),
                None => std::ptr::null(),
            },
        )
    }

    /// Same as ``create_function`` but with an added ``debugname``
    pub fn create_function_with_debug<F, A, R>(
        &self,
        func: F,
        debugname: Option<&'static CStr>,
    ) -> Result<Function>
    where
        F: Fn(&Lua, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult,
    {
        (self.lock()).create_callback(
            Box::new(move |rawlua, nargs| unsafe {
                let state = rawlua.state();
                let args = match A::from_specified_stack_args(nargs, 1, None, rawlua, state) {
                    Ok(a) => a,
                    Err(e) => {
                        push_callback_error(state, rawlua.extra(), e.to_string());
                        return CallbackFinalizeAction::Error
                    }
                };
                func(rawlua.lua(), args).finalize(rawlua, state)
            }),
            debugname.map(|x| x.as_ptr()).unwrap_or(std::ptr::null()),
        )
    }

    /// Wraps a C function, creating a callable Lua function handle to it.
    ///
    /// # Safety
    /// This function is unsafe because provides a way to execute unsafe C function.
    pub unsafe fn create_c_function(&self, func: ffi::lua_CFunction) -> Result<Function> {
        let lua = self.lock();
        // Lua <5.2 requires memory allocation to push a C function
        let state = lua.state();
        {
            let _sg = StackGuard::new(state);
            check_stack(state, 3)?;

            protect_lua!(state, 0, 1, |state| ffi::lua_pushcfunction(state, func))?;
            Ok(Function(lua.pop_ref()))
        }
    }

    /// Wraps a Lua function into a new thread (or coroutine).
    ///
    /// Equivalent to `coroutine.create`.
    pub fn create_thread(&self, func: Function) -> Result<Thread> {
        unsafe { self.lock().create_thread(&func) }
    }

    /// `create_any_userdata_with_tag` but with default tag
    pub fn create_any_userdata<T: 'static>(&self, data: T, metatable: Option<&Table>) -> Result<AnyUserData> {
        self.create_any_userdata_with_tag::<T, USERDATA2_TAG>(data, metatable)
    }

    /// Creates a userdata with data and optional metatable using the specified tag
    /// 
    /// # Safety
    /// 
    /// When used in a FFI context, the tag used here must not conflict with any FFI-defined tag
    pub fn create_any_userdata_with_tag<T: 'static, const TAG: c_int>(&self, data: T, metatable: Option<&Table>) -> Result<AnyUserData> {
        const { assert_ud_tag::<TAG>(); }

        let lua = self.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 3)?;

            // If not default tag, set dtor lazily
            if const { TAG != USERDATA2_TAG } {
                let is_registered = &(*lua.extra()).registered_tags[TAG as usize];
                if !is_registered.get() {
                    ExtraData::set_userdata_dtor(state, TAG);
                    is_registered.set(true);
                }
            }
                
            // Create ud_ptr, then use place_into_gc_memory to write data onto the GC allocated memory
            protect_lua!(state, 0, 1, |state| {
                let ud_ptr = ffi::lua_newuserdatatagged(state, crate::types::ErasedHeader::wrapper_size::<T>(), TAG);
                crate::types::ErasedHeader::place_into_gc_memory(ud_ptr, data);

                // Set metatable if we have one
                if let Some(mt) = metatable {
                    lua.push_ref_at(&mt.0, state);
                    ffi::lua_setmetatable(state, -2);
                }
            })?;
            Ok(AnyUserData(lua.pop_ref()))
        }
    }

    /// Reserve the specified userdata tag's dtor so `lua_findunuseduserdatatag` etc. can find the dtor
    pub fn reserve_userdata_tag<const TAG: c_int>(&self) {
        const { assert_ud_tag::<TAG>(); }
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            // If not default tag, set dtor lazily
            if const { TAG != USERDATA2_TAG } {
                let is_registered = &(*lua.extra()).registered_tags[TAG as usize];
                if !is_registered.get() {
                    ExtraData::set_userdata_dtor(state, TAG);
                    is_registered.set(true);
                }
            }
        }
    }

    /// Executes `f` with the CStr of the method name within a `__namecall` callback. 
    /// 
    /// For example: inside a `ud:method()` callback in Rust, this will execute f with a CStr of "method"
    pub fn with_namecall<R, F>(&self, f: F) -> R
    where
        F: for<'a> FnOnce(Option<&'a CStr>) -> R,
    {
        let lua = self.lock();
        let state = lua.state();
        
        let atom_slice = unsafe {
            let ptr = ffi::lua_namecallatom(state, std::ptr::null_mut());
            if ptr.is_null() {
                None
            } else {
                Some(std::ffi::CStr::from_ptr(ptr))
            }
        };
        
        f(atom_slice)
    }

    /// Create a Lua userdata "proxy" object from a custom userdata type.
    ///
    /// Proxy object is an empty userdata object that has `T` metatable attached.
    /// The main purpose of this object is to provide access to static fields and functions
    /// Gets the metatable of a Lua built-in (primitive) type.
    ///
    /// The metatable is shared by all values of the given type.
    ///
    /// See [`Lua::set_type_metatable`] for examples.
    #[allow(private_bounds)]
    pub fn type_metatable<T: LuaType>(&self) -> Option<Table> {
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 2);

            if lua.push_primitive_type::<T>(state) && ffi::lua_getmetatable(state, -1) != 0 {
                return Some(Table(lua.pop_ref_at(state)));
            }
        }
        None
    }

    /// Sets the metatable for a Lua built-in (primitive) type.
    ///
    /// The metatable will be shared by all values of the given type.
    ///
    /// # Examples
    ///
    /// Change metatable for Lua boolean type:
    ///
    /// ```
    /// # use mluau::{Lua, Result, Function};
    /// # fn main() -> Result<()> {
    /// # let lua = Lua::new();
    /// let mt = lua.create_table()?;
    /// mt.set("__tostring", lua.create_function(|_, b: bool| Ok::<_, mluau::Error>(if b { "2" } else { "0" }))?)?;
    /// lua.set_type_metatable::<bool>(Some(mt));
    /// lua.load("assert(tostring(true) == '2')").call::<()>(())?;
    /// # Ok(())
    /// # }
    /// ```
    #[allow(private_bounds)]
    pub fn set_type_metatable<T: LuaType>(&self, metatable: Option<Table>) {
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 2);

            if lua.push_primitive_type::<T>(state) {
                match metatable {
                    Some(metatable) => lua.push_ref_at(&metatable.0, state),
                    None => ffi::lua_pushnil(state),
                }
                ffi::lua_setmetatable(state, -2);
            }
        }
    }

    /// Returns a handle to the global environment.
    pub fn globals(&self) -> Table {
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 1);
            ffi::lua_pushvalue(state, ffi::LUA_GLOBALSINDEX);
            Table(lua.pop_ref())
        }
    }

    /// Returns a handle to the registry.
    pub fn registry(&self) -> Table {
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 1);
            ffi::lua_pushvalue(state, ffi::LUA_REGISTRYINDEX);
            Table(lua.pop_ref())
        }
    }

    /// A metatable attachable to a Lua table to systematically encode it as Array (instead of Map).
    /// As result, encoded Array will contain only sequence part of the table, with the same length
    /// as the `#` operator on that table.
    ///
    /// # Example
    ///
    /// ```
    /// use mluau::{Lua, Result};
    ///
    /// fn main() -> Result<()> {
    ///     let lua = Lua::new();
    ///     lua.globals().set("array_mt", lua.array_metatable())?;
    ///
    ///     Ok(())
    /// }
    /// ```
    pub fn array_metatable(&self) -> Table {
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 1);
            ffi::lua_getrefpool(state, (*lua.extra()).array_metatable_ref);
            Table(lua.pop_ref())
        }
    }

    /// Sets the global environment.
    ///
    /// This will replace the current global environment with the provided `globals` table.
    ///
    /// For Lua 5.2+ the globals table is stored in the registry and shared between all threads.
    /// For Lua 5.1 and Luau the globals table is stored in each thread.
    ///
    /// Please note that any existing Lua functions have cached global environment and will not
    /// see the changes made by this method.
    /// To update the environment for existing Lua functions, use [`Function::set_environment`].
    pub fn set_globals(&self, globals: Table) -> Result<()> {
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            if (*lua.extra.get()).sandboxed {
                return Err(Error::runtime("cannot change globals in a sandboxed Lua state"));
            }

            let _sg = StackGuard::new(state);
            check_stack(state, 1)?;

            lua.push_ref_at(&globals.0, state);
            ffi::lua_replace(state, ffi::LUA_GLOBALSINDEX);
        }

        Ok(())
    }

    /// Returns a handle to the active `Thread`.
    ///
    /// For calls to `Lua` this will be the main Lua thread, for parameters given to a callback,
    /// this will be whatever Lua thread called the callback.
    pub fn current_thread(&self) -> Thread {
        let lua = self.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 1);
            ffi::lua_pushthread(state);
            Thread(lua.pop_ref(), state)
        }
    }

    /// Attempts to coerce a Lua value into a String in a manner consistent with Lua's internal
    /// behavior.
    ///
    /// To succeed, the value must be a string (in which case this is a no-op), an integer, or a
    /// number.
    pub fn coerce_string(&self, v: Value) -> Result<Option<String>> {
        Ok(match v {
            Value::String(s) => Some(s),
            v => unsafe {
                let lua = self.lock();
                let state = lua.state();
                let _sg = StackGuard::new(state);
                check_stack(state, 4)?;

                lua.push_value_at(&v, state);
                let res = protect_lua!(state, 1, 1, |state| {
                    ffi::lua_tolstring(state, -1, ptr::null_mut())
                })?;
                if !res.is_null() {
                    Some(String(lua.pop_ref()))
                } else {
                    None
                }
            },
        })
    }

    /// Attempts to coerce a Lua value into an integer in a manner consistent with Lua's internal
    /// behavior.
    ///
    /// To succeed, the value must be an integer, a floating point number that has an exact
    /// representation as an integer, or a string that can be converted to an integer. Refer to the
    /// Lua manual for details.
    pub fn coerce_integer(&self, v: Value) -> Result<Option<Integer>> {
        Ok(match v {
            Value::Integer(i) => Some(i),
            v => unsafe {
                let lua = self.lock();
                let state = lua.state();
                let _sg = StackGuard::new(state);
                check_stack(state, 2)?;

                lua.push_value_at(&v, state);
                let mut isint = 0;
                let i = ffi::lua_tointegerx(state, -1, &mut isint);
                if isint == 0 {
                    None
                } else {
                    Some(i)
                }
            },
        })
    }

    /// Attempts to coerce a Lua value into a Number in a manner consistent with Lua's internal
    /// behavior.
    ///
    /// To succeed, the value must be a number or a string that can be converted to a number. Refer
    /// to the Lua manual for details.
    pub fn coerce_number(&self, v: Value) -> Result<Option<Number>> {
        Ok(match v {
            Value::Number(n) => Some(n),
            v => unsafe {
                let lua = self.lock();
                let state = lua.state();
                let _sg = StackGuard::new(state);
                check_stack(state, 2)?;

                lua.push_value_at(&v, state);
                let mut isnum = 0;
                let n = ffi::lua_tonumberx(state, -1, &mut isnum);
                if isnum == 0 {
                    None
                } else {
                    Some(n)
                }
            },
        })
    }

    /// Converts a value that implements [`IntoLua`] into a [`Value`] instance.
    #[inline]
    pub fn pack(&self, t: impl IntoLua) -> Result<Value> {
        t.into_lua(self)
    }

    /// Converts a [`Value`] instance into a value that implements [`FromLua`].
    #[inline]
    pub fn unpack<T: FromLua>(&self, value: Value) -> Result<T> {
        T::from_lua(value, self)
    }

    /// Converts a value that implements [`IntoLua`] into a [`FromLua`] variant.
    #[inline]
    pub fn convert<U: FromLua>(&self, value: impl IntoLua) -> Result<U> {
        U::from_lua(value.into_lua(self)?, self)
    }

    /// Converts a value that implements [`IntoLuaMulti`] into a [`MultiValue`] instance.
    #[inline]
    pub fn pack_multi(&self, t: impl IntoLuaMulti) -> Result<MultiValue> {
        t.into_lua_multi(self)
    }

    /// Converts a [`MultiValue`] instance into a value that implements [`FromLuaMulti`].
    #[inline]
    pub fn unpack_multi<T: FromLuaMulti>(&self, value: MultiValue) -> Result<T> {
        T::from_lua_multi(value, self)
    }

    /// Sets or replaces an application data object of type `T`.
    ///
    /// Application data could be accessed at any time by using [`Lua::app_data_ref`] or
    /// [`Lua::app_data_mut`] methods where `T` is the data type.
    ///
    /// # Panics
    ///
    /// Panics if the app data container is currently borrowed.
    ///
    /// # Examples
    ///
    /// ```
    /// use mluau::{Lua, Result};
    ///
    /// fn hello(lua: &Lua, _: ()) -> Result<()> {
    ///     let mut s = lua.app_data_mut::<&str>().unwrap();
    ///     assert_eq!(*s, "hello");
    ///     *s = "world";
    ///     Ok(())
    /// }
    ///
    /// fn main() -> Result<()> {
    ///     let lua = Lua::new();
    ///     lua.set_app_data("hello");
    ///     lua.create_function(hello)?.call::<()>(())?;
    ///     let s = lua.app_data_ref::<&str>().unwrap();
    ///     assert_eq!(*s, "world");
    ///     Ok(())
    /// }
    /// ```
    #[track_caller]
    pub fn set_app_data<T: 'static>(&self, data: T) -> Option<T> {
        let lua = self.lock();
        let extra = unsafe { &*lua.extra.get() };
        extra.app_data.insert(data)
    }

    /// Tries to set or replace an application data object of type `T`.
    ///
    /// Returns:
    /// - `Ok(Some(old_data))` if the data object of type `T` was successfully replaced.
    /// - `Ok(None)` if the data object of type `T` was successfully inserted.
    /// - `Err(data)` if the data object of type `T` was not inserted because the container is
    ///   currently borrowed.
    ///
    /// See [`Lua::set_app_data`] for examples.
    pub fn try_set_app_data<T: 'static>(&self, data: T) -> StdResult<Option<T>, T> {
        let lua = self.lock();
        let extra = unsafe { &*lua.extra.get() };
        extra.app_data.try_insert(data)
    }

    /// Gets a reference to an application data object stored by [`Lua::set_app_data`] of type
    /// `T`.
    ///
    /// # Panics
    ///
    /// Panics if the data object of type `T` is currently mutably borrowed. Multiple immutable
    /// reads can be taken out at the same time.
    #[track_caller]
    pub fn app_data_ref<T: 'static>(&self) -> Option<AppDataRef<'_, T>> {
        let guard = self.guard();
        let extra = unsafe { &*self.raw.extra.get() };
        extra.app_data.borrow(Some(guard))
    }

    /// Tries to get a reference to an application data object stored by [`Lua::set_app_data`] of
    /// type `T`.
    pub fn try_app_data_ref<T: 'static>(&self) -> StdResult<Option<AppDataRef<'_, T>>, BorrowError> {
        let guard = self.guard();
        let extra = unsafe { &*guard.extra.get() };
        extra.app_data.try_borrow(Some(guard))
    }

    /// Gets a mutable reference to an application data object stored by [`Lua::set_app_data`] of
    /// type `T`.
    ///
    /// # Panics
    ///
    /// Panics if the data object of type `T` is currently borrowed.
    #[track_caller]
    pub fn app_data_mut<T: 'static>(&self) -> Option<AppDataRefMut<'_, T>> {
        let guard = self.guard();
        let extra = unsafe { &*guard.extra.get() };
        extra.app_data.borrow_mut(Some(guard))
    }

    /// Tries to get a mutable reference to an application data object stored by
    /// [`Lua::set_app_data`] of type `T`.
    pub fn try_app_data_mut<T: 'static>(&self) -> StdResult<Option<AppDataRefMut<'_, T>>, BorrowMutError> {
        let guard = self.guard();
        let extra = unsafe { &*guard.extra.get() };
        extra.app_data.try_borrow_mut(Some(guard))
    }

    /// Removes an application data of type `T`.
    ///
    /// # Panics
    ///
    /// Panics if the app data container is currently borrowed.
    #[track_caller]
    pub fn remove_app_data<T: 'static>(&self) -> Option<T> {
        let lua = self.lock();
        let extra = unsafe { &*lua.extra.get() };
        extra.app_data.remove()
    }

    /// Returns a weak reference to the Lua instance.
    ///
    /// This is useful for creating a reference to the Lua instance that does not prevent it from
    /// being deallocated.
    #[inline(always)]
    pub fn weak(&self) -> WeakLua {
        WeakLua(XRc::downgrade(&self.raw))
    }

    #[inline(always)]
    pub(crate) fn lock(&self) -> &RawLua {
        let rawlua = &self.raw;

        if unsafe { (*rawlua.extra.get()).running_gc } {
            panic!("Luau VM is suspended while GC is running");
        }
        rawlua
    }

    #[inline(always)]
    pub(crate) fn guard(&self) -> LuaGuard {
        LuaGuard(XRc::clone(&self.raw))
    }

    /// Checks if Luau is allowed to yield.
    #[inline]
    pub fn is_yieldable(&self) -> bool {
        self.raw.is_yieldable()
    }

    /// Returns the address of the Lua main state as a string
    #[inline]
    pub fn main_state_address(&self) -> StdString {
        let raw = &self.raw;
        format!("{:?}", raw.main_state())
    }

    /// Sets a callback that will be called when the underlying Lua state is closed
    ///
    /// Note: this callback must move all data it needs beforehand into the closure
    /// (which is why it is defined as a `Fn` instead of a function pointer).
    ///
    /// Safety:
    ///
    /// The provided function should either not panic or catch all panics using catch_unwind
    pub fn set_on_close<F>(&self, f: F)
    where
        F: Fn() + 'static,
    {
        let lua = &self.raw;
        unsafe {
            (*lua.extra.get()).on_close = Some(Box::new(f));
        }
    }

    /// Returns the state of the garbage collector as a string
    ///
    /// Useful when paired with GC interrupts

    pub fn gc_state_name(&self, state: c_int) -> Option<StdString> {
        let raw = &self.raw;
        raw.gc_state_name(state)
    }

    /// Returns the current allocation rate of garbage collector
    ///
    /// Returns -1 on failure

    pub fn gc_allocation_rate(&self) -> i64 {
        let raw = &self.raw;
        raw.gc_allocation_rate()
    }

    /// Returns the strong count of the Lua instance.
    ///
    /// This can be useful for debugging purposes
    #[inline(always)]
    pub fn strong_count(&self) -> usize {
        XRc::strong_count(&self.raw)
    }

    /// Returns the weak count of the Lua instance.
    ///
    /// This can be useful for debugging purposes
    #[inline(always)]
    pub fn weak_count(&self) -> usize {
        XRc::weak_count(&self.raw)
    }

    /// Runs callback with the inner RawLua value. It can be used to manually push and get values on
    /// the stack.
    ///
    /// This function is safe because all unsafe actions with RawLua can only be done with unsafe
    pub fn exec_raw_lua<R>(&self, f: impl FnOnce(&RawLua) -> R) -> R {
        let lua = self.lock();
        f(&lua)
    }
}

impl WeakLua {
    #[track_caller]
    #[inline(always)]
    pub(crate) fn lock(&self) -> LuaGuard {
        let guard = LuaGuard::new(self.0.upgrade().expect("Lua instance is destroyed"));

        if unsafe { (*guard.extra.get()).running_gc } {
            panic!("Luau VM is suspended while GC is running");
        }
        guard
    }

    #[inline(always)]
    pub(crate) fn try_lock(&self) -> Option<LuaGuard> {
        Some(LuaGuard::new(self.0.upgrade()?))
    }

    /// Upgrades the weak Lua reference to a strong reference.
    ///
    /// # Panics
    ///
    /// Panics if the Lua instance is destroyed.
    #[track_caller]
    #[inline(always)]
    pub fn upgrade(&self) -> Lua {
        Lua {
            raw: self.0.upgrade().expect("Lua instance is destroyed"),
            collect_garbage: false,
        }
    }

    /// Tries to upgrade the weak Lua reference to a strong reference.
    ///
    /// Returns `None` if the Lua instance is destroyed.
    #[inline(always)]
    pub fn try_upgrade(&self) -> Option<Lua> {
        Some(Lua {
            raw: self.0.upgrade()?,
            collect_garbage: false,
        })
    }

    /// Returns the strong count of the WeakLua
    ///
    /// Returns `0` if the Lua instance is destroyed.
    #[inline(always)]
    pub fn strong_count(&self) -> usize {
        self.0.strong_count()
    }

    /// Returns the weak count of the WeakLua
    ///     
    /// Returns `0` if the Lua instance is destroyed.
    #[inline(always)]
    pub fn weak_count(&self) -> usize {
        self.0.weak_count()
    }

    /// Returns if the Lua instance is destroyed.
    ///
    /// This is equivalent to checking if the strong count is `0`.
    #[inline(always)]
    pub fn is_destroyed(&self) -> bool {
        self.0.strong_count() == 0
    }
}

impl PartialEq for WeakLua {
    fn eq(&self, other: &Self) -> bool {
        XWeak::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for WeakLua {}

impl LuaGuard {
    pub(crate) fn new(handle: XRc<RawLua>) -> Self {
        LuaGuard(handle)
    }
}

impl Deref for LuaGuard {
    type Target = RawLua;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

unsafe extern "C" fn buffer_free_cb(
    state: *mut ffi::lua_State,
    _data: *mut std::ffi::c_void,
    _size: usize,
    userdata: *mut std::ffi::c_void,
) {
    if !userdata.is_null() {
        let extra = crate::state::extra::ExtraData::get(state);
        if !extra.is_null() {
            let prev_gc = (*extra).running_gc;
            (*extra).running_gc = true;
            ErasedHeader::drop(userdata);
            (*extra).running_gc = prev_gc;
        } else {
            ErasedHeader::drop(userdata);
        }
    }
}

pub(crate) mod extra;
mod raw;
pub(crate) mod util;

#[cfg(test)]
mod assertions {
    use super::*;

    // Lua has lots of interior mutability, should not be RefUnwindSafe
    static_assertions::assert_not_impl_any!(Lua: std::panic::RefUnwindSafe);

    static_assertions::assert_not_impl_any!(Lua: Send);
}
