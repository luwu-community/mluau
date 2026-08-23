use std::{fs, io};

use mluau::{Chunk, ChunkMode, ChunkSource, Lua, Result};

#[test]
fn test_chunk_methods() -> Result<()> {
    let lua = Lua::new();

    #[cfg(unix)]
    assert!(lua.load("return 123").name().starts_with("@tests/chunk.rs"));
    let chunk2 = lua.load("return 123").set_name("@new_name");
    assert_eq!(chunk2.name(), "@new_name");

    let env = lua.create_table_from([("a", 987)])?;
    let chunk3 = lua.load("return a").set_environment(env.clone());
    assert_eq!(chunk3.environment().unwrap(), &env);
    assert_eq!(chunk3.mode(), ChunkMode::Text);
    assert_eq!(chunk3.call::<i32>(())?, 987);

    Ok(())
}

#[test]
#[cfg(not(target_os = "wasi"))]
fn test_chunk_path() -> Result<()> {
    use std::env::temp_dir;

    let lua = Lua::new();

    if cfg!(target_arch = "wasm32") {
        // TODO: figure out why emscripten fails on file operations
        // Also see https://github.com/rust-lang/rust/issues/119250
        return Ok(());
    }

    let tmp_dir = temp_dir();
    fs::write(
        tmp_dir.join("module.lua"),
        r#"
        return 321
    "#,
    )?;
    let module_path = tmp_dir.join("module.lua");
    let source = fs::read_to_string(&module_path)?;
    let i: i32 = lua
        .load(ChunkSource::src(source).path(module_path.display()))
        .eval()?;
    assert_eq!(i, 321);

    match fs::read_to_string(tmp_dir.join("module2.lua")) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        res => panic!("expected io::Error, got {:?}", res),
    };

    Ok(())
}

#[test]
fn test_chunk_impls() -> Result<()> {
    let lua = Lua::new();

    // StdString
    assert_eq!(lua.load(String::from("1")).eval::<i32>()?, 1);
    assert_eq!(lua.load(&String::from("2")).eval::<i32>()?, 2);

    // ChunkSource::Src
    assert_eq!(lua.load(ChunkSource::src("3")).eval::<i32>()?, 3);

    // ChunkSource::bytecode
    let bytecode = mluau::Compiler::new().compile("return 4")?;
    // SAFETY: bytecode was just produced by `Compiler::compile` above
    assert_eq!(
        lua.load(unsafe { ChunkSource::bytecode(bytecode) })
            .eval::<i32>()?,
        4
    );

    Ok(())
}


#[test]
fn test_compiler() -> Result<()> {
    let compiler = mluau::Compiler::new()
        .set_optimization_level(2)
        .set_debug_level(2)
        .set_type_info_level(1)
        .set_coverage_level(2)
        .set_vector_ctor("vector.new")
        .set_vector_type("vector")
        .set_mutable_globals(["mutable_global"])
        .set_userdata_types(["MyUserdata"])
        .set_disabled_builtins(["tostring"]);

    assert!(compiler.compile("return tostring(vector.new(1, 2, 3))").is_ok());

    // Error
    match compiler.compile("%") {
        Err(mluau::Error::SyntaxError { ref message, .. }) => {
            assert!(message.contains("Expected identifier when parsing expression, got '%'"),);
        }
        res => panic!("expected result: {res:?}"),
    }

    Ok(())
}

#[test]
fn test_compiler_library_constants() {
    use mluau::{Compiler, Vector};

    let compiler = Compiler::new()
        .set_optimization_level(2)
        .add_library_constant("mylib.const_bool", true)
        .add_library_constant("mylib.const_num", 123.0)
        .add_library_constant("mylib.const_vec", Vector::zero())
        .add_library_constant("mylib.const_str", "value1");

    let lua = Lua::new();
    lua.set_compiler(compiler);
    let const_bool = lua.load("return mylib.const_bool").eval::<bool>().unwrap();
    assert_eq!(const_bool, true);
    let const_num = lua.load("return mylib.const_num").eval::<f64>().unwrap();
    assert_eq!(const_num, 123.0);
    let const_vec = lua.load("return mylib.const_vec").eval::<Vector>().unwrap();
    assert_eq!(const_vec, Vector::zero());
    let const_str = lua.load("return mylib.const_str").eval::<String>();
    assert_eq!(const_str.unwrap(), "value1");
}

#[test]
fn test_chunk_wrap() -> Result<()> {
    let lua = Lua::new();

    let f = Chunk::wrap("return 123");
    lua.globals().set("f", f)?;
    lua.load("assert(f() == 123)").call::<()>(()).unwrap();

    lua.globals().set("f2", Chunk::wrap("c()"))?;
    assert!(
        (lua.load("f2()").call::<()>(()).err().unwrap().to_string()).contains(file!()),
        "wrong chunk location"
    );

    Ok(())
}
