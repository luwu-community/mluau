//! (De)Serialization support using serde.

use serde::de::DeserializeOwned;
use serde::ser::Serialize;

use crate::error::Result;
use crate::private::Sealed;
use crate::state::Lua;
use crate::value::Value;

/// Trait for serializing/deserializing Lua values using Serde.
#[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
pub trait LuaSerdeExt: Sealed {
    /// Converts `T` into a [`Value`] instance.
    ///
    /// [`Value`]: crate::Value
    ///
    /// # Example
    ///
    /// ```
    /// use mluau::{Lua, Result, LuaSerdeExt};
    /// use serde::Serialize;
    ///
    /// #[derive(Serialize)]
    /// struct User {
    ///     name: String,
    ///     age: u8,
    /// }
    ///
    /// fn main() -> Result<()> {
    ///     let lua = Lua::new();
    ///     let u = User {
    ///         name: "John Smith".into(),
    ///         age: 20,
    ///     };
    ///     lua.globals().set("user", lua.to_value(&u)?)?;
    ///     lua.load(r#"
    ///         assert(user["name"] == "John Smith")
    ///         assert(user["age"] == 20)
    ///     "#).call::<()>(())
    /// }
    /// ```
    fn to_value<T: Serialize + ?Sized>(&self, t: &T) -> Result<Value>;

    /// Converts `T` into a [`Value`] instance with options.
    ///
    /// # Example
    ///
    /// ```
    /// use mluau::{Lua, Result, LuaSerdeExt, SerializeOptions};
    ///
    /// fn main() -> Result<()> {
    ///     let lua = Lua::new();
    ///     let v = vec![1, 2, 3];
    ///     let options = SerializeOptions::new().set_array_metatable(false);
    ///     lua.globals().set("v", lua.to_value_with(&v, options)?)?;
    ///
    ///     lua.load(r#"
    ///         assert(#v == 3 and v[1] == 1 and v[2] == 2 and v[3] == 3)
    ///         assert(getmetatable(v) == nil)
    ///     "#).call::<()>(())
    /// }
    /// ```
    fn to_value_with<T>(&self, t: &T, options: ser::Options) -> Result<Value>
    where
        T: Serialize + ?Sized;

    /// Deserializes a [`Value`] into any serde deserializable object.
    ///
    /// # Example
    ///
    /// ```
    /// use mluau::{Lua, Result, LuaSerdeExt};
    /// use serde::Deserialize;
    ///
    /// #[derive(Deserialize, Debug, PartialEq)]
    /// struct User {
    ///     name: String,
    ///     age: u8,
    /// }
    ///
    /// fn main() -> Result<()> {
    ///     let lua = Lua::new();
    ///     let val = lua.load(r#"{name = "John Smith", age = 20}"#).eval()?;
    ///     let u: User = lua.from_value(val)?;
    ///
    ///     assert_eq!(u, User { name: "John Smith".into(), age: 20 });
    ///
    ///     Ok(())
    /// }
    /// ```
    #[allow(clippy::wrong_self_convention)]
    fn from_value<T: DeserializeOwned>(&self, value: Value) -> Result<T>;

    /// Deserializes a [`Value`] into any serde deserializable object with options.
    ///
    /// # Example
    ///
    /// ```
    /// use mluau::{Lua, Result, LuaSerdeExt, DeserializeOptions};
    /// use serde::Deserialize;
    ///
    /// #[derive(Deserialize, Debug, PartialEq)]
    /// struct User {
    ///     name: String,
    ///     age: u8,
    /// }
    ///
    /// fn main() -> Result<()> {
    ///     let lua = Lua::new();
    ///     let val = lua.load(r#"{name = "John Smith", age = 20, f = function() end}"#).eval()?;
    ///     let options = DeserializeOptions::new().deny_unsupported_types(false);
    ///     let u: User = lua.from_value_with(val, options)?;
    ///
    ///     assert_eq!(u, User { name: "John Smith".into(), age: 20 });
    ///
    ///     Ok(())
    /// }
    /// ```
    #[allow(clippy::wrong_self_convention)]
    fn from_value_with<T: DeserializeOwned>(&self, value: Value, options: de::Options) -> Result<T>;
}

impl LuaSerdeExt for Lua {
    fn to_value<T>(&self, t: &T) -> Result<Value>
    where
        T: Serialize + ?Sized,
    {
        t.serialize(ser::Serializer::new(self))
    }

    fn to_value_with<T>(&self, t: &T, options: ser::Options) -> Result<Value>
    where
        T: Serialize + ?Sized,
    {
        t.serialize(ser::Serializer::new_with_options(self, options))
    }

    fn from_value<T>(&self, value: Value) -> Result<T>
    where
        T: DeserializeOwned,
    {
        T::deserialize(de::Deserializer::new(value))
    }

    fn from_value_with<T>(&self, value: Value, options: de::Options) -> Result<T>
    where
        T: DeserializeOwned,
    {
        T::deserialize(de::Deserializer::new_with_options(value, options))
    }
}

pub mod de;
pub mod ser;

#[doc(inline)]
pub use de::Deserializer;
#[doc(inline)]
pub use ser::Serializer;
