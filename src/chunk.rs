use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::CString;
use std::io::Result as IoResult;
use std::panic::Location;
use std::string::String as StdString;

use crate::{FromLuaErr, IntoLuaMulti};
use crate::error::{Error, Result};
use crate::function::Function;
use crate::state::{Lua, WeakLua};
use crate::table::Table;
use crate::traits::{FromLuaMulti, IntoLua};
use crate::value::Value;

/// Trait for types [loadable by Lua] and convertible to a [`Chunk`]
///
/// Prefer [`ChunkSource`] over this trait. `AsChunk` is kept mainly so `&str`/`String` literals
/// can still be passed to [`Lua::load`] directly; anything else should use `ChunkSource`.
///
/// # Safety
///
/// Implementer is responsible for ensuring `source()` is valid/trusted bytecode whenever
/// `mode()` returns `Some(ChunkMode::Binary)`.
///
/// [loadable by Lua]: https://www.lua.org/manual/5.4/manual.html#3.3.2
pub unsafe trait AsChunk {
    /// Returns optional chunk name
    ///
    /// See [`Chunk::set_name`] for possible name prefixes.
    fn name(&self) -> Option<StdString> {
        None
    }

    /// Returns optional chunk [environment]
    ///
    /// [environment]: https://www.lua.org/manual/5.4/manual.html#2.2
    fn environment(&self, lua: &Lua) -> Result<Option<Table>> {
        let _lua = lua; // suppress warning
        Ok(None)
    }

    /// Returns optional chunk mode (text or binary)
    fn mode(&self) -> ChunkMode;

    /// Returns chunk data (can be text or binary)
    fn source<'a>(&self) -> IoResult<Cow<'a, [u8]>>
    where
        Self: 'a;
}

unsafe impl AsChunk for &str {
    fn mode(&self) -> ChunkMode {
        ChunkMode::Text
    }

    fn source<'a>(&self) -> IoResult<Cow<'a, [u8]>>
    where
        Self: 'a,
    {
        Ok(Cow::Borrowed(self.as_bytes()))
    }
}

unsafe impl AsChunk for StdString {
    fn mode(&self) -> ChunkMode {
        ChunkMode::Text
    }

    fn source<'a>(&self) -> IoResult<Cow<'a, [u8]>> {
        Ok(Cow::Owned(self.clone().into_bytes()))
    }
}

unsafe impl AsChunk for &StdString {
    fn mode(&self) -> ChunkMode {
        ChunkMode::Text
    }

    fn source<'a>(&self) -> IoResult<Cow<'a, [u8]>>
    where
        Self: 'a,
    {
        Ok(Cow::Borrowed(self.as_bytes()))
    }
}

unsafe impl<C: AsChunk + ?Sized> AsChunk for Box<C> {
    fn name(&self) -> Option<StdString> {
        (**self).name()
    }

    fn environment(&self, lua: &Lua) -> Result<Option<Table>> {
        (**self).environment(lua)
    }

    fn mode(&self) -> ChunkMode {
        (**self).mode()
    }

    fn source<'a>(&self) -> IoResult<Cow<'a, [u8]>>
    where
        Self: 'a,
    {
        (**self).source()
    }
}

/// Private marker that gates construction of [`ChunkSource::Bytecode`].
///
/// It has no public constructor, so the only way to build a `ChunkSource::Bytecode` from outside
/// this crate is through the `unsafe fn` [`ChunkSource::bytecode`], which requires the caller to
/// assert the safety precondition. Downstream code can still pattern-match the variant freely
/// (e.g. `ChunkSource::Bytecode(bytes, _)`); it just can't construct one without going through the
/// unsafe constructor.
#[doc(hidden)]
pub struct ChunkSourceBytecodeSealed(());

/// An owned chunk of Lua/Luau code, explicitly tagged as either source text or precompiled
/// bytecode.
///
/// Prefer this over [`AsChunk`] for anything other than a plain string literal. Passing raw bytes
/// to [`Lua::load`] is ambiguous: is this buffer source code, or bytecode?
/// Guessing wrong (or trusting a caller-supplied guess) can segfault, because binary chunks are
/// not validated the way source text is. `ChunkSource` makes the choice explicit at the type
/// level: [`ChunkSource::Src`] is always safe to build, while [`ChunkSource::Bytecode`] can only
/// be built via the `unsafe fn` [`ChunkSource::bytecode`].
///
/// The optional chunk name (used in error messages/tracebacks) is set with the [`name`] or
/// [`path`] builder methods; neither reads from the filesystem, unlike the old `AsChunk` impls for
/// `&Path`/`PathBuf` this type replaces. Callers who want to load a file must read it themselves
/// (e.g. via `std::fs::read`/`std::fs::read_to_string`) and pass the contents in.
///
/// [`Lua::load`]: crate::Lua::load
/// [`name`]: ChunkSource::name
/// [`path`]: ChunkSource::path
pub enum ChunkSource<'a> {
    /// Lua/Luau source code (text).
    Src(Cow<'a, str>, Option<StdString>),
    /// Precompiled bytecode. Only constructible via [`ChunkSource::bytecode`].
    Bytecode(Cow<'a, [u8]>, Option<StdString>, ChunkSourceBytecodeSealed),
}

impl<'a> ChunkSource<'a> {
    /// Wraps `source` as a [`ChunkSource::Src`], with no name set.
    pub fn src(source: impl Into<Cow<'a, str>>) -> Self {
        ChunkSource::Src(source.into(), None)
    }

    /// Wraps `bytecode` as a [`ChunkSource::Bytecode`], with no name set.
    ///
    /// # Safety
    ///
    /// `bytecode` must be valid Luau bytecode (e.g. produced by [`Compiler::compile`] or
    /// [`Function::dump`](crate::Function::dump)). Passing arbitrary or corrupted bytes here can
    /// cause undefined behavior, including segfaults, when the resulting chunk is loaded.
    pub unsafe fn bytecode(bytecode: impl Into<Cow<'a, [u8]>>) -> Self {
        ChunkSource::Bytecode(bytecode.into(), None, ChunkSourceBytecodeSealed(()))
    }

    /// Sets a custom chunk name, used as-is in error messages/tracebacks.
    ///
    /// See [`Chunk::set_name`] for the meaning of the `@`/`=` name prefixes; this method does not
    /// apply either automatically. Use [`path`](ChunkSource::path) if the name is a file path.
    #[must_use]
    pub fn name(self, name: impl Into<StdString>) -> Self {
        let name = Some(name.into());
        match self {
            ChunkSource::Src(source, _) => ChunkSource::Src(source, name),
            ChunkSource::Bytecode(bytecode, _, sealed) => ChunkSource::Bytecode(bytecode, name, sealed),
        }
    }

    /// Sets the chunk name from a file path, applying Lua's `@` file-path prefix convention.
    #[must_use]
    pub fn path(self, path: impl std::fmt::Display) -> Self {
        self.name(format!("@{path}"))
    }
}

unsafe impl AsChunk for ChunkSource<'_> {
    fn name(&self) -> Option<StdString> {
        match self {
            ChunkSource::Src(_, name) => name.clone(),
            ChunkSource::Bytecode(_, name, _) => name.clone(),
        }
    }

    fn mode(&self) -> ChunkMode {
        match self {
            ChunkSource::Src(..) => ChunkMode::Text,
            ChunkSource::Bytecode(..) => ChunkMode::Binary,
        }
    }

    fn source<'b>(&self) -> IoResult<Cow<'b, [u8]>>
    where
        Self: 'b,
    {
        Ok(match self {
            ChunkSource::Src(cow, _) => match cow {
                Cow::Borrowed(s) => Cow::Borrowed((*s).as_bytes()),
                Cow::Owned(s) => Cow::Owned(s.clone().into_bytes()),
            },
            ChunkSource::Bytecode(cow, _, _) => match cow {
                Cow::Borrowed(b) => Cow::Borrowed(*b),
                Cow::Owned(b) => Cow::Owned(b.clone()),
            },
        })
    }
}

/// Returned from [`Lua::load`] and is used to finalize loading and executing Lua main chunks.
#[must_use = "`Chunk`s do nothing unless one of `exec`, `eval`, `call`, or `into_function` are called on them"]
pub struct Chunk<'a> {
    pub(crate) lua: WeakLua,
    pub(crate) name: StdString,
    pub(crate) env: Result<Option<Table>>,
    pub(crate) mode: ChunkMode,
    pub(crate) source: IoResult<Cow<'a, [u8]>>,
    pub(crate) compiler: Option<Compiler>,
    /// Set by `compile` once it has replaced `source` with bytecode it just produced
    /// itself. Lets `load_chunk` skip the binary/text sniff in `luaL_loadbufferenv`,
    /// which only guesses from the leading byte (the bytecode version number) and
    /// can misclassify legitimate bytecode as text once that version reaches the
    /// range of common leading whitespace bytes (e.g. version 10 == b'\n').
    /// Distinct from `mode`: `mode` is what the caller *claims* the data is (and must
    /// still be validated, e.g. by `test_load_mode`), this is what *we* know it is.
    pub(crate) trusted_binary: bool,
}

/// Represents chunk mode (text or binary).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkMode {
    Text,
    Binary,
}

/// Represents a constant value that can be used by Luau compiler.
#[derive(Clone, Debug)]
pub enum CompileConstant {
    Nil,
    Boolean(bool),
    Number(crate::Number),
    Vector(crate::Vector),
    String(StdString),
}

impl From<bool> for CompileConstant {
    fn from(b: bool) -> Self {
        CompileConstant::Boolean(b)
    }
}

impl From<crate::Number> for CompileConstant {
    fn from(n: crate::Number) -> Self {
        CompileConstant::Number(n)
    }
}

impl From<crate::Vector> for CompileConstant {
    fn from(v: crate::Vector) -> Self {
        CompileConstant::Vector(v)
    }
}

impl From<&str> for CompileConstant {
    fn from(s: &str) -> Self {
        CompileConstant::String(s.to_owned())
    }
}

type LibraryMemberConstantMap = HashMap<(StdString, StdString), CompileConstant>;

/// Luau compiler
#[derive(Clone, Debug)]
pub struct Compiler {
    optimization_level: u8,
    debug_level: u8,
    type_info_level: u8,
    coverage_level: u8,
    vector_lib: Option<StdString>,
    vector_ctor: Option<StdString>,
    vector_type: Option<StdString>,
    mutable_globals: Vec<StdString>,
    userdata_types: Vec<StdString>,
    libraries_with_known_members: Vec<StdString>,
    library_constants: Option<LibraryMemberConstantMap>,
    disabled_builtins: Vec<StdString>,
}

impl Default for Compiler {
    fn default() -> Self {
        const { Self::new() }
    }
}

impl Compiler {
    /// Creates Luau compiler instance with default options
    pub const fn new() -> Self {
        // Defaults are taken from luacode.h
        Compiler {
            optimization_level: 1,
            debug_level: 1,
            type_info_level: 0,
            coverage_level: 0,
            vector_lib: None,
            vector_ctor: None,
            vector_type: None,
            mutable_globals: Vec::new(),
            userdata_types: Vec::new(),
            libraries_with_known_members: Vec::new(),
            library_constants: None,
            disabled_builtins: Vec::new(),
        }
    }

    /// Sets Luau compiler optimization level.
    ///
    /// Possible values:
    /// * 0 - no optimization
    /// * 1 - baseline optimization level that doesn't prevent debuggability (default)
    /// * 2 - includes optimizations that harm debuggability such as inlining
    #[must_use]
    pub const fn set_optimization_level(mut self, level: u8) -> Self {
        self.optimization_level = level;
        self
    }

    /// Sets Luau compiler debug level.
    ///
    /// Possible values:
    /// * 0 - no debugging support
    /// * 1 - line info & function names only; sufficient for backtraces (default)
    /// * 2 - full debug info with local & upvalue names; necessary for debugger
    #[must_use]
    pub const fn set_debug_level(mut self, level: u8) -> Self {
        self.debug_level = level;
        self
    }

    /// Sets Luau type information level used to guide native code generation decisions.
    ///
    /// Possible values:
    /// * 0 - generate for native modules (default)
    /// * 1 - generate for all modules
    #[must_use]
    pub const fn set_type_info_level(mut self, level: u8) -> Self {
        self.type_info_level = level;
        self
    }

    /// Sets Luau compiler code coverage level.
    ///
    /// Possible values:
    /// * 0 - no code coverage support (default)
    /// * 1 - statement coverage
    /// * 2 - statement and expression coverage (verbose)
    #[must_use]
    pub const fn set_coverage_level(mut self, level: u8) -> Self {
        self.coverage_level = level;
        self
    }

    /// Sets alternative global builtin to construct vectors, in addition to default builtin
    /// `vector.create`.
    ///
    /// To set the library and method name, use the `lib.ctor` format.
    #[doc(hidden)]
    #[must_use]
    pub fn set_vector_ctor(mut self, ctor: impl Into<StdString>) -> Self {
        let ctor = ctor.into();
        let lib_ctor = ctor.split_once('.');
        self.vector_lib = lib_ctor.as_ref().map(|&(lib, _)| lib.to_owned());
        self.vector_ctor = (lib_ctor.as_ref())
            .map(|&(_, ctor)| ctor.to_owned())
            .or(Some(ctor));
        self
    }

    /// Sets alternative vector type name for type tables, in addition to default type `vector`.
    #[doc(hidden)]
    #[must_use]
    pub fn set_vector_type(mut self, r#type: impl Into<StdString>) -> Self {
        self.vector_type = Some(r#type.into());
        self
    }

    /// Adds a mutable global.
    ///
    /// It disables the import optimization for fields accessed through it.
    #[must_use]
    pub fn add_mutable_global(mut self, global: impl Into<StdString>) -> Self {
        self.mutable_globals.push(global.into());
        self
    }

    /// Sets a list of globals that are mutable.
    ///
    /// It disables the import optimization for fields accessed through these.
    #[must_use]
    pub fn set_mutable_globals<S: Into<StdString>>(mut self, globals: impl IntoIterator<Item = S>) -> Self {
        self.mutable_globals = globals.into_iter().map(|s| s.into()).collect();
        self
    }

    /// Adds a userdata type to the list that will be included in the type information.
    #[must_use]
    pub fn add_userdata_type(mut self, r#type: impl Into<StdString>) -> Self {
        self.userdata_types.push(r#type.into());
        self
    }

    /// Sets a list of userdata types that will be included in the type information.
    #[must_use]
    pub fn set_userdata_types<S: Into<StdString>>(mut self, types: impl IntoIterator<Item = S>) -> Self {
        self.userdata_types = types.into_iter().map(|s| s.into()).collect();
        self
    }

    /// Adds a constant for a known library member.
    ///
    /// The constants are used by the compiler to optimize the generated bytecode.
    /// Optimization level must be at least 2 for this to have any effect.
    ///
    /// The `name` is a string in the format `lib.member`, where `lib` is the library name
    /// and `member` is the member (constant) name.
    #[must_use]
    pub fn add_library_constant(
        mut self,
        name: impl AsRef<str>,
        r#const: impl Into<CompileConstant>,
    ) -> Self {
        let Some((lib, member)) = name.as_ref().split_once('.') else {
            return self;
        };
        let (lib, member) = (lib.to_owned(), member.to_owned());

        if !self.libraries_with_known_members.contains(&lib) {
            self.libraries_with_known_members.push(lib.clone());
        }
        self.library_constants
            .get_or_insert_with(HashMap::new)
            .insert((lib, member), r#const.into());
        self
    }

    /// Adds a builtin that should be disabled.
    #[must_use]
    pub fn add_disabled_builtin(mut self, builtin: impl Into<StdString>) -> Self {
        self.disabled_builtins.push(builtin.into());
        self
    }

    /// Sets a list of builtins that should be disabled.
    #[must_use]
    pub fn set_disabled_builtins<S: Into<StdString>>(
        mut self,
        builtins: impl IntoIterator<Item = S>,
    ) -> Self {
        self.disabled_builtins = builtins.into_iter().map(|s| s.into()).collect();
        self
    }

    /// Compiles the `source` into bytecode.
    ///
    /// Returns [`Error::SyntaxError`] if the source code is invalid.
    pub fn compile(&self, source: impl AsRef<[u8]>) -> Result<Vec<u8>> {
        use std::cell::RefCell;
        use std::ffi::CStr;
        use std::os::raw::{c_char, c_int};
        use std::ptr;

        let vector_lib = self.vector_lib.clone();
        let vector_lib = vector_lib.and_then(|lib| CString::new(lib).ok());
        let vector_lib = vector_lib.as_ref();
        let vector_ctor = self.vector_ctor.clone();
        let vector_ctor = vector_ctor.and_then(|ctor| CString::new(ctor).ok());
        let vector_ctor = vector_ctor.as_ref();
        let vector_type = self.vector_type.clone();
        let vector_type = vector_type.and_then(|t| CString::new(t).ok());
        let vector_type = vector_type.as_ref();

        macro_rules! vec2cstring_ptr {
            ($name:ident, $name_ptr:ident) => {
                let $name = self
                    .$name
                    .iter()
                    .map(|name| CString::new(name.clone()).ok())
                    .collect::<Option<Vec<_>>>()
                    .unwrap_or_default();
                let mut $name = $name.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
                let mut $name_ptr = ptr::null();
                if !$name.is_empty() {
                    $name.push(ptr::null());
                    $name_ptr = $name.as_ptr();
                }
            };
        }

        vec2cstring_ptr!(mutable_globals, mutable_globals_ptr);
        vec2cstring_ptr!(userdata_types, userdata_types_ptr);
        vec2cstring_ptr!(libraries_with_known_members, libraries_with_known_members_ptr);
        vec2cstring_ptr!(disabled_builtins, disabled_builtins_ptr);

        thread_local! {
            static LIBRARY_MEMBER_CONSTANT_MAP: RefCell<LibraryMemberConstantMap> = Default::default();
        }

        unsafe extern "C-unwind" fn library_member_constant_callback(
            library: *const c_char,
            member: *const c_char,
            constant: *mut ffi::lua_CompileConstant,
        ) {
            let library = CStr::from_ptr(library).to_string_lossy();
            let member = CStr::from_ptr(member).to_string_lossy();
            LIBRARY_MEMBER_CONSTANT_MAP.with_borrow(|map| {
                if let Some(cons) = map.get(&(library.to_string(), member.to_string())) {
                    match cons {
                        CompileConstant::Nil => ffi::luau_set_compile_constant_nil(constant),
                        CompileConstant::Boolean(b) => {
                            ffi::luau_set_compile_constant_boolean(constant, *b as c_int)
                        }
                        CompileConstant::Number(n) => ffi::luau_set_compile_constant_number(constant, *n),
                        CompileConstant::Vector(v) => {
                            #[cfg(not(feature = "luau-vector4"))]
                            ffi::luau_set_compile_constant_vector(constant, v.x(), v.y(), v.z(), 0.0);
                            #[cfg(feature = "luau-vector4")]
                            ffi::luau_set_compile_constant_vector(constant, v.x(), v.y(), v.z(), v.w());
                        }
                        CompileConstant::String(s) => ffi::luau_set_compile_constant_string(
                            constant,
                            s.as_ptr() as *const c_char,
                            s.len(),
                        ),
                    }
                }
            })
        }

        let bytecode = unsafe {
            let mut options = ffi::lua_CompileOptions::default();
            options.optimizationLevel = self.optimization_level as c_int;
            options.debugLevel = self.debug_level as c_int;
            options.typeInfoLevel = self.type_info_level as c_int;
            options.coverageLevel = self.coverage_level as c_int;
            options.vectorLib = vector_lib.map_or(ptr::null(), |s| s.as_ptr());
            options.vectorCtor = vector_ctor.map_or(ptr::null(), |s| s.as_ptr());
            options.vectorType = vector_type.map_or(ptr::null(), |s| s.as_ptr());
            options.mutableGlobals = mutable_globals_ptr;
            options.userdataTypes = userdata_types_ptr;
            options.librariesWithKnownMembers = libraries_with_known_members_ptr;
            if let Some(map) = self.library_constants.as_ref() {
                if !self.libraries_with_known_members.is_empty() {
                    LIBRARY_MEMBER_CONSTANT_MAP.with_borrow_mut(|gmap| *gmap = map.clone());
                    options.libraryMemberConstantCallback = Some(library_member_constant_callback);
                }
            }
            options.disabledBuiltins = disabled_builtins_ptr;
            ffi::luau_compile(source.as_ref(), options)
        };

        if bytecode.first() == Some(&0) {
            // The rest of the bytecode is the error message starting with `:`
            // See https://github.com/luau-lang/luau/blob/0.640/Compiler/src/Compiler.cpp#L4336
            let message = StdString::from_utf8_lossy(&bytecode[2..]).into_owned();
            return Err(Error::SyntaxError {
                incomplete_input: message.ends_with("<eof>"),
                message,
            });
        }

        Ok(bytecode)
    }
}

impl Chunk<'_> {
    /// Returns the name of this chunk.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sets the name of this chunk, which results in more informative error traces.
    ///
    /// Possible name prefixes:
    /// - `@` - file path (when truncation is needed, the end of the file path is kept, as this is
    ///   more useful for identifying the file)
    /// - `=` - custom chunk name (when truncation is needed, the beginning of the name is kept)
    pub fn set_name(mut self, name: impl Into<StdString>) -> Self {
        self.name = name.into();
        self
    }

    /// Returns the environment of this chunk.
    pub fn environment(&self) -> Option<&Table> {
        self.env.as_ref().ok()?.as_ref()
    }

    /// Sets the environment of the loaded chunk to the given value.
    ///
    /// In Lua >=5.2 main chunks always have exactly one upvalue, and this upvalue is used as the
    /// `_ENV` variable inside the chunk. By default this value is set to the global environment.
    ///
    /// Calling this method changes the `_ENV` upvalue to the value provided, and variables inside
    /// the chunk will refer to the given environment rather than the global one.
    ///
    /// All global variables (including the standard library!) are looked up in `_ENV`, so it may be
    /// necessary to populate the environment in order for scripts using custom environments to be
    /// useful.
    pub fn set_environment(mut self, env: Table) -> Self {
        self.env = Ok(Some(env));
        self
    }

    /// Returns the mode of this chunk.
    pub fn mode(&self) -> ChunkMode {
        self.mode
    }

    /// Sets or overwrites a Luau compiler used for this chunk.
    ///
    /// See [`Compiler`] for details and possible options.
    pub fn set_compiler(mut self, compiler: Compiler) -> Self {
        self.compiler = Some(compiler);
        self
    }

    /// Evaluate the chunk as either an expression or block.
    ///
    /// If the chunk can be parsed as an expression, this loads and executes the chunk and returns
    /// the value that it evaluates to. Otherwise, the chunk is interpreted as a block as normal,
    /// and this is equivalent to calling `exec`.
    pub fn eval<R: FromLuaMulti>(self) -> Result<R> {
        self.eval_with_err::<R, crate::Error>()
    }

    /// Same as eval but supports custom error values
    pub fn eval_with_err<R: FromLuaMulti, E: FromLuaErr>(self) -> std::result::Result<R, E> {
        // Bytecode is always interpreted as a statement.
        // For source code, first try interpreting the lua as an expression by adding
        // "return", then as a statement. This is the same thing the
        // actual lua repl does.
        if self.mode == ChunkMode::Binary {
            self.into_function().map_err(E::from_rust_err)?.call_with_err::<R, E>(())
        } else if let Ok(function) = self.to_expression() {
            function.call_with_err::<R, E>(())
        } else {
            self.into_function().map_err(E::from_rust_err)?.call_with_err::<R, E>(())
        }
    }

    /// Load the chunk function and call it with the given arguments.
    ///
    /// This is equivalent to `into_function` and calling the resulting function.
    pub fn call<R: FromLuaMulti>(self, args: impl IntoLuaMulti) -> Result<R> {
        self.call_with_err::<R, crate::Error>(args)
    }

    /// Same as call but supports custom error values
    pub fn call_with_err<R: FromLuaMulti, E: FromLuaErr>(self, args: impl IntoLuaMulti) -> std::result::Result<R, E> {
        self.into_function().map_err(E::from_rust_err)?.call_with_err::<R, E>(args)
    }

    /// Load this chunk into a regular [`Function`].
    ///
    /// This simply compiles the chunk without actually executing it.
    pub fn into_function(mut self) -> Result<Function> {
        if self.compiler.is_some() {
            // We don't need to compile source if no compiler set
            self.compile();
        }

        let name = Self::convert_name(self.name)?;
        self.lua.lock().load_chunk(
            Some(&name),
            self.env?.as_ref(),
            self.mode,
            self.source.map_err(crate::Error::runtime)?.as_ref(),
            self.trusted_binary,
        )
    }

    /// Compiles the chunk and changes mode to binary.
    ///
    /// It does nothing if the chunk is already binary or invalid.
    fn compile(&mut self) {
        if let Ok(ref source) = self.source {
            if self.mode == ChunkMode::Text {
                if let Ok(data) = self.compiler.get_or_insert_with(Default::default).compile(source) {
                    self.source = Ok(Cow::Owned(data));
                    self.mode = ChunkMode::Binary;
                    self.trusted_binary = true;
                }
            }
        }
    }

    /// Fetches compiled bytecode of this chunk from the cache.
    ///
    /// If not found, compiles the source code and stores it on the cache.
    pub fn try_cache(mut self) -> Self {
        struct ChunksCache(HashMap<Vec<u8>, Vec<u8>>);

        // Try to fetch compiled chunk from cache
        let mut text_source = None;
        if let Ok(ref source) = self.source {
            if self.mode == ChunkMode::Text {
                let lua = self.lua.lock();
                if let Some(cache) = lua.priv_app_data_ref::<ChunksCache>() {
                    if let Some(data) = cache.0.get(source.as_ref()) {
                        self.source = Ok(Cow::Owned(data.clone()));
                        self.mode = ChunkMode::Binary;
                        self.trusted_binary = true;
                        return self;
                    }
                }
                text_source = Some(source.as_ref().to_vec());
            }
        }

        // Compile and cache the chunk
        if let Some(text_source) = text_source {
            self.compile();
            if let Ok(ref binary_source) = self.source {
                if self.mode == ChunkMode::Binary {
                    let lua = self.lua.lock();
                    if let Some(mut cache) = lua.priv_app_data_mut::<ChunksCache>() {
                        cache.0.insert(text_source, binary_source.to_vec());
                    } else {
                        let mut cache = ChunksCache(HashMap::new());
                        cache.0.insert(text_source, binary_source.to_vec());
                        lua.set_priv_app_data(cache);
                    };
                }
            }
        }

        self
    }

    fn to_expression(&self) -> Result<Function> {
        if matches!(self.mode, ChunkMode::Binary) {
            return Err(Error::RuntimeError(String::from(
                "Expression should be a chunk of source code, not compiled bytecode",
            )));
        }

        // We assume that mode is Text
        let source = self.source.as_ref();
        let source = source.map_err(Error::runtime)?;
        let source = Self::expression_source(source);
        // We don't need to compile source if no compiler options set
        let compiled = self.compiler.as_ref().map(|c| c.compile(&source)).transpose()?;

        let trusted_binary = compiled.is_some();
        let source = compiled.unwrap_or(source);

        let name = Self::convert_name(self.name.clone())?;
        let env = match &self.env {
            Ok(Some(env)) => Some(env),
            Ok(None) => None,
            Err(err) => return Err(err.clone()),
        };

        self.lua
            .lock()
            .load_chunk(Some(&name), env, ChunkMode::Text, &source, trusted_binary)
    }

    fn convert_name(name: StdString) -> Result<CString> {
        CString::new(name).map_err(|err| Error::runtime(format!("invalid name: {err}")))
    }

    fn expression_source(source: &[u8]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(b"return ".len() + source.len());
        buf.extend(b"return ");
        buf.extend(source);
        buf
    }
}

struct WrappedChunk<T: AsChunk> {
    chunk: T,
    caller: &'static Location<'static>,
}

impl Chunk<'_> {
    /// Wraps a chunk of Lua code, returning an opaque type that implements [`IntoLua`] trait.
    ///
    /// The resulted `IntoLua` implementation will convert the chunk into a Lua function without
    /// executing it.
    #[doc(hidden)]
    #[track_caller]
    pub fn wrap(chunk: impl AsChunk) -> impl IntoLua {
        WrappedChunk {
            chunk,
            caller: Location::caller(),
        }
    }
}

impl<T: AsChunk> IntoLua for WrappedChunk<T> {
    fn into_lua(self, lua: &Lua) -> Result<Value> {
        lua.load_with_location(self.chunk, self.caller)
            .into_function()
            .map(Value::Function)
    }
}
