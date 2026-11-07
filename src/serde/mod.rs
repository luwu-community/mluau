//! (De)Serialization support using serde.

use serde::de::DeserializeOwned;
use serde::ser::Serialize;

use crate::error::Result;
use crate::private::Sealed;
use crate::state::Lua;
use crate::util::StackGuard;
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

    /// Builds a [`Value`] from a serde [`Deserializer`] while it parses.
    ///
    /// The result is what `to_value` gives for the same data, but no Rust value is built first: decoding JSON
    /// text with `to_value` needs a `serde_json::Value` of the whole document next to the Lua copy. Strings are
    /// copied once, from the input into Lua.
    ///
    /// [`Deserializer`]: serde::Deserializer
    ///
    /// # Example
    ///
    /// ```
    /// use mluau::{Lua, Result, LuaSerdeExt};
    ///
    /// fn main() -> Result<()> {
    ///     let lua = Lua::new();
    ///     let mut json = serde_json::Deserializer::from_str(r#"{"name": "John Smith", "tags": [1, 2]}"#);
    ///     let value = lua.deserialize_value(&mut json)?;
    ///     lua.globals().set("user", value)?;
    ///     lua.load(r#"
    ///         assert(user.name == "John Smith")
    ///         assert(#user.tags == 2)
    ///     "#).call::<()>(())
    /// }
    /// ```
    fn deserialize_value<'de, D: serde::Deserializer<'de>>(&self, deserializer: D) -> Result<Value>;

    /// Builds a [`Value`] from a serde [`Deserializer`] while it parses, with the options `to_value_with` takes.
    ///
    /// [`Deserializer`]: serde::Deserializer
    fn deserialize_value_with<'de, D: serde::Deserializer<'de>>(
        &self,
        deserializer: D,
        options: ser::Options,
    ) -> Result<Value>;
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

    fn deserialize_value<'de, D: serde::Deserializer<'de>>(&self, deserializer: D) -> Result<Value> {
        self.deserialize_value_with(deserializer, ser::Options::default())
    }

    fn deserialize_value_with<'de, D: serde::Deserializer<'de>>(
        &self,
        deserializer: D,
        options: ser::Options,
    ) -> Result<Value> {
        let lua = self.lock();
        let _sg = unsafe { StackGuard::new(lua.state()) };
        push::push_deserialized(&lua, deserializer, options)
    }
}

pub mod de;
mod push;
pub mod ser;

#[doc(inline)]
pub use de::Deserializer;
#[doc(inline)]
pub use ser::Serializer;
