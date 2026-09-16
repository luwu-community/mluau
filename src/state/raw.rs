use std::cell::{Cell, UnsafeCell};
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::string::String as StdString;

use crate::chunk::ChunkMode;
use crate::error::Result;
use crate::function::Function;
use crate::luau::ENABLED_FFLAGS;
use crate::memory::{MemoryState, ALLOCATOR};
use crate::state::util::{StateGuard, extract_panic_str, push_callback_error};
use crate::stdlib::StdLib;
use crate::string::String;
use crate::table::Table;
use crate::thread::Thread;
use crate::traits::IntoLua;
use crate::types::{
    AppDataRef, AppDataRefMut, Callback, Integer, LightUserData,
    LuaType, ValueRef
};
use std::rc::Rc as XRc;

use crate::types::Continuation;

use crate::userdata::AnyUserData;
use crate::util::{
    assert_stack, check_stack, get_main_state,
    pop_error,
    push_string, push_table,
    to_string, StackGuard,
};
use crate::value::{Nil, Value};

use super::extra::ExtraData;
use super::{Lua, WeakLua};

/// An inner Lua struct which holds a raw Lua state.
pub struct RawLua {
    // The state is dynamic and depends on context
    pub(super) state: Cell<*mut ffi::lua_State>,
    pub(super) main_state: Option<NonNull<ffi::lua_State>>,
    pub(super) extra: XRc<UnsafeCell<ExtraData>>,
    owned: bool,
}

impl Drop for RawLua {
    fn drop(&mut self) {
        unsafe {
            if !self.owned {
                return;
            }

            {
                let extra = self.extra.get();
                if let Some(on_close) = (*extra).on_close.take() {
                    // Call the on_close callback
                    on_close();
                }
            }

            let mem_state = MemoryState::get(self.main_state());

            {
                // Reset any callbacks
                (*ffi::lua_callbacks(self.main_state())).interrupt = None;
                //(*ffi::lua_callbacks(self.main_state())).userthread = None;
            }

            ffi::lua_close(self.main_state());

            // Deallocate `MemoryState`
            if !mem_state.is_null() {
                drop(Box::from_raw(mem_state));
            }
        }
    }
}

impl RawLua {
    #[inline(always)]
    pub(crate) fn lua(&self) -> &Lua {
        unsafe { (*self.extra.get()).lua() }
    }

    #[inline(always)]
    pub(crate) fn weak(&self) -> &WeakLua {
        unsafe { (*self.extra.get()).weak() }
    }

    /// Returns a pointer to the current Lua state.
    ///
    /// The pointer refers to the active Lua coroutine and depends on the context.
    #[inline(always)]
    pub fn state(&self) -> *mut ffi::lua_State {
        self.state.get()
    }

    #[inline(always)]
    pub fn main_state(&self) -> *mut ffi::lua_State {
        self.main_state
            .map(|state| state.as_ptr())
            .unwrap_or_else(|| self.state())
    }




    #[inline(always)]
    pub(crate) fn extra(&self) -> *mut ExtraData {
        self.extra.get()
    }

    pub(super) unsafe fn new(libs: StdLib, allocator: Box<dyn crate::memory::LuaAllocator>) -> XRc<Self> {
        // init needed fflags
        {
            static INIT_FFLAGS: std::sync::Once = std::sync::Once::new();
            INIT_FFLAGS.call_once(|| {
                for fflag in ENABLED_FFLAGS {
                    mlua_expect!(Lua::set_fflag_inner(fflag, true), "base fflag {fflag} not set",)
                }
            });
        }

        Self::new_ext(libs, true, allocator)
    }

    pub(super) unsafe fn new_ext(
        libs: StdLib,
        owned: bool,
        allocator: Box<dyn crate::memory::LuaAllocator>
    ) -> XRc<Self> {
        let mem_state: *mut MemoryState = Box::into_raw(Box::new(MemoryState::new(allocator)));
        let mut state = ffi::lua_newstate(ALLOCATOR, mem_state as *mut c_void);
        // If state is null then switch to Lua internal allocator
        if state.is_null() {
            drop(Box::from_raw(mem_state));
            state = ffi::luaL_newstate();
        }
        assert!(!state.is_null(), "Failed to create a Lua VM");

        ffi::luaL_requiref(state, cstr!("_G"), ffi::luaopen_base, 1);
        ffi::lua_pop(state, 1);

        // Init Luau code generator (jit)
        #[cfg(feature = "luau-jit")]
        if ffi::luau_codegen_supported() != 0 {
            ffi::luau_codegen_create(state);
        }

        let rawlua = Self::init_from_ptr(state, owned);
        let extra = rawlua.extra.get();

        mlua_expect!(
            load_std_libs(state, libs),
            "Error during loading standard libraries"
        );
        (*extra).libs |= libs;

        rawlua
    }

    pub(super) unsafe fn init_from_ptr(state: *mut ffi::lua_State, owned: bool) -> XRc<Self> {
        assert!(!state.is_null(), "Lua state is NULL");
        if let Some(lua) = Self::try_from_ptr(state) {
            return lua;
        }

        let main_state = get_main_state(state).unwrap_or(state);
        let main_state_top = ffi::lua_gettop(main_state);

        // Init ExtraData first so protect_lua can use it for error_traceback
        let extra = ExtraData::init(main_state, owned);

        mlua_debug_assert!(
            ffi::lua_gettop(main_state) == main_state_top,
            "stack leak during creation"
        );
        assert_stack(main_state, ffi::LUA_MINSTACK);

        #[allow(clippy::arc_with_non_send_sync)]
        let rawlua = XRc::new(RawLua {
            state: Cell::new(state),
            // Make sure that we don't store current state as main state (if it's not available)
            main_state: get_main_state(state).and_then(NonNull::new),
            extra: XRc::clone(&extra),
            owned,
        });
        (*extra.get()).set_lua(&rawlua);
        if owned {
            // If Lua state is managed by us, then make internal `RawLua` reference "weak"
            XRc::decrement_strong_count(XRc::as_ptr(&rawlua));
        } else {
            // If Lua state is not managed by us, then keep internal `RawLua` reference "strong"
            // but `Extra` reference weak (it will be collected from registry at lua_close time)
            XRc::decrement_strong_count(XRc::as_ptr(&extra));
        }

        rawlua
    }

    unsafe fn try_from_ptr(state: *mut ffi::lua_State) -> Option<XRc<Self>> {
        match ExtraData::get(state) {
            extra if extra.is_null() => None,
            extra => Some(XRc::clone(&(*extra).lua().raw)),
        }
    }

    /// Marks the Lua state as safe.
    #[inline(always)]
    pub(super) fn mark_safe(&self) {
        unsafe { (*self.extra.get()).safe = true };
    }

    /// Loads the specified subset of the standard libraries into an existing Lua state.
    ///
    /// Use the [`StdLib`] flags to specify the libraries you want to load.
    ///
    /// [`StdLib`]: crate::StdLib
    pub(super) unsafe fn load_std_libs(&self, libs: StdLib) -> Result<()> {
        let is_safe = (*self.extra.get()).safe;



        let res = load_std_libs(self.main_state(), libs);

        // If `package` library loaded into a safe lua state then disable C modules


        let _ = is_safe;
        unsafe { (*self.extra.get()).libs |= libs };

        res
    }

    /// Private version of [`Lua::try_set_app_data`]
    #[inline]
    pub(crate) fn set_priv_app_data<T: 'static>(&self, data: T) -> Option<T> {
        let extra = unsafe { &*self.extra.get() };
        extra.app_data_priv.insert(data)
    }

    /// Private version of [`Lua::app_data_ref`]
    #[track_caller]
    #[inline]
    pub(crate) fn priv_app_data_ref<T: 'static>(&self) -> Option<AppDataRef<'_, T>> {
        let extra = unsafe { &*self.extra.get() };
        extra.app_data_priv.borrow(None)
    }

    /// Private version of [`Lua::app_data_mut`]
    #[track_caller]
    #[inline]
    pub(crate) fn priv_app_data_mut<T: 'static>(&self) -> Option<AppDataRefMut<'_, T>> {
        let extra = unsafe { &*self.extra.get() };
        extra.app_data_priv.borrow_mut(None)
    }

    pub(crate) fn load_chunk(
        &self,
        name: Option<&CStr>,
        env: Option<&Table>,
        mode: ChunkMode,
        source: &[u8],
        trusted_binary: bool,
    ) -> Result<Function> {
        let state = self.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 3)?;

            let name = name.map(CStr::as_ptr).unwrap_or(ptr::null());
            let mode = match mode {
                ChunkMode::Binary => cstr!("b"),
                ChunkMode::Text => cstr!("t"),
                // None => cstr!("bt"),
            };
            let status = protect_lua!(state, 0, 1, |state| {
                self.load_chunk_inner(state, name, env, mode, source, trusted_binary)
            })?;
            match status {
                ffi::LUA_OK => Ok(Function(self.pop_ref())),
                err => Err(pop_error(state, err)),
            }
        }
    }

    pub(crate) unsafe fn load_chunk_inner(
        &self,
        state: *mut ffi::lua_State,
        name: *const c_char,
        env: Option<&Table>,
        mode: *const c_char,
        source: &[u8],
        trusted_binary: bool,
    ) -> c_int {
        let env = match env {
            Some(env) => {
                self.push_ref_at(&env.0, self.state());
                -1
            }
            _ => 0,
        };

        let status = if trusted_binary {
            ffi::luau_load_trusted_binary(state, source.as_ptr() as *const c_char, source.len(), name, env)
        } else {
            ffi::luaL_loadbufferenv(
                state,
                source.as_ptr() as *const c_char,
                source.len(),
                name,
                mode,
                env,
            )
        };
        #[cfg(feature = "luau-jit")]
        if status == ffi::LUA_OK {
            if (*self.extra.get()).enable_jit && ffi::luau_codegen_supported() != 0 {
                ffi::luau_codegen_compile(state, -1);
            }
        }
        status
    }

    /// See [`Lua::create_string`]
    pub(crate) unsafe fn create_string(&self, s: &[u8]) -> Result<String> {
        let state = self.state();
        let _sg = StackGuard::new(state);
        check_stack(state, 3)?;
        push_string(state, s)?;
        Ok(String(self.pop_ref()))
    }

    pub(crate) unsafe fn create_external_string<S: crate::string::ExternalString>(
        &self,
        s: S,
    ) -> Result<String> {
        let state = self.state();
        let _sg = StackGuard::new(state);
        check_stack(state, 3)?;

        let free_cb = if S::is_static(&s) {
            None
        } else {
            Some(S::free_string as ffi::lua_StringFree)
        };
        let (ptr, len, userdata) = s.into_ext_parts()?;

        crate::util::push_external_string(state, ptr as *const _, len, userdata, free_cb)?;
        Ok(String(self.pop_ref()))
    }

    pub(crate) unsafe fn create_buffer_with_capacity(&self, size: usize) -> Result<(*mut u8, crate::Buffer)> {
        let state = self.state();
        let _sg = StackGuard::new(state);
        check_stack(state, 3)?;
        let ptr = crate::util::push_buffer(state, size)?;
        Ok((ptr, crate::Buffer(self.pop_ref())))
    }

    pub(crate) unsafe fn create_external_buffer(
        &self,
        size: usize,
        data: *mut u8,
        userdata: *mut std::ffi::c_void,
        free_cb: Option<ffi::lua_BufferFree>,
        mode: std::os::raw::c_int,
    ) -> Result<(*mut u8, crate::Buffer)> {
        let state = self.state();
        let _sg = StackGuard::new(state);
        check_stack(state, 3)?;
        let ptr = crate::util::push_external_buffer(state, size, data, userdata, free_cb, mode)?;
        Ok((ptr, crate::Buffer(self.pop_ref())))
    }

    /// See [`Lua::create_table_with_capacity`]
    pub(crate) unsafe fn create_table_with_capacity(&self, narr: usize, nrec: usize) -> Result<Table> {
        let state = self.state();
        let _sg = StackGuard::new(state);
        check_stack(state, 3)?;
        push_table(state, narr, nrec)?;
        Ok(Table(self.pop_ref()))
    }

    /// See [`Lua::create_table_from`]
    pub(crate) unsafe fn create_table_from<I, K, V>(&self, iter: I) -> Result<Table>
    where
        I: IntoIterator<Item = (K, V)>,
        K: IntoLua,
        V: IntoLua,
    {
        let state = self.state();
        let _sg = StackGuard::new(state);
        check_stack(state, 6)?;

        let iter = iter.into_iter();
        let lower_bound = iter.size_hint().0;
        push_table(state, 0, lower_bound)?;
        for (k, v) in iter {
            self.push_at(state, k)?;
            self.push_at(state, v)?;
            protect_lua!(state, 3, 1, fn(state) ffi::lua_rawset(state, -3))?;
        }

        Ok(Table(self.pop_ref()))
    }

    /// See [`Lua::create_sequence_from`]
    pub(crate) unsafe fn create_sequence_from<T, I>(&self, iter: I) -> Result<Table>
    where
        T: IntoLua,
        I: IntoIterator<Item = T>,
    {
        let state = self.state();
        let _sg = StackGuard::new(state);
        check_stack(state, 5)?;

        let iter = iter.into_iter();
        let lower_bound = iter.size_hint().0;
        push_table(state, lower_bound, 0)?;
        for (i, v) in iter.enumerate() {
            self.push_at(state, v)?;
            protect_lua!(state, 2, 1, |state| {
                ffi::lua_rawseti(state, -2, (i + 1) as Integer);
            })?;
        }

        Ok(Table(self.pop_ref()))
    }

    /// Wraps a Lua function into a new thread (or coroutine).
    ///
    /// Takes function by reference.
    pub(crate) unsafe fn create_thread(&self, func: &Function) -> Result<Thread> {
        let state = self.state();
        let _sg = StackGuard::new(state);

        let thread_state = {
            check_stack(state, 3)?;

            let thread_state = protect_lua!(state, 0, 1, |state| ffi::lua_newthread(state))?;

            thread_state
        };

        let thread = Thread(self.pop_ref(), thread_state);
        self.push_ref_at(&func.0, thread_state);
        Ok(thread)
    }

    /// Pushes a primitive type value onto the Lua stack.
    pub(crate) unsafe fn push_primitive_type<T: LuaType>(&self, state: *mut ffi::lua_State) -> bool {
        match T::TYPE_ID {
            ffi::LUA_TBOOLEAN => {
                ffi::lua_pushboolean(state, 0);
            }
            ffi::LUA_TLIGHTUSERDATA => {
                ffi::lua_pushlightuserdata(state, ptr::null_mut());
            }
            ffi::LUA_TNUMBER => {
                ffi::lua_pushnumber(state, 0.);
            }

            ffi::LUA_TVECTOR => {
                #[cfg(not(feature = "luau-vector4"))]
                ffi::lua_pushvector(state, 0., 0., 0.);
                #[cfg(feature = "luau-vector4")]
                ffi::lua_pushvector(state, 0., 0., 0., 0.);
            }
            ffi::LUA_TSTRING => {
                ffi::lua_pushstring(state, b"\0" as *const u8 as *const _);
            }
            ffi::LUA_TFUNCTION => {
                unsafe extern "C-unwind" fn func(_state: *mut ffi::lua_State) -> c_int {
                    0
                }
                ffi::lua_pushcfunction(state, func);
            }
            ffi::LUA_TTHREAD => {
                ffi::lua_pushthread(state);
            }

            ffi::LUA_TBUFFER => {
                ffi::lua_newbuffer(state, 0);
            }
            _ => return false,
        }
        true
    }

    /// Pushes a value that implements `IntoLua` onto the Lua stack.
    ///
    /// Uses up to 2 stack spaces to push a single value, does not call `checkstack`.
    #[inline(always)]
    pub unsafe fn push_at(&self, state: *mut ffi::lua_State, value: impl IntoLua) -> Result<()> {
        value.push_into_specified_stack(self, state)
    }

    /// Pushes a `Value` (by reference) onto the specified Lua stack.
    ///
    /// Uses 3 stack spaces, does not call `checkstack`.
    pub unsafe fn push_value_at(&self, value: &Value, state: *mut ffi::lua_State) {
        match value {
            Value::Nil => ffi::lua_pushnil(state),
            #[cfg(feature = "none-primitive")]
            Value::None => ffi::lua_pushsymnone(state),
            Value::Boolean(b) => ffi::lua_pushboolean(state, *b as c_int),
            Value::LightUserData(ud) => ffi::lua_pushlightuserdata(state, ud.0),
            Value::Integer(i) => ffi::lua_pushinteger(state, *i),

            Value::Int64(i) => ffi::lua_pushinteger64(state, *i),
            Value::Number(n) => ffi::lua_pushnumber(state, *n),

            Value::Vector(v) => {
                #[cfg(not(feature = "luau-vector4"))]
                ffi::lua_pushvector(state, v.x(), v.y(), v.z());
                #[cfg(feature = "luau-vector4")]
                ffi::lua_pushvector(state, v.x(), v.y(), v.z(), v.w());
            }
            Value::String(s) => self.push_ref_at(&s.0, state),
            Value::Table(t) => self.push_ref_at(&t.0, state),
            Value::Function(f) => self.push_ref_at(&f.0, state),
            Value::Thread(t) => self.push_ref_at(&t.0, state),
            Value::UserData(ud) => self.push_ref_at(&ud.0, state),

            Value::Buffer(buf) => self.push_ref_at(&buf.0, state),

            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Class(c) => self.push_ref_at(&c.0, state),
            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Object(o) => self.push_ref_at(&o.0, state),
            Value::Other(vref) => self.push_ref_at(vref, state),
        }
    }

    pub unsafe fn pop_value_at(&self, state: *mut ffi::lua_State) -> Value {
        let value = self.stack_value_at(-1, None, state);
        ffi::lua_pop(state, 1);
        value
    }

    /// Returns value at given stack index without popping it.
    pub unsafe fn stack_value_at(
        &self,
        idx: c_int,
        type_hint: Option<c_int>,
        state: *mut ffi::lua_State,
    ) -> Value {
        match type_hint.unwrap_or_else(|| ffi::lua_type(state, idx)) {
            ffi::LUA_TNIL => Nil,
            #[cfg(feature = "none-primitive")]
            ffi::LUA_TSYMNONE => Value::None,

            ffi::LUA_TBOOLEAN => Value::Boolean(ffi::lua_toboolean(state, idx) != 0),

            ffi::LUA_TLIGHTUSERDATA => Value::LightUserData(LightUserData(ffi::lua_touserdata(
                state, idx,
            ))),

            ffi::LUA_TNUMBER => {
                use crate::types::Number;

                let n = ffi::lua_tonumber(state, idx);
                match num_traits::cast(n) {
                    Some(i) if n.to_bits() == (i as Number).to_bits() => Value::Integer(i),
                    _ => Value::Number(n),
                }
            }

            ffi::LUA_TINTEGER => Value::Int64(ffi::lua_tointeger64(state, idx)),

            ffi::LUA_TVECTOR => {
                let v = ffi::lua_tovector(state, idx);
                mlua_debug_assert!(!v.is_null(), "vector is null");
                #[cfg(not(feature = "luau-vector4"))]
                return Value::Vector(crate::Vector([*v, *v.add(1), *v.add(2)]));
                #[cfg(feature = "luau-vector4")]
                return Value::Vector(crate::Vector([
                    *v,
                    *v.add(1),
                    *v.add(2),
                    *v.add(3),
                ]));
            }

            ffi::LUA_TSTRING => {
                Value::String(String(self.new_value_ref_from(state, idx)))
            }

            ffi::LUA_TTABLE => {
                Value::Table(Table(self.new_value_ref_from(state, idx)))
            }

            ffi::LUA_TFUNCTION => {
                Value::Function(Function(self.new_value_ref_from(state, idx)))
            }
            ffi::LUA_TUSERDATA => {
                Value::UserData(AnyUserData(self.new_value_ref_from(state, idx)))
            }

            ffi::LUA_TTHREAD => {
                let thread_state = ffi::lua_tothread(state, idx);
                Value::Thread(Thread(
                    self.new_value_ref_from(state, idx),
                    thread_state,
                ))
            }

            ffi::LUA_TBUFFER => {
                Value::Buffer(crate::Buffer(self.new_value_ref_from(state, idx)))
            }

            #[cfg(feature = "luwu-classes")]
            ffi::LUA_TCLASS => {
                Value::Class(crate::Class(self.new_value_ref_from(state, idx)))
            }

            #[cfg(feature = "luwu-classes")]
            ffi::LUA_TOBJECT => {
                Value::Object(crate::Object(self.new_value_ref_from(state, idx)))
            }

            _ => {
                Value::Other(self.new_value_ref_from(state, idx))
            }
        }
    }

    // Pushes a ValueRef value onto the specified Lua stack, uses 1 stack space, does not call
    // checkstack
    #[inline]
    pub(crate) unsafe fn push_ref_at(&self, vref: &ValueRef, state: *mut ffi::lua_State) {
        assert!(
            self.weak() == &vref.lua,
            "Lua instance passed Value created from a different main Lua state"
        );
        ffi::lua_getrefpool(state, vref.ref_id);
    }

    // Pops the topmost element of the stack and stores a reference to it. This pins the object,
    // preventing garbage collection until the returned `ValueRef` is dropped.
    #[inline]
    pub(crate) unsafe fn pop_ref(&self) -> ValueRef {
        self.pop_ref_at(self.state())
    }

    /// Same as pop_ref but allows specifying state
    pub unsafe fn pop_ref_at(&self, state: *mut ffi::lua_State) -> ValueRef {
        let ref_id = ffi::lua_refpool(state, -1);
        ffi::lua_pop(state, 1);
        ValueRef::new(self, ref_id)
    }

    pub unsafe fn new_value_ref_from(&self, state: *mut ffi::lua_State, idx: c_int) -> ValueRef {
        let ref_id = ffi::lua_refpool(state, idx);
        ValueRef::new(self, ref_id)
    }

    pub unsafe fn drop_ref(&self, vref: &ValueRef) {
        ffi::lua_unrefpool(self.state(), vref.ref_id);
    }

    // Creates a Function out of a Callback containing a 'static Fn.
    pub(crate) fn create_callback(&self, func: Callback, debugname: *const c_char) -> Result<Function> {
        unsafe extern "C-unwind" fn call_callback(state: *mut ffi::lua_State) -> c_int {
            let upvalue = ffi::lua_getcclosuredata(state) as *mut Callback;
            let extra = crate::state::extra::ExtraData::get(state);
            match catch_unwind(AssertUnwindSafe(|| {
                // Lua ensures that `LUA_MINSTACK` stack spaces are available (after pushing arguments)
                let nargs = ffi::lua_gettop(state);
                let rawlua = (*extra).raw_lua();
                let _guard = StateGuard::new(rawlua, state);
                (*upvalue)(rawlua, nargs)
            })) {
                Ok(ret) => ret.finish(state),
                Err(panic) => {
                    let panic = extract_panic_str(panic);
                    push_callback_error(state, extra, panic);
                    ffi::lua_error(state);
                }
            }
        }

        let state = self.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 4)?;

            crate::util::push_fat_cclosure(
                state,
                func,
                call_callback,
                debugname,
                None,
            )?;

            Ok(Function(self.pop_ref()))
        }
    }

    // Creates a Function out of a Callback and a continuation containing a 'static Fn.
    //
    // In Luau, uses pushcclosurek
    //
    // In Lua 5.2/5.3/5.4/JIT, makes a normal function that then yields to the continuation via yieldk

    #[allow(unused_variables)]
    pub(crate) fn create_callback_with_continuation(
        &self,
        func: Callback,
        cont: Continuation,
        debugname: *const c_char,
    ) -> Result<Function> {
        unsafe extern "C-unwind" fn call_callback(state: *mut ffi::lua_State) -> c_int {
            let upvalue = ffi::lua_getcclosuredata(state) as *mut (Callback, Continuation);
            let extra = crate::state::extra::ExtraData::get(state);
            match catch_unwind(AssertUnwindSafe(|| {
                // Lua ensures that `LUA_MINSTACK` stack spaces are available (after pushing arguments)
                let nargs = ffi::lua_gettop(state);
                let rawlua = (*extra).raw_lua();
                let _guard = StateGuard::new(rawlua, state);
                ((*upvalue).0)(rawlua, nargs)
            })) {
                Ok(ret) => ret.finish(state),
                Err(panic) => {
                    let panic = extract_panic_str(panic);
                    push_callback_error(state, extra, panic);
                    ffi::lua_error(state);
                }
            }
        }

        unsafe extern "C-unwind" fn cont_callback(state: *mut ffi::lua_State, status: c_int) -> c_int {
            let upvalue = ffi::lua_getcclosuredata(state) as *mut (Callback, Continuation);
            let extra = crate::state::extra::ExtraData::get(state);
            match catch_unwind(AssertUnwindSafe(|| {
                // Lua ensures that `LUA_MINSTACK` stack spaces are available (after pushing arguments)
                let nargs = ffi::lua_gettop(state);
                let rawlua = (*extra).raw_lua();
                let _guard = StateGuard::new(rawlua, state);
                ((*upvalue).1)(rawlua, nargs, status)
            })) {
                Ok(ret) => ret.finish(state),
                Err(panic) => {
                    let panic = extract_panic_str(panic);
                    push_callback_error(state, extra, panic);
                    ffi::lua_error(state);
                }
            }
        }

        let state = self.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 4)?;

            crate::util::push_fat_cclosure(
                state,
                (func, cont),
                call_callback,
                debugname,
                Some(cont_callback),
            )?;

            Ok(Function(self.pop_ref()))
        }
    }

    /// Returns the state of garbage collector as a string

    pub(crate) fn gc_state_name(&self, state: c_int) -> Option<StdString> {
        let state_ptr = unsafe { ffi::lua_gcstatename(state) };
        if state_ptr.is_null() {
            None
        } else {
            let c_str = unsafe { CStr::from_ptr(state_ptr) };
            Some(c_str.to_string_lossy().into_owned())
        }
    }

    /// Returns the current allocation rate of garbage collector
    ///
    /// Returns -1 on failure

    pub(crate) fn gc_allocation_rate(&self) -> i64 {
        unsafe { ffi::lua_gcallocationrate(self.state()) }
    }


    #[inline]
    pub(crate) fn is_yieldable(&self) -> bool {
        unsafe { ffi::lua_isyieldable(self.state()) != 0 }
    }

    pub(crate) unsafe fn traceback_at(&self, state: *mut ffi::lua_State) -> Result<StdString> {
        check_stack(state, ffi::LUA_TRACEBACK_STACK)?;

        let _sg = StackGuard::new(state);
        ffi::luaL_traceback(state, state, ptr::null(), 0);
        let traceback = to_string(state, -1);
        ffi::lua_pop(state, 1);
        Ok(traceback)
    }
}

// Uses 3 stack spaces
unsafe fn load_std_libs(state: *mut ffi::lua_State, libs: StdLib) -> Result<()> {
    unsafe fn requiref(
        state: *mut ffi::lua_State,
        modname: *const c_char,
        openf: ffi::lua_CFunction,
        glb: c_int,
    ) -> Result<()> {
        protect_lua!(state, 0, 0, |state| {
            ffi::luaL_requiref(state, modname, openf, glb)
        })
    }

    if libs.contains(StdLib::COROUTINE) {
        requiref(state, ffi::LUA_COLIBNAME, ffi::luaopen_coroutine, 1)?;
    }

    if libs.contains(StdLib::TABLE) {
        requiref(state, ffi::LUA_TABLIBNAME, ffi::luaopen_table, 1)?;
    }

    if libs.contains(StdLib::OS) {
        requiref(state, ffi::LUA_OSLIBNAME, ffi::luaopen_os, 1)?;
    }

    if libs.contains(StdLib::STRING) {
        requiref(state, ffi::LUA_STRLIBNAME, ffi::luaopen_string, 1)?;
    }

    if libs.contains(StdLib::UTF8) {
        requiref(state, ffi::LUA_UTF8LIBNAME, ffi::luaopen_utf8, 1)?;
    }

    if libs.contains(StdLib::BIT) {
        requiref(state, ffi::LUA_BITLIBNAME, ffi::luaopen_bit32, 1)?;
    }

    if libs.contains(StdLib::BUFFER) {
        requiref(state, ffi::LUA_BUFFERLIBNAME, ffi::luaopen_buffer, 1)?;
    }

    if libs.contains(StdLib::VECTOR) {
        requiref(state, ffi::LUA_VECLIBNAME, ffi::luaopen_vector, 1)?;
    }

    if libs.contains(StdLib::INTEGER) {
        requiref(state, ffi::LUA_INTLIBNAME, ffi::luaopen_integer, 1)?;
    }

    if libs.contains(StdLib::MATH) {
        requiref(state, ffi::LUA_MATHLIBNAME, ffi::luaopen_math, 1)?;
    }

    if libs.contains(StdLib::DEBUG) {
        requiref(state, ffi::LUA_DBLIBNAME, ffi::luaopen_debug, 1)?;
    }

    Ok(())
}
