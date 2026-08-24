use std::{any::TypeId, ffi::{c_int, c_void}, marker::PhantomData, ptr::NonNull};

use crate::{FromLua, FromLuaMulti, Function, Integer, IntoLua, IntoLuaMulti, Lua, Number, Result, Table, USERDATA2_TAG, Value, WeakLua, state::LuaGuard, types::{TypedRef, UnbackedTypedRef, ValueRef}, util::{StackGuard, assert_stack, check_stack, short_type_name}};

pub(crate) const fn assert_ud_tag<const TAG: c_int>() {
    assert!(TAG > 0 && TAG < ffi::LUA_UTAG_LIMIT);
}

/// Handle to an internal Lua userdata
#[derive(Clone, Debug, PartialEq)]
pub struct AnyUserData(pub(crate) ValueRef);

impl AnyUserData {
    /// Returns the metatable of this [`AnyUserData`].
    pub fn metatable(&self) -> Option<Table> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);

            // Push the userdata onto the stack
            lua.push_ref_at(&self.0, state);

            let res = ffi::lua_getmetatable(state, -1); // Checked that non-empty on the previous call
            if res == 0 {
                return None;
            }
            Some(Table(lua.pop_ref()))
        }
    }

    /// Returns the metatable of this [`AnyUserData`].
    pub fn set_metatable(&self, metatable: Option<Table>) {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            assert_stack(state, 2);

            lua.push_ref_at(&self.0, state);
            if let Some(metatable) = &metatable {
                lua.push_ref_at(&metatable.0, state);
            } else {
                ffi::lua_pushnil(state);
            }
            ffi::lua_setmetatable(state, -2);
        }
    }

    #[inline(always)]
    fn borrow_to_ptr<T: 'static, const TAG: c_int>(&self) -> (Option<NonNull<T>>, LuaGuard) {
        const { assert_ud_tag::<TAG>() }

        let lua = self.0.lua.lock();
        let state = lua.state();
        let ptr = unsafe {
            let _sg = StackGuard::new(state);

            // Push the userdata onto the stack
            lua.push_ref_at(&self.0, state);

            let res = ffi::lua_touserdatatagged(state, -1, TAG);
            crate::types::ErasedHeader::downcast_ref(res)
        };
        
        (ptr.map(NonNull::from), lua)
    }

    /// `into_with_tag` but with default tag
    #[inline(always)]
    pub fn into<T: 'static>(self) -> Option<TypedUserData<T>> {
        self.into_with_tag::<T, USERDATA2_TAG>()
    }

    /// Turns the userdata immutably into a TypedUserData handle if it is of type `T`
    /// given the userdata has a *tag* of `tag`.
    #[inline(always)]
    pub fn into_with_tag<T: 'static, const TAG: c_int>(self) -> Option<TypedRef<T, Self, TAG>> {
        let (ptr, lua) = self.borrow_to_ptr::<T, TAG>();
        ptr.map(|p| TypedRef::new(lua.0, p, self))
    }

    /// `borrow_with_tag` but with default tag
    #[inline(always)]
    pub fn borrow<T: 'static>(&self) -> Option<UnbackedTypedRef<'_, T>> {
        self.borrow_with_tag::<T, USERDATA2_TAG>()
    }

    /// Same as `into` but returns a unbacked type ref. In most cases, into/into_with_tag is preferable
    #[inline(always)]
    pub fn borrow_with_tag<T: 'static, const TAG: c_int>(&self) -> Option<UnbackedTypedRef<'_, T>> {
        let (ptr, lua) = self.borrow_to_ptr::<T, TAG>();
        ptr.map(|p| UnbackedTypedRef::new(lua.0, p, &self.0))
    }

    /// `type_id_with_tag` but with default tag
    #[inline(always)]
    pub fn type_id(&self) -> Option<TypeId> {
        self.type_id_with_tag::<USERDATA2_TAG>()
    }

    /// Same as `into` but returns a unbacked type ref. In most cases, into/into_with_tag is preferable
    #[inline(always)]
    pub fn type_id_with_tag<const TAG: c_int>(&self) -> Option<TypeId> {
        const { assert_ud_tag::<TAG>() }

        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);

            // Push the userdata onto the stack
            lua.push_ref_at(&self.0, state);

            let res = ffi::lua_touserdatatagged(state, -1, TAG);
            crate::types::ErasedHeader::type_id(res)
        }
    }

    #[inline]
    pub fn get<V: FromLua>(&self, key: impl IntoLua) -> Result<V> {
        // `lua_gettable` method used under the hood can work with any Lua value
        // that has `__index` metamethod
        Table(self.0.clone()).get_protected(key)
    }

    #[inline]
    pub fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        // `lua_settable` method used under the hood can work with any Lua value
        // that has `__newindex` metamethod
        Table(self.0.clone()).set_protected(key, value)
    }

    #[inline]
    pub fn call<R>(&self, args: impl IntoLuaMulti) -> Result<R>
    where
        R: FromLuaMulti,
    {
        Function(self.0.clone()).call(args)
    }

    #[inline]
    pub fn to_string(&self) -> Result<String> {
        Value::UserData(self.clone()).to_string()
    }

    #[inline]
    pub fn weak_lua(&self) -> &WeakLua {
        &self.0.lua
    }

    pub(crate) fn equals(&self, other: &Self) -> Result<bool> {
        // Uses lua_rawequal() under the hood
        if self == other {
            return Ok(true);
        }

        let mt = self.metatable();
        if mt != other.metatable() {
            return Ok(false);
        }

        if let Some(mt) = mt {
            if mt.contains_key("__eq")? {
                return mt.get::<Function>("__eq")?.call((self, other));
            }
        }

        Ok(false)
    }

    /// Returns a type name of this `UserData` (from a metatable field).
    ///
    /// Returns ``None`` if the type name is not set
    pub fn type_name(&self) -> Result<Option<String>> {
        let lua = self.0.lua.lock();
        let state = lua.state();
        unsafe {
            let _sg = StackGuard::new(state);
            check_stack(state, 3)?;

            lua.push_ref_at(&self.0, state);
            let name_type = protect_lua!(state, 1, 1, |state| {
                ffi::luaL_getmetafield(state, -1, c"__type".as_ptr())
            })?;
            match name_type {
                ffi::LUA_TSTRING => Ok(Some(crate::String(lua.pop_ref()).to_str()?.to_owned())),
                _ => Ok(None),
            }
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
}

pub type TypedUserData<T, const TAG: c_int = USERDATA2_TAG> = TypedRef<T, AnyUserData, TAG>;

impl<T: 'static, const TAG: c_int> IntoLua for TypedRef<T, AnyUserData, TAG> {
    fn into_lua(self, _lua: &Lua) -> Result<Value> {
        Ok(Value::UserData(self.ud))
    }
}

impl<T: 'static, const TAG: c_int> crate::FromLua for TypedRef<T, AnyUserData, TAG> {
    fn from_lua(value: crate::Value, _lua: &crate::Lua) -> crate::Result<Self> {
        let from_type = value.type_name(); 

        if let crate::Value::UserData(ud) = value {
            if let Some(typed_ref) = ud.into_with_tag::<T, TAG>() {
                return Ok(typed_ref);
            }
        }

        Err(crate::Error::FromLuaConversionError {
            from: from_type, 
            to: short_type_name::<T>().to_string(),
            message: Some(format!("expected userdata of type {}", short_type_name::<T>())),
        })
    }

    // Fast-path: we can directly use touserdatatagged directly and avoid extra work
    unsafe fn from_specified_stack(idx: std::os::raw::c_int, lua: &crate::state::RawLua, state: *mut ffi::lua_State) -> Result<Self> {
        // Tag safety
        const { assert_ud_tag::<TAG>() }
        let err = || {
            let from = crate::util::lua_type_to_str(ffi::lua_type(state, idx));
            crate::Error::FromLuaConversionError {
                from,
                to: short_type_name::<T>().to_string(),
                message: Some(format!("expected userdata of type {}", short_type_name::<T>())),
            }
        };
        
        let ud_ptr = ffi::lua_touserdatatagged(state, idx, TAG); // returns nullptr if not ud or incorrect tag
        if ud_ptr.is_null() {
            return Err(err())
        }

        if let Some(data_ref) = crate::types::ErasedHeader::downcast_ref::<T>(ud_ptr) {
            return Ok(Self::new(lua.lua().guard().0, NonNull::from(data_ref), AnyUserData(lua.new_value_ref_from(state, idx))))
        }

        Err(err())
    }   
}

/// A untyped userdata pointer from a special Luau callback such as
/// direct field access
pub struct UntypedUserDataPtr<'a> {
    data: *mut c_void,
    _marker: PhantomData<&'a ()>,
}

impl<'a> UntypedUserDataPtr<'a> {
    /// Returns a new UntypedUserDataPtr
    #[inline(always)]
    pub(crate) fn new(data: *mut c_void) -> Self {
        Self { data, _marker: PhantomData }
    }

    /// Returns if the underlying ptr is null or not
    #[inline(always)]
    pub fn is_null(&self) -> bool {
        self.data.is_null()
    }

    /// Turns the userdata immutably into a &'a T handle if it is of type `T`
    #[inline(always)]
    pub fn borrow<T: 'static>(&self) -> Option<&'a T> {
        unsafe { crate::types::ErasedHeader::downcast_ref(self.data) }
    }

    /// Returns the underlying TypeId of the underlying userdata data ptr
    #[inline(always)]
    pub fn type_id(&self) -> Option<TypeId> {
        unsafe { crate::types::ErasedHeader::type_id(self.data) }
    }
}

/// The result from a userdata direct field get callback
/// 
/// Note that not all types from LuaValue are supported here
pub enum UserDataDirectFieldGet {
    /// The Lua value `nil`.
    Nil,
    /// The Lua value `true` or `false`.
    Boolean(bool),
    /// A floating point number.
    Number(Number),
    /// An integer number.
    ///
    /// Any Lua number convertible to a `Integer` will be represented as this variant.
    Integer(Integer),
    /// A luau (64-bit) `integer` (not to be confused with number)
    ///
    /// Note that in `luau`, an 64-bit `integer` is a distinct (and completely unrelated type)
    /// to a number (``Value::Number`` and ``Value::Integer``)
    Int64(i64),
    /// A Luau vector.
    Vector(crate::Vector),
}

impl UserDataDirectFieldGet {
    /* Places the result of the userdata direct field getter into the result TValue ptr */
    pub(crate) unsafe fn finalize(self, result: *mut c_void) {
        match self {
            UserDataDirectFieldGet::Nil => ffi::lua_userdatadirectfield_setnil(result),
            UserDataDirectFieldGet::Boolean(b) => ffi::lua_userdatadirectfield_setboolean(result, b as c_int),
            UserDataDirectFieldGet::Number(n) => ffi::lua_userdatadirectfield_setnumber(result, n),
            UserDataDirectFieldGet::Integer(i) => ffi::lua_userdatadirectfield_setnumber(result, i as Number),
            #[cfg(not(feature = "luau-vector4"))]
            UserDataDirectFieldGet::Vector(v) => ffi::lua_userdatadirectfield_setvector(result, v.x(), v.y(), v.z()),
            #[cfg(feature = "luau-vector4")]
            UserDataDirectFieldGet::Vector(v) => ffi::lua_userdatadirectfield_setvector(result, v.x(), v.y(), v.z(), v.w()),
            UserDataDirectFieldGet::Int64(i) => ffi::lua_userdatadirectfield_setinteger64(result, i),
        }
    }
}