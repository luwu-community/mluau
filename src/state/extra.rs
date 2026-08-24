use std::cell::{Cell, UnsafeCell};
use std::mem::MaybeUninit;
use std::os::raw::{c_int};

use crate::{DirectUserdataGetField, UntypedUserDataPtr};
use crate::error::Result;
use crate::state::RawLua;
use crate::stdlib::StdLib;
use crate::types::{AppData, ErasedHeader, UserDataDirectFieldGetCallback};
use std::rc::Rc as XRc;

use crate::chunk::Compiler;

use super::{Lua, WeakLua};

// userdata (v2) tag
//
// we use 126 as the tag here to try and avoid conflicts with other embedders although other embedders *should*
// be using the dedicated lua_findunuseduserdatatag to find unused tags
//
// We use a const here though for performance purposes
pub const USERDATA2_TAG: c_int = 2;

/// Data associated with the Lua state.
pub(crate) struct ExtraData {
    pub(super) lua: MaybeUninit<Lua>,
    pub(super) weak: MaybeUninit<WeakLua>,
    pub(super) owned: bool,

    // Containers to store arbitrary data (extensions)
    pub(super) app_data: AppData,
    pub(super) app_data_priv: AppData,

    pub(super) safe: bool,
    pub(super) libs: StdLib,

    pub(crate) error_traceback_ref: c_int,
    pub(crate) func_call_error_traceback: c_int,
    pub(crate) func_call_error: c_int,
    pub(crate) memory_error_ref: c_int,
    pub(crate) call_trampoline_ref: c_int,
    pub(crate) original_globals_ref: c_int,
    pub(crate) array_metatable_ref: c_int,

    pub(super) interrupt_callback: Option<crate::types::InterruptCallback>,

    pub(super) gc_interrupt_callback: Option<crate::types::GcInterruptCallback>,

    pub(super) thread_creation_callback: Option<crate::types::ThreadCreationCallback>,
    pub(crate) thread_state_change_callback: Option<crate::types::ThreadStateChangeCallback>,

    pub(super) thread_collection_callback: Option<crate::types::ThreadCollectionCallback>,

    pub(crate) have_thread_data: bool, // It is a memory leak in this case

    pub(crate) running_gc: bool,

    pub(crate) sandboxed: bool,

    pub(super) compiler: Option<Compiler>,
    #[cfg(feature = "luau-jit")]
    pub(super) enable_jit: bool,

    pub(super) on_close: Option<Box<dyn Fn() + 'static>>,

    pub(crate) mem_categories: Vec<std::ffi::CString>,

    pub(crate) registered_tags: [Cell<bool>; ffi::LUA_UTAG_LIMIT as usize],
    pub(crate) registered_directfieldgetters: [Option<UserDataDirectFieldGetCallback>; ffi::LUA_UTAG_LIMIT as usize],
}

impl Drop for ExtraData {
    fn drop(&mut self) {
        unsafe {
            if !self.owned {
                self.lua.assume_init_drop();
            }

            self.weak.assume_init_drop();
        }
    }
}

impl ExtraData {
    pub(crate) unsafe fn set_userdata_dtor(state: *mut ffi::lua_State, tag: c_int) {
        // Set global dtor for userdata v2, the data `ud` is guaranteed to be a ErasedHeader vtable
        //
        // All mluau owned userdata v2 will use this dtor for cleanup
        unsafe extern "C" fn userdata2_dtor(
            state: *mut ffi::lua_State,
            ud: *mut std::os::raw::c_void,
        ) {
            // Almost none Lua operations are allowed when destructor is running,
            // so we need to set a flag to prevent calling any Lua functions
            let extra = ExtraData::get(state);
            let prev_gc = (*extra).running_gc;
            (*extra).running_gc = true;
            ErasedHeader::drop(ud); // Note: panicking in the dtor for a userdata is not allowed and will call abort() bc this is a extern "C"
            (*extra).running_gc = prev_gc;
        }
        ffi::lua_setuserdatadtor(state, tag, Some(userdata2_dtor));
    }

    pub(crate) unsafe fn set_userdata_directfieldget<const TAG: c_int, S: DirectUserdataGetField>(state: *mut ffi::lua_State) {
        // Set global dtor for userdata v2, the data `ud` is guaranteed to be a ErasedHeader vtable
        //
        // All mluau owned userdata v2 will use this dtor for cleanup
        unsafe extern "C" fn userdata2_directfieldget<const TAG: c_int, S: DirectUserdataGetField>(
            state: *mut ffi::lua_State,
            ud: *mut std::os::raw::c_void,
            result: *mut std::os::raw::c_void,
        ) {
            // Almost none Lua operations are allowed when direct field getter is running,
            // so we need to set a flag to prevent calling any Lua functions
            let extra = ExtraData::get(state);
            let prev_gc = (*extra).running_gc;
            (*extra).running_gc = true;
            // Note: panicking in the direct field get for a userdata is not allowed and will call abort() bc this is a extern "C"
            if let Some(ref func) = (*extra).registered_directfieldgetters[TAG as usize] {
                let cb_ud = UntypedUserDataPtr::new(ud);
                func(S::STR, cb_ud).finalize(result);
            } else {
                ffi::lua_userdatadirectfield_setnil(result);
            }
            (*extra).running_gc = prev_gc;
        }
        ffi::lua_registeruserdatadirectfieldget(state, TAG, S::C_STR.as_ptr(), userdata2_directfieldget::<TAG, S>);
    }

    pub(super) unsafe fn init(state: *mut ffi::lua_State, owned: bool) -> XRc<UnsafeCell<Self>> {
        Self::set_userdata_dtor(state, USERDATA2_TAG); // Base userdata dtor

        #[allow(clippy::arc_with_non_send_sync)]
        let extra = XRc::new(UnsafeCell::new(ExtraData {
            lua: MaybeUninit::uninit(),
            weak: MaybeUninit::uninit(),
            owned,
            app_data: AppData::default(),
            app_data_priv: AppData::default(),
            safe: false,
            libs: StdLib::NONE,
            error_traceback_ref: {
                ffi::lua_pushcfunction(state, crate::util::error_traceback);
                let r = ffi::lua_refpool(state, -1);
                ffi::lua_pop(state, 1);
                r
            },
            func_call_error_traceback: {
                ffi::lua_pushcfunction(state, crate::util::func_call_error_traceback);
                let r = ffi::lua_refpool(state, -1);
                ffi::lua_pop(state, 1);
                r
            },
            func_call_error: {
                ffi::lua_pushcfunction(state, crate::util::func_call_error);
                let r = ffi::lua_refpool(state, -1);
                ffi::lua_pop(state, 1);
                r
            },
            memory_error_ref: {
                let s = "memory error";
                ffi::lua_pushlstring(state, s.as_ptr() as *const std::os::raw::c_char, s.len());
                let r = ffi::lua_refpool(state, -1);
                ffi::lua_pop(state, 1);
                r
            },
            call_trampoline_ref: {
                ffi::lua_pushcfunction(state, crate::util::call_trampoline);
                let r = ffi::lua_refpool(state, -1);
                ffi::lua_pop(state, 1);
                r
            },
            original_globals_ref: {
                ffi::lua_pushvalue(state, ffi::LUA_GLOBALSINDEX);
                let r = ffi::lua_refpool(state, -1);
                ffi::lua_pop(state, 1);
                r
            },
            array_metatable_ref: {
                // Create array metatable
                ffi::lua_createtable(state, 0, 1);
                ffi::lua_pushstring(state, cstr!("__metatable"));
                ffi::lua_pushboolean(state, 0);
                ffi::lua_rawset(state, -3);

                let r = ffi::lua_refpool(state, -1);
                ffi::lua_pop(state, 1);
                r
            },
            interrupt_callback: None,

            gc_interrupt_callback: None,

            thread_creation_callback: None,
            thread_state_change_callback: None,

            thread_collection_callback: None,

            have_thread_data: false,

            sandboxed: false,

            compiler: None,
            #[cfg(feature = "luau-jit")]
            enable_jit: true,

            running_gc: false,
            on_close: None,

            mem_categories: vec![std::ffi::CString::new("main").unwrap()],
            registered_tags: {
                let tags = [const { Cell::new(false) }; _];
                tags[USERDATA2_TAG as usize].set(true); // USERDATA2_TAG is a default registered tag
                tags
            },
            registered_directfieldgetters: {
                let tags = [const { None }; _];
                tags
            },
        }));

        // Store it in the registry
        mlua_expect!(Self::store(&extra, state), "Error while storing extra data");

        extra
    }

    pub(super) unsafe fn set_lua(&mut self, raw: &XRc<RawLua>) {
        self.lua.write(Lua {
            raw: XRc::clone(raw),
            collect_garbage: false,
        });
        self.weak.write(WeakLua(XRc::downgrade(raw)));
    }

    pub(crate) unsafe fn get(state: *mut ffi::lua_State) -> *mut Self {
        return (*ffi::lua_callbacks(state)).userdata as *mut _;
    }

    unsafe fn store(extra: &XRc<UnsafeCell<Self>>, state: *mut ffi::lua_State) -> Result<()> {
        (*ffi::lua_callbacks(state)).userdata = extra.get() as *mut _;
        Ok(())
    }

    #[inline(always)]
    pub(crate) unsafe fn lua(&self) -> &Lua {
        self.lua.assume_init_ref()
    }

    #[inline(always)]
    pub(crate) unsafe fn raw_lua(&self) -> &RawLua {
        &*self.lua.assume_init_ref().raw
    }

    #[inline(always)]
    pub(super) unsafe fn weak(&self) -> &WeakLua {
        self.weak.assume_init_ref()
    }
}
