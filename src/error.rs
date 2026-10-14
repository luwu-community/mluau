use std::error::Error as StdError;
use std::fmt;
use std::result::Result as StdResult;
use std::string::String as StdString;
use std::sync::Arc;

/// Error type returned by `mlua` methods.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Error {
    /// Syntax error while parsing Lua source code.
    SyntaxError {
        /// The error message as returned by Lua.
        message: StdString,
        /// `true` if the error can likely be fixed by appending more input to the source code.
        ///
        /// This is useful for implementing REPLs as they can query the user for more input if this
        /// is set.
        incomplete_input: bool,
    },
    /// Lua runtime error, aka `LUA_ERRRUN`.
    ///
    /// The Lua VM returns this error when a builtin operation is performed on incompatible types.
    /// Among other things, this includes invoking operators on wrong types (such as calling or
    /// indexing a `nil` value).
    RuntimeError(StdString),
    /// Lua memory error, aka `LUA_ERRMEM`
    ///
    /// The Lua VM returns this error when the allocator does not return the requested memory, aka
    /// it is an out-of-memory error.
    MemoryError(StdString),
    /// Potentially unsafe action in safe mode.
    SafetyError(StdString),
    /// Memory control is not available.
    ///
    /// This error can only happen when Lua state was not created by us and does not have the
    /// custom allocator attached.
    MemoryControlNotAvailable,
    /// A mutable callback has triggered Lua code that has called the same mutable callback again.
    ///
    /// This is an error because a mutable callback can only be borrowed mutably once.
    RecursiveMutCallback,
    /// Not enough stack space to place arguments to Lua functions or return values from callbacks.
    ///
    /// Due to the way `mlua` works, it should not be directly possible to run out of stack space
    /// during normal use. The only way that this error can be triggered is if a `Function` is
    /// called with a huge number of arguments, or a Rust callback returns a huge number of return
    /// values.
    StackError,
    /// Too many arguments to [`Function::bind`].
    ///
    /// [`Function::bind`]: crate::Function::bind
    BindError,
    /// Bad argument received from Lua (usually when calling a function).
    ///
    /// This error can help to identify the argument that caused the error
    /// (which is stored in the corresponding field).
    BadArgument {
        /// Function that was called.
        to: Option<StdString>,
        /// Argument position (usually starts from 1).
        pos: usize,
        /// Argument name.
        name: Option<StdString>,
        /// Underlying error returned when converting argument to a Lua value.
        cause: Arc<Error>,
    },
    /// A Rust value could not be converted to a Lua value.
    ToLuaConversionError {
        /// Name of the Rust type that could not be converted.
        from: String,
        /// Name of the Lua type that could not be created.
        to: &'static str,
        /// A message indicating why the conversion failed in more detail.
        message: Option<StdString>,
    },
    /// A Lua value could not be converted to the expected Rust type.
    FromLuaConversionError {
        /// Name of the Lua type that could not be converted.
        from: &'static str,
        /// Name of the Rust type that could not be created.
        to: String,
        /// A string containing more detailed error information.
        message: Option<StdString>,
    },
    /// [`Thread::resume`] was called on an unresumable coroutine.
    ///
    /// A coroutine is unresumable if its main function has returned or if an error has occurred
    /// inside the coroutine. Already running coroutines are also marked as unresumable.
    ///
    /// [`Thread::status`] can be used to check if the coroutine can be resumed without causing this
    /// error.
    ///
    /// [`Thread::resume`]: crate::Thread::resume
    /// [`Thread::status`]: crate::Thread::status
    CoroutineUnresumable,

    /// Serialization error.
    #[cfg(feature = "serde")]
    #[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
    SerializeError(StdString),
    /// Deserialization error.
    #[cfg(feature = "serde")]
    #[cfg_attr(docsrs, doc(cfg(feature = "serde")))]
    DeserializeError(StdString),
    /// A custom error.
    ///
    /// This can be used for returning user-defined errors from callbacks.
    ///
    /// Returning `Err(ExternalError(...))` from a Rust callback will raise the error as a Lua
    /// error.
    ExternalError(StdString),
    UnsupportedLanguage,
}

/// A specialized `Result` type used by `mlua`'s API.
pub type Result<T> = StdResult<T, Error>;

#[cfg(not(tarpaulin_include))]
impl fmt::Display for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::SyntaxError { message, .. } => write!(fmt, "syntax error: {message}"),
            Error::RuntimeError(msg) => write!(fmt, "runtime error: {msg}"),
            Error::MemoryError(msg) => {
                write!(fmt, "memory error: {msg}")
            }
            Error::SafetyError(msg) => {
                write!(fmt, "safety error: {msg}")
            },
            Error::MemoryControlNotAvailable => {
                write!(fmt, "memory control is not available")
            }
            Error::RecursiveMutCallback => write!(fmt, "mutable callback called recursively"),
            Error::StackError => write!(
                fmt,
                "out of Lua stack, too many arguments to a Lua function or too many return values from a callback"
            ),
            Error::BindError => write!(
                fmt,
                "too many arguments to Function::bind"
            ),
            Error::BadArgument { to, pos, name, cause } => {
                if let Some(name) = name {
                    write!(fmt, "bad argument `{name}`")?;
                } else {
                    write!(fmt, "bad argument #{pos}")?;
                }
                if let Some(to) = to {
                    write!(fmt, " to `{to}`")?;
                }
                write!(fmt, ": {cause}")
            },
            Error::ToLuaConversionError { from, to, message } => {
                write!(fmt, "error converting {from} to Lua {to}")?;
                match message {
                    None => Ok(()),
                    Some(message) => write!(fmt, " ({message})"),
                }
            }
            Error::FromLuaConversionError { from, to, message } => {
                write!(fmt, "error converting Lua {from} to {to}")?;
                match message {
                    None => Ok(()),
                    Some(message) => write!(fmt, " ({message})"),
                }
            }
            Error::CoroutineUnresumable => write!(fmt, "coroutine is non-resumable"),
            #[cfg(feature = "serde")]
            Error::SerializeError(err) => {
                write!(fmt, "serialize error: {err}")
            },
            #[cfg(feature = "serde")]
            Error::DeserializeError(err) => {
                write!(fmt, "deserialize error: {err}")
            },
            Error::ExternalError(err) => err.fmt(fmt),
            Error::UnsupportedLanguage => {
                write!(fmt, "mluau doesn't support this programming language (we only accept Lua, Luau, and Luwu)")
            }
        }
    }
}

impl StdError for Error {}

impl Error {
    /// Creates a new `RuntimeError` with the given message.
    #[inline]
    pub fn runtime<S: fmt::Display>(message: S) -> Self {
        Error::RuntimeError(message.to_string())
    }

    /// Wraps an external error object.
    #[inline]
    pub fn external<T: Into<Box<dyn StdError + Send + Sync>>>(err: T) -> Self {
        Error::ExternalError(err.into().to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::external(err)
    }
}

#[cfg(feature = "serde")]
impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::SerializeError(msg.to_string())
    }
}

#[cfg(feature = "serde")]
impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::DeserializeError(msg.to_string())
    }
}

#[cfg(test)]
mod assertions {
    use super::*;
    static_assertions::assert_impl_all!(Error: Send, Sync);
}
