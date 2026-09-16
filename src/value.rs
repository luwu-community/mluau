use std::cmp::Ordering;
use std::collections::HashSet;
use std::os::raw::c_void;
use std::string::String as StdString;
use std::{fmt, ptr, str};

use num_traits::FromPrimitive;

use crate::error::Result;
use crate::function::Function;
use crate::string::{BorrowedStr, String};
use crate::table::Table;
use crate::thread::Thread;
use crate::types::{Integer, LightUserData, Number, ValueRef};
use crate::userdata::AnyUserData;
use crate::util::{check_stack, StackGuard};

#[cfg(feature = "serde")]
use {
    crate::table::SerializableTable,
    rustc_hash::FxHashSet,
    serde::ser::{self, Serialize, Serializer},
    std::{cell::RefCell, rc::Rc, result::Result as StdResult},
};

/// A dynamically typed Lua value.
///
/// The non-primitive variants (eg. string/table/function/thread/userdata) contain handle types
/// into the internal Lua state. It is a logic error to mix handle types between separate
/// `Lua` instances, and doing so will result in a panic.
#[derive(Clone, Default)]
pub enum Value {
    /// The Lua value `nil`.
    #[default]
    Nil,
    /// A Luau none primitive (mluau only)
    #[cfg(any(feature = "none-primitive", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "none-primitive")))]
    None,
    /// The Lua value `true` or `false`.
    Boolean(bool),
    /// A "light userdata" object, equivalent to a raw pointer.
    LightUserData(LightUserData),
    /// An integer number.
    ///
    /// Any Lua number convertible to a `Integer` will be represented as this variant.
    Integer(Integer),
    /// A floating point number.
    Number(Number),
    /// A luau (64-bit) `integer` (not to be confused with number)
    ///
    /// Note that in `luau`, an 64-bit `integer` is a distinct (and completely unrelated type)
    /// to a number (``Value::Number`` and ``Value::Integer``)
    Int64(i64),
    /// A Luau vector.
    Vector(crate::Vector),
    /// An interned string, managed by Lua.
    ///
    /// Unlike Rust strings, Lua strings may not be valid UTF-8.
    String(String),
    /// Reference to a Lua table.
    Table(Table),
    /// Reference to a Lua function (or closure).
    Function(Function),
    /// Reference to a Lua thread (or coroutine).
    Thread(Thread),
    /// Reference to a userdata object that holds a custom type which implements `UserData`.
    ///
    /// Special builtin userdata types will be represented as other `Value` variants.
    UserData(AnyUserData),
    /// A Luau buffer.
    Buffer(crate::Buffer),
    /// A Luau class (the "blueprint" produced by a `class ... end` declaration).
    #[cfg(any(feature = "luwu-classes", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
    Class(crate::Class),
    /// An instance of a Luau [`Class`](crate::Class).
    #[cfg(any(feature = "luwu-classes", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
    Object(crate::Object),
    /// Any other value not known to mlua (eg. LuaJIT CData).
    Other(#[doc(hidden)] ValueRef),
}

pub use self::Value::Nil;

impl Value {
    /// Returns type name of this value.
    pub fn type_name(&self) -> &'static str {
        match *self {
            Value::Nil => "nil",
            #[cfg(any(feature = "none-primitive", doc))]
            Value::None => "none",
            Value::Boolean(_) => "boolean",
            Value::LightUserData(_) => "lightuserdata",
            Value::Integer(_) => "number",
            Value::Number(_) => "number",

            Value::Int64(_) => "integer",

            Value::Vector(_) => "vector",
            Value::String(_) => "string",
            Value::Table(_) => "table",
            Value::Function(_) => "function",
            Value::Thread(_) => "thread",
            Value::UserData(_) => "userdata",

            Value::Buffer(_) => "buffer",

            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Class(_) => "class",
            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Object(_) => "object",
            Value::Other(_) => "other",
        }
    }

    /// Compares two values for equality.
    ///
    /// Equality comparisons do not convert strings to numbers or vice versa.
    /// Tables, functions, threads, and userdata are compared by reference:
    /// two objects are considered equal only if they are the same object.
    ///
    /// If table or userdata have `__eq` metamethod then mlua will try to invoke it.
    /// The first value is checked first. If that value does not define a metamethod
    /// for `__eq`, then mlua will check the second value.
    /// Then mlua calls the metamethod with the two values as arguments, if found.
    pub fn equals(&self, other: &Self) -> Result<bool> {
        match (self, other) {
            (Value::Table(a), Value::Table(b)) => a.equals(b),
            (Value::UserData(a), Value::UserData(b)) => a.equals(b),
            (a, b) => Ok(a == b),
        }
    }

    /// Converts the value to a generic C pointer.
    ///
    /// The value can be a userdata, a table, a thread, a string, or a function; otherwise it
    /// returns NULL. Different objects will give different pointers.
    /// There is no way to convert the pointer back to its original value.
    ///
    /// Typically this function is used only for hashing and debug information.
    #[inline]
    pub fn to_pointer(&self) -> *const c_void {
        match self {
            Value::String(String(vref)) => {
                // In Lua < 5.4 (excluding Luau), string pointers are NULL
                // Use alternative approach
                let lua = vref.lua.lock();
                unsafe { 
                    let state = lua.state();
                    let _sg = crate::util::StackGuard::new(state);
                    lua.push_ref_at(vref, state);
                    ffi::lua_tostring(state, -1) as *const c_void 
                }
            }
            Value::LightUserData(ud) => ud.0,
            Value::Table(Table(vref))
            | Value::Function(Function(vref))
            | Value::Thread(Thread(vref, ..))
            | Value::UserData(AnyUserData(vref))
            | Value::Other(vref) => vref.to_pointer(),

            Value::Buffer(crate::Buffer(vref)) => vref.to_pointer(),

            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Class(crate::Class(vref)) | Value::Object(crate::Object(vref)) => vref.to_pointer(),
            _ => ptr::null(),
        }
    }

    /// Converts the value to a string.
    ///
    /// This might invoke the `__tostring` metamethod for non-primitive types (eg. tables,
    /// functions).
    pub fn to_string(&self) -> Result<StdString> {
        unsafe fn invoke_to_string(vref: &ValueRef) -> Result<StdString> {
            let lua = vref.lua.lock();
            let state = lua.state();
            let _guard = StackGuard::new(state);
            check_stack(state, 3)?;

            lua.push_ref_at(vref, state);
            protect_lua!(state, 1, 1, fn(state) {
                ffi::luaL_tolstring(state, -1, ptr::null_mut());
            })?;
            Ok(String(lua.pop_ref()).to_str()?.to_string())
        }

        match self {
            Value::Nil => Ok("nil".to_string()),
            #[cfg(feature = "none-primitive")]
            Value::None => Ok("none".to_string()),
            Value::Boolean(b) => Ok(b.to_string()),
            Value::LightUserData(ud) => Ok(format!("lightuserdata: {:p}", ud.0)),
            Value::Integer(i) => Ok(i.to_string()),

            Value::Int64(i) => Ok(i.to_string()),
            Value::Number(n) => Ok(n.to_string()),

            Value::Vector(v) => Ok(v.to_string()),
            Value::String(s) => Ok(s.to_str()?.to_string()),
            Value::Table(Table(vref))
            | Value::Function(Function(vref))
            | Value::Thread(Thread(vref, ..))
            | Value::UserData(AnyUserData(vref))
            | Value::Other(vref) => unsafe { invoke_to_string(vref) },

            Value::Buffer(crate::Buffer(vref)) => unsafe { invoke_to_string(vref) },

            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Class(crate::Class(vref)) | Value::Object(crate::Object(vref)) => unsafe {
                invoke_to_string(vref)
            },
        }
    }

    /// Returns `true` if the value is a [`Nil`].
    #[inline]
    pub fn is_nil(&self) -> bool {
        self == &Nil
    }

    /// Returns `true` if the value is a [`None`]
    #[inline]
    #[cfg(any(feature = "none-primitive", doc))]
    pub fn is_none(&self) -> bool {
        self == &Self::None
    }

    /// Returns `true` if the value is a boolean.
    #[inline]
    pub fn is_boolean(&self) -> bool {
        self.as_boolean().is_some()
    }

    /// Cast the value to boolean.
    ///
    /// If the value is a Boolean, returns it or `None` otherwise.
    #[inline]
    pub fn as_boolean(&self) -> Option<bool> {
        match *self {
            Value::Boolean(b) => Some(b),
            _ => None,
        }
    }

    /// Returns `true` if the value is a [`LightUserData`].
    #[inline]
    pub fn is_light_userdata(&self) -> bool {
        self.as_light_userdata().is_some()
    }

    /// Cast the value to [`LightUserData`].
    ///
    /// If the value is a [`LightUserData`], returns it or `None` otherwise.
    #[inline]
    pub fn as_light_userdata(&self) -> Option<LightUserData> {
        match *self {
            Value::LightUserData(l) => Some(l),
            _ => None,
        }
    }

    /// Returns `true` if the value is an [`Integer`].
    #[inline]
    pub fn is_integer(&self) -> bool {
        self.as_integer().is_some()
    }

    /// Cast the value to [`Integer`].
    ///
    /// If the value is a Lua [`Integer`], returns it or `None` otherwise.
    #[inline]
    pub fn as_integer(&self) -> Option<Integer> {
        match *self {
            Value::Integer(i) => Some(i),
            _ => None,
        }
    }

    /// Cast the value to `i32`.
    ///
    /// If the value is a Lua [`Integer`], try to convert it to `i32` or return `None` otherwise.
    #[inline]
    pub fn as_i32(&self) -> Option<i32> {
        #[allow(clippy::useless_conversion)]
        self.as_integer().and_then(|i| i32::try_from(i).ok())
    }

    /// Cast the value to `u32`.
    ///
    /// If the value is a Lua [`Integer`], try to convert it to `u32` or return `None` otherwise.
    #[inline]
    pub fn as_u32(&self) -> Option<u32> {
        self.as_integer().and_then(|i| u32::try_from(i).ok())
    }

    /// Cast the value to `i64`.
    ///
    /// If the value is a Lua [`Integer`], try to convert it to `i64` or return `None` otherwise.
    #[inline]
    pub fn as_i64(&self) -> Option<i64> {
        #[cfg(target_pointer_width = "64")]
        return self.as_integer();
        #[cfg(not(target_pointer_width = "64"))]
        return self.as_integer().map(i64::from);
    }

    /// Cast the value to `u64`.
    ///
    /// If the value is a Lua [`Integer`], try to convert it to `u64` or return `None` otherwise.
    #[inline]
    pub fn as_u64(&self) -> Option<u64> {
        self.as_integer().and_then(|i| u64::try_from(i).ok())
    }

    /// Cast the value to `isize`.
    ///
    /// If the value is a Lua [`Integer`], try to convert it to `isize` or return `None` otherwise.
    #[inline]
    pub fn as_isize(&self) -> Option<isize> {
        self.as_integer().and_then(|i| isize::try_from(i).ok())
    }

    /// Cast the value to `usize`.
    ///
    /// If the value is a Lua [`Integer`], try to convert it to `usize` or return `None` otherwise.
    #[inline]
    pub fn as_usize(&self) -> Option<usize> {
        self.as_integer().and_then(|i| usize::try_from(i).ok())
    }

    /// Returns `true` if the value is a Lua [`Number`].
    #[inline]
    pub fn is_number(&self) -> bool {
        self.as_number().is_some()
    }

    /// Cast the value to [`Number`].
    ///
    /// If the value is a Lua [`Number`], returns it or `None` otherwise.
    #[inline]
    pub fn as_number(&self) -> Option<Number> {
        match *self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// Cast the value to `f32`.
    ///
    /// If the value is a Lua [`Number`], try to convert it to `f32` or return `None` otherwise.
    #[inline]
    pub fn as_f32(&self) -> Option<f32> {
        self.as_number().and_then(f32::from_f64)
    }

    /// Cast the value to `f64`.
    ///
    /// If the value is a Lua [`Number`], try to convert it to `f64` or return `None` otherwise.
    #[inline]
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number()
    }

    /// Returns `true` if the value is a Lua [`String`].
    #[inline]
    pub fn is_string(&self) -> bool {
        self.as_string().is_some()
    }

    /// Cast the value to Lua [`String`].
    ///
    /// If the value is a Lua [`String`], returns it or `None` otherwise.
    #[inline]
    pub fn as_string(&self) -> Option<&String> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Cast the value to [`BorrowedStr`].
    ///
    /// If the value is a Lua [`String`], try to convert it to [`BorrowedStr`] or return `None`
    /// otherwise.
    #[deprecated(
        since = "0.11.0",
        note = "This method does not follow Rust naming convention. Use `as_string().and_then(|s| s.to_str().ok())` instead."
    )]
    #[inline]
    pub fn as_str(&self) -> Option<BorrowedStr<'_>> {
        self.as_string().and_then(|s| s.to_str().ok())
    }

    /// Cast the value to [`StdString`].
    ///
    /// If the value is a Lua [`String`], converts it to [`StdString`] or returns `None` otherwise.
    #[deprecated(
        since = "0.11.0",
        note = "This method does not follow Rust naming convention. Use `as_string().map(|s| s.to_string_lossy())` instead."
    )]
    #[inline]
    pub fn as_string_lossy(&self) -> Option<StdString> {
        self.as_string().map(|s| s.to_string_lossy())
    }

    /// Returns `true` if the value is a Lua [`Table`].
    #[inline]
    pub fn is_table(&self) -> bool {
        self.as_table().is_some()
    }

    /// Cast the value to [`Table`].
    ///
    /// If the value is a Lua [`Table`], returns it or `None` otherwise.
    #[inline]
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Returns `true` if the value is a Lua [`Thread`].
    #[inline]
    pub fn is_thread(&self) -> bool {
        self.as_thread().is_some()
    }

    /// Cast the value to [`Thread`].
    ///
    /// If the value is a Lua [`Thread`], returns it or `None` otherwise.
    #[inline]
    pub fn as_thread(&self) -> Option<&Thread> {
        match self {
            Value::Thread(t) => Some(t),
            _ => None,
        }
    }

    /// Returns `true` if the value is a Lua [`Function`].
    #[inline]
    pub fn is_function(&self) -> bool {
        self.as_function().is_some()
    }

    /// Cast the value to [`Function`].
    ///
    /// If the value is a Lua [`Function`], returns it or `None` otherwise.
    #[inline]
    pub fn as_function(&self) -> Option<&Function> {
        match self {
            Value::Function(f) => Some(f),
            _ => None,
        }
    }

    /// Returns `true` if the value is an [`AnyUserData`].
    #[inline]
    pub fn is_userdata(&self) -> bool {
        self.as_userdata().is_some()
    }

    /// Cast the value to [`AnyUserData`].
    ///
    /// If the value is an [`AnyUserData`], returns it or `None` otherwise.
    #[inline]
    pub fn as_userdata(&self) -> Option<&AnyUserData> {
        match self {
            Value::UserData(ud) => Some(ud),
            _ => None,
        }
    }

    /// Cast the value to a [`Buffer`].
    ///
    /// If the value is [`Buffer`], returns it or `None` otherwise.
    ///
    /// [`Buffer`]: crate::Buffer
    #[inline]
    pub fn as_buffer(&self) -> Option<&crate::Buffer> {
        match self {
            Value::Buffer(b) => Some(b),
            _ => None,
        }
    }

    /// Returns `true` if the value is a [`Buffer`].
    ///
    /// [`Buffer`]: crate::Buffer
    #[inline]
    pub fn is_buffer(&self) -> bool {
        self.as_buffer().is_some()
    }

    /// Cast the value to a [`Class`].
    ///
    /// If the value is a [`Class`], returns it or `None` otherwise.
    ///
    /// [`Class`]: crate::Class
    #[cfg(any(feature = "luwu-classes", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
    #[inline]
    pub fn as_class(&self) -> Option<&crate::Class> {
        match self {
            Value::Class(c) => Some(c),
            _ => None,
        }
    }

    /// Returns `true` if the value is a [`Class`].
    ///
    /// [`Class`]: crate::Class
    #[cfg(any(feature = "luwu-classes", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
    #[inline]
    pub fn is_class(&self) -> bool {
        self.as_class().is_some()
    }

    /// Cast the value to an [`Object`].
    ///
    /// If the value is an [`Object`], returns it or `None` otherwise.
    ///
    /// [`Object`]: crate::Object
    #[cfg(any(feature = "luwu-classes", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
    #[inline]
    pub fn as_object(&self) -> Option<&crate::Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Returns `true` if the value is an [`Object`].
    ///
    /// [`Object`]: crate::Object
    #[cfg(any(feature = "luwu-classes", doc))]
    #[cfg_attr(docsrs, doc(cfg(feature = "luwu-classes")))]
    #[inline]
    pub fn is_object(&self) -> bool {
        self.as_object().is_some()
    }

    /// Wrap reference to this Value into [`SerializableValue`].
    ///
    /// This allows customizing serialization behavior using serde.
    #[cfg(feature = "serde")]
    #[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
    pub fn to_serializable(&self) -> SerializableValue<'_> {
        SerializableValue::new(self, Default::default(), None)
    }

    // Compares two values.
    // Used to sort values for Debug printing.
    pub(crate) fn sort_cmp(&self, other: &Self) -> Ordering {
        fn cmp_num(a: Number, b: Number) -> Ordering {
            match (a, b) {
                _ if a < b => Ordering::Less,
                _ if a > b => Ordering::Greater,
                _ => Ordering::Equal,
            }
        }

        match (self, other) {
            // Nil
            (Value::Nil, Value::Nil) => Ordering::Equal,
            (Value::Nil, _) => Ordering::Less,
            (_, Value::Nil) => Ordering::Greater,
            // None
            #[cfg(feature = "none-primitive")]
            (Value::None, Value::None) => Ordering::Equal,
            #[cfg(feature = "none-primitive")]
            (Value::None, _) => Ordering::Less,
            #[cfg(feature = "none-primitive")]
            (_, Value::None) => Ordering::Greater,

            // Null (a special case)
            (Value::LightUserData(ud1), Value::LightUserData(ud2)) if ud1 == ud2 => Ordering::Equal,
            // Boolean
            (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
            (Value::Boolean(_), _) => Ordering::Less,
            (_, Value::Boolean(_)) => Ordering::Greater,
            // Integer && Number
            (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
            (Value::Integer(a), Value::Number(b)) => cmp_num(*a as Number, *b),
            (Value::Number(a), Value::Integer(b)) => cmp_num(*a, *b as Number),
            (Value::Number(a), Value::Number(b)) => cmp_num(*a, *b),
            // Int64 && Int64
            (Value::Int64(a), Value::Int64(b)) => a.cmp(b),
            (Value::Integer(_) | Value::Number(_), _) => Ordering::Less,
            (_, Value::Integer(_) | Value::Number(_)) => Ordering::Greater,
            // Vector (Luau)
            (Value::Vector(a), Value::Vector(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            // String
            (Value::String(a), Value::String(b)) => a.as_bytes().cmp(&b.as_bytes()),
            (Value::String(_), _) => Ordering::Less,
            (_, Value::String(_)) => Ordering::Greater,
            // Other variants can be ordered by their pointer
            (a, b) => a.to_pointer().cmp(&b.to_pointer()),
        }
    }

    pub(crate) fn fmt_pretty(
        &self,
        fmt: &mut fmt::Formatter,
        recursive: bool,
        ident: usize,
        visited: &mut HashSet<*const c_void>,
    ) -> fmt::Result {
        match self {
            Value::Nil => write!(fmt, "nil"),
            #[cfg(feature = "none-primitive")]
            Value::None => write!(fmt, "none"),
            Value::Boolean(b) => write!(fmt, "{b}"),
            Value::LightUserData(ud) => write!(fmt, "lightuserdata: {:?}", ud.0),
            Value::Integer(i) => write!(fmt, "{i}"),
            Value::Number(n) => write!(fmt, "{n}"),

            Value::Int64(i) => write!(fmt, "{i}"),

            Value::Vector(v) => write!(fmt, "{v}"),
            Value::String(s) => write!(fmt, "{s:?}"),
            Value::Table(t) if recursive && !visited.contains(&t.to_pointer()) => {
                t.fmt_pretty(fmt, ident, visited)
            }
            t @ Value::Table(_) => write!(fmt, "table: {:?}", t.to_pointer()),
            f @ Value::Function(_) => write!(fmt, "function: {:?}", f.to_pointer()),
            t @ Value::Thread(_) => write!(fmt, "thread: {:?}", t.to_pointer()),
            u @ Value::UserData(ud) => {
                // Try `__name/__type` first then `__tostring`
                let name = ud.type_name().ok().flatten();
                let s = name
                    .map(|name| format!("{name}: {:?}", u.to_pointer()))
                    .or_else(|| u.to_string().ok())
                    .unwrap_or_else(|| format!("userdata: {:?}", u.to_pointer()));
                write!(fmt, "{s}")
            }

            buf @ Value::Buffer(_) => write!(fmt, "buffer: {:?}", buf.to_pointer()),
            // Classes have no metatable at all (unlike objects), so `__tostring` never applies
            // and `luaL_tolstring`'s default is just "class: 0x...' -- skip the round-trip call
            // and format the pointer directly.
            #[cfg(any(feature = "luwu-classes", doc))]
            c @ Value::Class(_) => write!(fmt, "class: {:?}", c.to_pointer()),
            // Unlike classes, objects get their class's `instancemetatable`, so they can define
            // `__tostring` -- fall back to the default (pointer-only) format if they don't.
            #[cfg(any(feature = "luwu-classes", doc))]
            o @ Value::Object(_) => {
                let s = o
                    .to_string()
                    .unwrap_or_else(|_| format!("object: {:?}", o.to_pointer()));
                write!(fmt, "{s}")
            }
            Value::Other(v) => write!(fmt, "other: {:?}", v.to_pointer()),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        if fmt.alternate() {
            return self.fmt_pretty(fmt, true, 0, &mut HashSet::new());
        }

        match self {
            Value::Nil => write!(fmt, "Nil"),
            #[cfg(feature = "none-primitive")]
            Value::None => write!(fmt, "None"),
            Value::Boolean(b) => write!(fmt, "Boolean({b})"),
            Value::LightUserData(ud) => write!(fmt, "{ud:?}"),
            Value::Integer(i) => write!(fmt, "Integer({i})"),
            Value::Number(n) => write!(fmt, "Number({n})"),

            Value::Int64(i) => write!(fmt, "Int64({i})"),

            Value::Vector(v) => write!(fmt, "{v:?}"),
            Value::String(s) => write!(fmt, "String({s:?})"),
            Value::Table(t) => write!(fmt, "{t:?}"),
            Value::Function(f) => write!(fmt, "{f:?}"),
            Value::Thread(t) => write!(fmt, "{t:?}"),
            Value::UserData(ud) => write!(fmt, "{ud:?}"),

            Value::Buffer(buf) => write!(fmt, "{buf:?}"),

            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Class(c) => write!(fmt, "{c:?}"),
            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Object(o) => write!(fmt, "{o:?}"),
            Value::Other(v) => write!(fmt, "Other({v:?})"),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            #[cfg(feature = "none-primitive")]
            (Value::None, Value::None) => true,
            (Value::Boolean(a), Value::Boolean(b)) => a == b,
            (Value::LightUserData(a), Value::LightUserData(b)) => a == b,
            (Value::Integer(a), Value::Integer(b)) => *a == *b,
            (Value::Int64(a), Value::Int64(b)) => *a == *b,
            (Value::Integer(a), Value::Number(b)) => *a as Number == *b,
            (Value::Number(a), Value::Integer(b)) => *a == *b as Number,
            (Value::Number(a), Value::Number(b)) => *a == *b,

            (Value::Vector(v1), Value::Vector(v2)) => v1 == v2,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Table(a), Value::Table(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => a == b,
            (Value::Thread(a), Value::Thread(b)) => a == b,
            (Value::UserData(a), Value::UserData(b)) => a == b,

            (Value::Buffer(a), Value::Buffer(b)) => a == b,

            #[cfg(any(feature = "luwu-classes", doc))]
            (Value::Class(a), Value::Class(b)) => a == b,
            #[cfg(any(feature = "luwu-classes", doc))]
            (Value::Object(a), Value::Object(b)) => a == b,
            _ => false,
        }
    }
}

/// A wrapped [`Value`] with customized serialization behavior.
#[cfg(feature = "serde")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub struct SerializableValue<'a> {
    value: &'a Value,
    options: crate::serde::de::Options,
    // In many cases we don't need `visited` map, so don't allocate memory by default
    visited: Option<Rc<RefCell<FxHashSet<*const c_void>>>>,
}

#[cfg(feature = "serde")]
impl Serialize for Value {
    #[inline]
    fn serialize<S: Serializer>(&self, serializer: S) -> StdResult<S::Ok, S::Error> {
        SerializableValue::new(self, Default::default(), None).serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'a> SerializableValue<'a> {
    #[inline]
    pub(crate) fn new(
        value: &'a Value,
        options: crate::serde::de::Options,
        visited: Option<&Rc<RefCell<FxHashSet<*const c_void>>>>,
    ) -> Self {
        if let Value::Table(_) = value {
            return Self {
                value,
                options,
                // We need to always initialize the `visited` map for Tables
                visited: visited.cloned().or_else(|| Some(Default::default())),
            };
        }
        Self {
            value,
            options,
            visited: None,
        }
    }

    /// If true, an attempt to serialize types such as [`Function`], [`Thread`], [`LightUserData`]
    /// and [`Error`] will cause an error.
    /// Otherwise these types skipped when iterating or serialized as unit type.
    ///
    /// Default: **true**
    #[must_use]
    pub fn deny_unsupported_types(mut self, enabled: bool) -> Self {
        self.options.deny_unsupported_types = enabled;
        self
    }

    /// If true, an attempt to serialize a recursive table (table that refers to itself)
    /// will cause an error.
    /// Otherwise subsequent attempts to serialize the same table will be ignored.
    ///
    /// Default: **true**
    #[must_use]
    pub fn deny_recursive_tables(mut self, enabled: bool) -> Self {
        self.options.deny_recursive_tables = enabled;
        self
    }

    /// If true, keys in tables will be iterated (and serialized) in sorted order.
    ///
    /// Default: **false**
    #[must_use]
    pub fn sort_keys(mut self, enabled: bool) -> Self {
        self.options.sort_keys = enabled;
        self
    }

    /// If true, empty Lua tables will be encoded as array, instead of map.
    ///
    /// Default: **false**
    #[must_use]
    pub fn encode_empty_tables_as_array(mut self, enabled: bool) -> Self {
        self.options.encode_empty_tables_as_array = enabled;
        self
    }

    /// If true, enable detection of mixed tables.
    ///
    /// A mixed table is a table that has both array-like and map-like entries or several borders.
    ///
    /// Default: **false**
    #[must_use]
    pub fn detect_mixed_tables(mut self, enabled: bool) -> Self {
        self.options.detect_mixed_tables = enabled;
        self
    }
}

#[cfg(feature = "serde")]
impl Serialize for SerializableValue<'_> {
    fn serialize<S>(&self, serializer: S) -> StdResult<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.value {
            Value::Nil => serializer.serialize_unit(),
            #[cfg(feature = "none-primitive")]
            Value::None => serializer.serialize_none(),
            Value::Boolean(b) => serializer.serialize_bool(*b),
            #[allow(clippy::useless_conversion)]
            Value::Integer(i) => serializer.serialize_i64((*i).into()),
            #[allow(clippy::useless_conversion)]
            Value::Int64(i) => serializer.serialize_i64((*i).into()),
            Value::Number(n) => serializer.serialize_f64(*n),

            Value::Vector(v) => v.serialize(serializer),
            Value::String(s) => s.serialize(serializer),
            Value::Table(t) => {
                let visited = self.visited.as_ref().unwrap().clone();
                SerializableTable::new(t, self.options, visited).serialize(serializer)
            }

            Value::Buffer(buf) => buf.serialize(serializer),
            Value::Function(_)
            | Value::Thread(_)
            | Value::UserData(_)
            | Value::LightUserData(_)
            | Value::Other(_) => {
                if self.options.deny_unsupported_types {
                    let msg = format!("cannot serialize <{}>", self.value.type_name());
                    Err(ser::Error::custom(msg))
                } else {
                    serializer.serialize_unit()
                }
            }
            #[cfg(any(feature = "luwu-classes", doc))]
            Value::Class(_) | Value::Object(_) => {
                if self.options.deny_unsupported_types {
                    let msg = format!("cannot serialize <{}>", self.value.type_name());
                    Err(ser::Error::custom(msg))
                } else {
                    serializer.serialize_unit()
                }
            }
        }
    }
}

#[cfg(test)]
mod assertions {
    use super::*;

    static_assertions::assert_not_impl_any!(Value: Send);
}
