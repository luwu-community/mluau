use std::borrow::Cow;
use std::ffi::c_int;
use std::{collections::HashMap, rc::Rc};
use std::marker::PhantomData;

use rustc_hash::{FxBuildHasher, FxHashMap};

use crate::types::UnbackedTypedRef;
use crate::util::short_type_name;
use crate::{AnyUserData, CallbackResult, FromLua, FromLuaMulti, IntoCallbackResult, IntoLua, Lua, MultiValue, Table, TypedUserData, USERDATA2_TAG, Value};

/// Kinds of metamethods that can be overridden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MetaMethod;

#[allow(non_upper_case_globals)]
impl MetaMethod {
    /// The `+` operator.
    pub const Add: &'static str = "__add";
    
    /// The `-` operator.
    pub const Sub: &'static str = "__sub";
    
    /// The `*` operator.
    pub const Mul: &'static str = "__mul";
    
    /// The `/` operator.
    pub const Div: &'static str = "__div";
    
    /// The `%` operator.
    pub const Mod: &'static str = "__mod";
    
    /// The `^` operator.
    pub const Pow: &'static str = "__pow";
    
    /// The unary minus (`-`) operator.
    pub const Unm: &'static str = "__unm";
    
    /// The floor division (//) operator.
    pub const IDiv: &'static str = "__idiv";
    
    /// The string concatenation operator `..`.
    pub const Concat: &'static str = "__concat";
    
    /// The length operator `#`.
    pub const Len: &'static str = "__len";
    
    /// The `==` operator.
    pub const Eq: &'static str = "__eq";
    
    /// The `<` operator.
    pub const Lt: &'static str = "__lt";
    
    /// The `<=` operator.
    pub const Le: &'static str = "__le";
    
    /// Index access `obj[key]`.
    pub const Index: &'static str = "__index";
    
    /// Index write access `obj[key] = value`.
    pub const NewIndex: &'static str = "__newindex";
    
    /// The call "operator" `obj(arg1, args2, ...)`.
    pub const Call: &'static str = "__call";
    
    /// The `__tostring` metamethod.
    ///
    /// This is not an operator, but will be called by methods such as `tostring` and `print`.
    pub const ToString: &'static str = "__tostring";
    
    /// The `__iter` metamethod.
    ///
    /// Executed before the iteration begins, and should return an iterator function like `next`
    /// (or a custom one).
    pub const Iter: &'static str = "__iter";
    
    /// The `__type` metafield.
    ///
    /// This is not a function, but it's value can be used by `tostring` and `typeof` built-in
    /// functions.
    pub const Type: &'static str = "__type";
}

/// Method registry for [`UserData`] implementors.
pub trait UserDataMethods<T, const TAG: c_int = USERDATA2_TAG> {
    /// Add a regular method which accepts a `&T` as the first parameter.
    ///
    /// Regular methods are implemented by overriding the `__index` metamethod and returning the
    /// accessed method. This allows them to be used with the expected `userdata:method()` syntax.
    ///
    /// If `add_meta_method` is used to set the `__index` metamethod, the `__index` metamethod will
    /// be used as a fall-back if no regular method is found.
    fn add_method<M, A, R>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult;

    /// Add a regular function which accepts generic arguments.
    ///
    /// The first argument will be a [`AnyUserData`] of type `T` if the method if it is passed in as 
    /// the first argument: `my_userdata.my_method(my_userdata, arg1, arg2)`.
    fn add_function<F, A, R>(&mut self, name: impl Into<&'static str>, function: F) where
        F: Fn(&Lua, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult;

    /// Add a metamethod which accepts a `&T` as the first parameter.
    fn add_meta_method<M, A, R>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult;

    /// Add a metamethod which accepts generic arguments.
    ///
    /// Metamethods for binary operators can be triggered if either the left or right argument to
    /// the binary operator has a metatable, so the first argument here is not necessarily a
    /// userdata of type `T`.
    fn add_meta_function<F, A, R>(&mut self, name: impl Into<&'static str>, function: F)
    where
        F: Fn(&Lua, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult;
}

/// Field registry for [`UserData`] implementors.
pub trait UserDataFields<T, const TAG: c_int = USERDATA2_TAG> {
    /// Add a static field to the [`UserData`].
    ///
    /// Static fields are implemented by updating the `__index` metamethod and returning the
    /// accessed field. This allows them to be used with the expected `userdata.field` syntax.
    ///
    /// Static fields are usually shared between all instances of the [`UserData`] of the same type.
    /// 
    /// Note: __index is not an allowed name here for performance purposes, use userdata v2 low-level API instead for that
    fn add_field<V>(&mut self, name: impl Into<&'static str>, value: V)
    where
        V: IntoLua + 'static;

    /// Add a metatable field.
    ///
    /// This will initialize the metatable field with `value` on [`UserData`] creation.
    /// 
    /// Note: __index is not an allowed name here for performance purposes, use userdata v2 low-level API instead for that
    fn add_meta_field<V>(&mut self, name: impl Into<&'static str>, value: V)
    where
        V: IntoLua + 'static;

    /// Add a regular field getter as a method which accepts a `&T` as the parameter.
    ///
    /// Regular field getters are implemented by overriding the `__index` metamethod and returning
    /// the accessed field. This allows them to be used with the expected `userdata.field` syntax.
    fn add_field_method_get<M, R>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>) -> R + 'static,
        R: IntoCallbackResult;

    /// Add a regular field setter as a method which accepts a `&T` as the parameter.
    ///
    /// Regular field setters are implemented by overriding the `__newindex` metamethod and returning
    /// the accessed field. This allows them to be used with the expected `userdata.field` syntax.
    fn add_field_method_set<M, A>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>, A) -> Result<(), crate::Error> + 'static,
        A: FromLua;
}

/// Trait for custom userdata types.
///
/// By implementing this trait, a struct becomes eligible for use inside Lua code.
///
/// Implementation of [`IntoLua`] is automatically provided, Use `TypedUserData<T>` for a [`FromLua`] implementation
/// 
/// Deoptimization notes:
///
/// There are certain cases that may deoptimize userdata accesses into slower paths automatically:
/// - Disabling namecall (forces __index then call as two separate accesses)
/// - Adding a __index metamethod via meta-method (forces func __index to allow fallback to custom __index)
/// - Field getters (forces func __index to allow dynamic call to field getter)
/// 
/// Mutable userdata can be created by defining [`UserDataMut`] instead of this trait. Note that this costs about 8 bytes extra. and is slower
/// than immutable userdata as it wraps every userdata access behind a RefCell meaning all normal RefCell guidance apply (incl. but not limited to
/// never holding the mutable ref to `T` beyond a await point)
pub trait UserData<const TAG: c_int = USERDATA2_TAG>: 'static + Sized {
    /// Whether or not to use __namecall optimization
    /// 
    /// When using namecall optimization:
    /// - Methods must only be added using "add_method". Otherwise method syntax will not work for them
    const USE_NAMECALL: bool = true;

    /// Type name
    fn type_name<'a>() -> Cow<'a, str> {
        Cow::Owned(short_type_name::<Self>())
    }

    /// Adds custom fields specific to this userdata.
    #[allow(unused_variables)]
    fn add_fields<F: UserDataFields<Self, TAG>>(fields: &mut F) {}

    /// Adds custom methods and operators specific to this userdata.
    #[allow(unused_variables)]
    fn add_methods<M: UserDataMethods<Self, TAG>>(methods: &mut M) {}
}

type MethodCb<T, const TAG: c_int> = Rc<dyn Fn(&Lua, TypedUserData<T, TAG>, MultiValue) -> CallbackResult + 'static>;
type FieldGetter<T, const TAG: c_int> = Rc<dyn Fn(&Lua, TypedUserData<T, TAG>) -> CallbackResult + 'static>;
type FieldSetter<T, const TAG: c_int> = Rc<dyn Fn(&Lua, TypedUserData<T, TAG>, Value) -> CallbackResult + 'static>;

struct UserDataRegistry<'a, const TAG: c_int, T: UserData<TAG>> {
    lua: &'a Lua,

    // __index props
    index_mt_props: FxHashMap<&'static str, crate::Result<Value>>,
    // main mt props
    mt_props: FxHashMap<&'static str, crate::Result<Value>>,
    
    // special cases
    namecall_methods: FxHashMap<&'static str, MethodCb<T, TAG>>, // Namecall methods (special cases that need namecall optimization)
    namecall_fb: Option<MethodCb<T, TAG>>, // a fallback namecall metamethod
    index_fb: Option<MethodCb<T, TAG>>, // a fallback index metamethod
    newindex_fb: Option<MethodCb<T, TAG>>, // a fallback newindex metamethod
    field_method_get: FxHashMap<&'static str, FieldGetter<T, TAG>>, // fallback index metamethod
    field_method_set: FxHashMap<&'static str, FieldSetter<T, TAG>> // newindex
}

impl<'a, const TAG: c_int, T: UserData<TAG>> UserDataRegistry<'a, TAG, T> {
    /// Sets up the metatable for a ud
    fn new_metatable(lua: &'a Lua) -> crate::Result<Table> {
        let mut reg = Self {
            lua,
            index_mt_props: FxHashMap::default(),
            mt_props: FxHashMap::default(),
            namecall_methods: FxHashMap::default(),
            namecall_fb: None,
            index_fb: None,
            newindex_fb: None,
            field_method_get: FxHashMap::default(),
            field_method_set: FxHashMap::default()
        };

        // Collect into reg
        T::add_fields(&mut reg);
        T::add_methods(&mut reg);

        reg.finalize_metatable()
    }

    fn finalize_metatable(self) -> crate::Result<Table> {
        // Metatable vecs, we can then build the 2 tables for __index and main mt in one shot later
        let mut mt = Vec::with_capacity(self.mt_props.len());

        // Setup __index as either table or fn
        let index_as_fn = self.index_fb.is_some() || !self.field_method_get.is_empty();
        let index = if index_as_fn {
            // Build up the index_mt_props hashmap with values resolved
            let index_mt_props = Rc::new({
                let mut new_index_mt_props = HashMap::with_capacity_and_hasher(self.index_mt_props.len(), FxBuildHasher);
                for (k, v) in self.index_mt_props {
                    new_index_mt_props.insert(k, v?);
                }
                new_index_mt_props
            });
            let index_fb = self.index_fb;
            let field_getters = self.field_method_get;
            let indexfn = self.lua.create_function_with_debug(move |lua, (ud, key): (TypedUserData<T, TAG>, crate::String)| {
                let key_str = match key.to_str() {
                    Ok(k) => k,
                    Err(e) => return CallbackResult::LuaError(e)
                };
                // Case 1: prop is in index_mt_props directly
                if let Some(prop) = index_mt_props.get(key_str.as_ref()) {
                    return CallbackResult::OkSingle(prop.clone())
                }
                // Case 2: field getter
                if let Some(ref getter) = field_getters.get(key_str.as_ref()) {
                    return getter(lua, ud)
                }
                // Lastly, check for custom index
                if let Some(ref ifb) = index_fb {
                    return ifb(lua, ud, MultiValue::from_iter([Value::String(key)]))
                }
                CallbackResult::OkSingle(Value::Nil)
            }, Some(c"__index"))?;

            Value::Function(indexfn)
        } else {
            let mut indexmt = Vec::with_capacity(self.index_mt_props.len());
            for (k, v) in self.index_mt_props {
                indexmt.push((k, v?));
            }
            indexmt.push(("__metatable", Value::Boolean(false)));
            let indextab = self.lua.create_table_from(indexmt)?;
            indextab.set_readonly(true);
            Value::Table(indextab)
        };

        // Setup main __mt
        for (k, v) in self.mt_props {
            mt.push((k, v?));
        }
        let mt = self.lua.create_table_from(mt)?;
        
        mt.set("__index", index)?;

        if !self.field_method_set.is_empty() || self.newindex_fb.is_some() {
            mt.set("__newindex", Self::newindex_cb(self.lua, Rc::new(self.field_method_set), self.newindex_fb)?)?;
        }
        
        // Inject __type if not explicitly set
        if !mt.raw_get("__type")? {
            mt.set("__type", T::type_name())?;
        }

        if T::USE_NAMECALL {
            let namecall_cbs = Rc::new(self.namecall_methods);
            mt.set("__namecall", Self::namecall_cb(self.lua, namecall_cbs, self.namecall_fb)?)?;
        }

        mt.set("__metatable", false)?;
        mt.set_readonly(true);

        Ok(mt)
    }

    fn namecall_cb(lua: &Lua, namecall_methods: Rc<FxHashMap<&'static str, MethodCb<T, TAG>>>, namecall_fb: Option<MethodCb<T, TAG>>) -> crate::Result<crate::Function> {
        lua.create_function(move |lua, (ud, args): (TypedUserData<T, TAG>, MultiValue)| {
            match lua.with_namecall(|method| -> Result<_, crate::Error> {
                let s = method.ok_or_else(|| crate::Error::external("internal error: no method set for namecall"))?.to_str().map_err(crate::Error::external)?;
                match namecall_methods.get(s) {
                    Some(s) => Ok(s.clone()),
                    None => {
                        if let Some(fb) = &namecall_fb {
                            Ok(fb.clone())   
                        } else {
                            Err(crate::Error::runtime(format!("{}: cannot find method `{s}`", T::type_name())))
                        }
                    }
                }
            }) {
                Ok(call) => call(lua, ud, args),
                Err(e) => CallbackResult::LuaError(e)
            }
        })
    }

    fn newindex_cb(lua: &Lua, field_setters: Rc<FxHashMap<&'static str, FieldSetter<T, TAG>>>, newindex_fb: Option<MethodCb<T, TAG>>) -> crate::Result<crate::Function> {
        lua.create_function(move |lua, (ud, key, arg): (TypedUserData<T, TAG>, crate::String, Value)| {
            let key_str = match key.to_str() {
                Ok(k) => k,
                Err(e) => return CallbackResult::LuaError(e)
            };
            match field_setters.get(key_str.as_ref()) {
                Some(s) => {
                    s(lua, ud, arg)
                },
                None => {
                    if let Some(fb) = &newindex_fb {
                        fb(lua, ud, MultiValue::from_iter([Value::String(key)]))
                    } else {
                        CallbackResult::LuaError(crate::Error::runtime(format!("{}: cannot find property `{}`", T::type_name(), key_str)))
                    }
                }
            }
        })
    }

    // TODO: Make this re-entrant/multiple threads safe

    /// The metatable every `T` shares, made on first use
    fn metatable(lua: &'a Lua) -> crate::Result<Table> {
        struct Ud<T> {
            tab: Table,
            _phantom: PhantomData<T>
        }
        if let Some(tab) = lua.try_app_data_ref::<Ud<T>>().map_err(crate::Error::external)? {
            return Ok(tab.tab.clone());
        }

        let mt = Self::new_metatable(lua)?;
        lua.set_app_data(Ud {
            tab: mt.clone(),
            _phantom: PhantomData::<T>
        });
        Ok(mt)
    }

    /// Creates the actual userdata w/ shared metatable for all `T` of same type
    fn create_userdata(lua: &'a Lua, data: T) -> crate::Result<AnyUserData> {
        let mt = Self::metatable(lua)?;
        lua.create_any_userdata_with_tag::<_, TAG>(data, Some(&mt))
    }

    /// Pushes a userdata of `data` onto `state` without making a ref, for a value that goes straight to Lua
    unsafe fn push_userdata_at(lua: &'a crate::state::RawLua, state: *mut crate::ffi::lua_State, data: T) -> crate::Result<()> {
        let mt = Self::metatable(lua.lua())?;
        lua.push_any_userdata_at::<T, TAG>(state, data, Some(&mt))
    }
}

impl<const TAG: c_int, T: UserData<TAG>> UserDataFields<T, TAG> for UserDataRegistry<'_, TAG, T> {
    fn add_field<V>(&mut self, name: impl Into<&'static str>, value: V)
    where V: IntoLua + 'static 
    {
        self.index_mt_props.insert(name.into(), value.into_lua(self.lua));
    }

    fn add_meta_field<V>(&mut self, name: impl Into<&'static str>, value: V)
    where
    V: IntoLua + 'static 
    {
        self.mt_props.insert(name.into(), value.into_lua(self.lua));
    }

    fn add_field_method_get<M, R>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>) -> R + 'static,
        R: IntoCallbackResult
    {
        self.field_method_get.insert(name.into(), wrap_field_getter::<TAG, T, M, R>(method));
    }

    fn add_field_method_set<M, A>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>, A) -> Result<(), crate::Error> + 'static,
        A: FromLua
    {
        self.field_method_set.insert(name.into(), wrap_field_setter::<TAG, T, A, M>(method));
    }
}

impl<const TAG: c_int, T: UserData<TAG>> UserDataMethods<T, TAG> for UserDataRegistry<'_, TAG, T> {
    fn add_function<F, A, R>(&mut self, name: impl Into<&'static str>, function: F) where
    F: Fn(&Lua, A) -> R + 'static,
    A: FromLuaMulti,
    R: IntoCallbackResult 
    {
        self.index_mt_props.insert(name.into(), self.lua.create_function(function).map(Value::Function));
    }

    fn add_method<M, A, R>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult 
    {
        // Methods are a bit special due to __namecall optimization which normal functions don't get
        //
        // If namecall optimization is enabled, we need to wrap methods to get a namecall repr for __namecall optimization
        let wrapped = wrap_method(method);
        let name = name.into();
        self.namecall_methods.insert(name, wrapped.clone());
        self.index_mt_props.insert(name, self.lua.create_function(move |lua: &Lua, (ud, args): (TypedUserData<T, TAG>, MultiValue)| {
            wrapped(lua, ud, args)
        }).map(Value::Function));
    }

    fn add_meta_function<F, A, R>(&mut self, name: impl Into<&'static str>, function: F) where
    F: Fn(&Lua, A) -> R + 'static,
    A: FromLuaMulti,
    R: IntoCallbackResult 
    {
        let name = name.into();
        match name {
            "__index" => panic!("__index must be registered as a meta method with add_meta_method"),
            "__namecall" => panic!("__namecall must be registered as a meta method with add_meta_method"),
            _ => self.mt_props.insert(name.into(), self.lua.create_function(function).map(Value::Function))
        };
    }

    fn add_meta_method<M, A, R>(&mut self, name: impl Into<&'static str>, method: M)
    where
        M: Fn(&Lua, TypedUserData<T, TAG>, A) -> R + 'static,
        A: FromLuaMulti,
        R: IntoCallbackResult 
    {
        let name = name.into();
        match name {
            "__index" => self.index_fb = Some(wrap_method(method)),
            "__namecall" => self.namecall_fb = Some(wrap_method(method)),
            _ => { self.mt_props.insert(name, self.lua.create_function(move |lua, (ud, args)| method(lua, ud, args)).map(Value::Function)); }
        };
    }
}

impl<T: UserData> IntoLua for T {
    #[inline]
    fn into_lua(self, lua: &Lua) -> crate::Result<Value> {
        let ud = UserDataRegistry::create_userdata(lua, self)?;
        Ok(Value::UserData(ud))
    }

    #[inline]
    unsafe fn push_into_specified_stack(self, lua: &crate::state::RawLua, state: *mut crate::ffi::lua_State) -> crate::Result<()> {
        UserDataRegistry::push_userdata_at(lua, state, self)
    }
}

pub trait LuaUserDataExt {
    /// Creates a immutable userdata with the associated data `T`
    fn create_userdata<const TAG: c_int, T: UserData<TAG>>(&self, data: T) -> crate::Result<AnyUserData>;
}

impl LuaUserDataExt for Lua {
    fn create_userdata<const TAG: c_int, T: UserData<TAG>>(&self, data: T) -> crate::Result<AnyUserData> {
        let ud = UserDataRegistry::create_userdata(self, data)?;
        Ok(ud)
    }
}

pub trait UserDataBorrowExt {
    fn try_borrow<const TAG: c_int, T: UserData<TAG>>(&self) -> crate::Result<UnbackedTypedRef<'_, T>>;
    fn with_ref<const TAG: c_int, T: UserData<TAG>, R>(
        &self, 
        f: impl FnOnce(&T) -> R
    ) -> crate::Result<R>;
}

impl UserDataBorrowExt for AnyUserData {
    fn try_borrow<const TAG: c_int, T: UserData<TAG>>(&self) -> crate::Result<UnbackedTypedRef<'_, T>> {
        match self.borrow_with_tag::<T, TAG>() {
            Some(tref) => Ok(tref),
            None => Err(crate::Error::FromLuaConversionError { from: "userdata", to: T::type_name().to_string(), message: None })
        }
    }
    fn with_ref<const TAG: c_int, T: UserData<TAG>, R>(
        &self, 
        f: impl FnOnce(&T) -> R
    ) -> crate::Result<R> {
        let tref = self.borrow_with_tag::<T, TAG>()
            .ok_or_else(|| crate::Error::FromLuaConversionError { 
                from: "userdata", 
                to: T::type_name().to_string(), 
                message: None 
            })?;
        
        Ok(f(&tref))
    }
}

// Helpers for wrapping fn's/cb's

#[inline]
/// Wraps a method into a type-erased MethodCb<T>
pub(super) fn wrap_method<const TAG: c_int, T, M, A, R>(method: M) -> MethodCb<T, TAG>
where
    T: UserData<TAG>,
    M: Fn(&Lua, TypedUserData<T, TAG>, A) -> R + 'static,
    A: FromLuaMulti,
    R: IntoCallbackResult,
{
    Rc::new(move |lua, this, args| {
        let args = match A::from_lua_multi(args, lua) {
            Ok(a) => a,
            Err(e) => return CallbackResult::LuaError(e),
        };

        method(lua, this, args).into_callback_result(lua)
    })
}

#[inline]
/// Wraps a field getter into a type-erased FieldGetter<T>
pub(super) fn wrap_field_getter<const TAG: c_int, T, M, R>(method: M) -> FieldGetter<T, TAG>
where
    T: UserData<TAG>,
    M: Fn(&Lua, TypedUserData<T, TAG>) -> R + 'static,
    R: IntoCallbackResult,
{
    Rc::new(move |lua, this| {
        // Call method and normalize
        method(lua, this).into_callback_result(lua)
    })
}

#[inline]
/// Wraps a field setter into a type-erased FieldSetter<T>
pub(super) fn wrap_field_setter<const TAG: c_int, T, A, M>(method: M) -> FieldSetter<T, TAG>
where
    T: UserData<TAG>,
    M: Fn(&Lua, TypedUserData<T, TAG>, A) -> Result<(), crate::Error> + 'static,
    A: FromLua,
{
    Rc::new(move |lua, this, value| {
        let args = match A::from_lua(value, lua) {
            Ok(a) => a,
            Err(e) => return CallbackResult::LuaError(e),
        };

        method(lua, this, args).into_callback_result(lua)
    })
}
