use mluau::{CustomError, Error, IntoLua, Lua, Result};

#[test]
fn test_disable_error_userdata() -> Result<()> {
    let lua = Lua::new_with(
        mluau::StdLib::ALL_SAFE,
    )?;

    let func =
        lua.create_function(|_, ()| Err::<(), _>(Error::runtime("runtime error")))?;
    lua.globals().set("func", func)?;

    let msg = lua
        .load("local _, err = pcall(func); return tostring(err)")
        .eval::<String>()?;
    assert!(msg.contains("runtime error"));

    let func2 = lua.create_function(|lua, ()| {
        lua.globals()
            .get::<String>("nonextant")
    })?;
    lua.globals().set("func2", func2)?;

    let msg2 = lua
        .load("local _, err = pcall(func2); return tostring(err)")
        .eval::<String>()?;
    assert!(msg2.contains("error converting Lua nil to String"));

    lua.set_memory_limit(1000)?;

    // Force a memory error
    for i in 0..10000 {
        match lua.load(format!("return string.rep('a', {})", i)).call::<()>(()) {
            Ok(_) => {}
            Err(mluau::Error::MemoryError { .. }) => {
                // Memory error is expected, we can stop here
                break;
            }
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    lua.set_memory_limit(10000000)
        .expect("Failed to set memory limit");

    // Next, test panic handling
    let func4 = lua.create_function(|_, ()| {
        if true {
            panic!("This is a test panic")
        } else {
            Ok(())
        }
    })?;
    lua.globals().set("func4", func4)?;
    let msg4 = lua
        .load("local ok, err = pcall(func4); return tostring(err)")
        .eval::<String>()?;
    assert!(msg4.contains("This is a test panic"));

    let res = lua.globals().get::<mluau::Function>("func4")?.call::<()>(());

    assert!(res.is_err());
    assert!(res.unwrap_err().to_string().contains("This is a test panic"));

    Ok(())
}

#[test]
fn test_custom_error_value() -> Result<()> {
    let lua = Lua::new();

    struct MyTableError {
        a: String,
        b: i32,
    }

    impl IntoLua for MyTableError {
        fn into_lua(self, lua: &Lua) -> Result<mluau::Value> {
            let table = lua.create_table()?;
            table.set("a", self.a)?;
            table.set("b", self.b)?;
            Ok(mluau::Value::Table(table))
        }
    }

    let func = lua.create_function(|_, ()| {
        CustomError(MyTableError { a: "hello".to_string(), b: 42 })
    })?;
    lua.globals().set("func", func)?;

    let res = lua.load(r#"
        local ok, err = pcall(func)
        assert(not ok, "function should have failed")
        assert(type(err) == "table", "error should be a table")
        assert(err.a == "hello", "err.a mismatch")
        assert(err.b == 42, "err.b mismatch")
        return true
    "#).eval::<bool>()?;

    assert!(res);

    Ok(())
}
