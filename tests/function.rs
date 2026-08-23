
use mluau::{Error, ErrorWithTraceback, Function, Lua, Result, String};

#[test]
fn test_function_call() -> Result<()> {
    let lua = Lua::new();

    let concat = lua
        .load(r#"function(arg1, arg2) return arg1 .. arg2 end"#)
        .eval::<Function>()?;
    assert_eq!(concat.call::<String>(("foo", "bar"))?, "foobar");

    Ok(())
}

#[test]
fn test_function_call_error() -> Result<()> {
    let lua = Lua::new();

    let concat_err = lua
        .load(r#"function(arg1, arg2) error("concat error") end"#)
        .eval::<Function>()?;
    match concat_err.call::<String>(("foo", "bar")) {
        Err(Error::RuntimeError(msg)) if msg.contains("concat error") => {}
        other => panic!("unexpected result: {other:?}"),
    }

    Ok(())
}

#[test]
fn test_function_info() -> Result<()> {
    let lua = Lua::new();

    let globals = lua.globals();
    lua.load(
        r#"
        function function1()
            return function() end
        end
    "#,
    )
    .set_name("source1")
    .call::<()>(())?;

    let function1 = globals.get::<Function>("function1")?;
    let function2 = function1.call::<Function>(())?;
    let function3 = lua.create_function(|_, ()| Ok(()))?;

    let function1_info = function1.info();

    assert_eq!(function1_info.name.as_deref(), Some("function1"));
    assert_eq!(function1_info.source.as_deref(), Some("source1"));
    assert_eq!(function1_info.line_defined, Some(2));


    assert_eq!(function1_info.last_line_defined, None);
    assert_eq!(function1_info.what, "Lua");

    let function2_info = function2.info();
    assert_eq!(function2_info.name, None);
    assert_eq!(function2_info.source.as_deref(), Some("source1"));
    assert_eq!(function2_info.line_defined, Some(3));


    assert_eq!(function2_info.last_line_defined, None);
    assert_eq!(function2_info.what, "Lua");

    let function3_info = function3.info();
    assert_eq!(function3_info.name, None);
    assert_eq!(function3_info.source.as_deref(), Some("=[C]"));
    assert_eq!(function3_info.line_defined, None);
    assert_eq!(function3_info.last_line_defined, None);
    assert_eq!(function3_info.what, "C");

    let print_info = globals.get::<Function>("print")?.info();

    assert_eq!(print_info.name.as_deref(), Some("print"));
    assert_eq!(print_info.source.as_deref(), Some("=[C]"));
    assert_eq!(print_info.what, "C");
    assert_eq!(print_info.line_defined, None);

    Ok(())
}


#[test]
fn test_function_coverage() -> Result<()> {
    let lua = Lua::new();

    lua.set_compiler(mluau::Compiler::default().set_coverage_level(1));

    let f = lua
        .load(
            r#"local s = "abc"
        assert(#s == 3)

        function abc(i)
            if i < 5 then
                return 0
            else
                return 1
            end
        end

        (function()
            (function() abc(10) end)()
        end)()
        "#,
        )
        .into_function()?;

    f.call::<()>(())?;

    let mut report = Vec::new();
    f.coverage(|cov| {
        report.push(cov);
    });

    assert_eq!(
        report[0],
        mluau::CoverageInfo {
            function: None,
            line_defined: 1,
            depth: 0,
            hits: vec![-1, 1, 1, -1, 1, -1, -1, -1, -1, -1, -1, -1, 1, -1, -1, -1],
        }
    );
    assert_eq!(
        report[1],
        mluau::CoverageInfo {
            function: Some("abc".into()),
            line_defined: 4,
            depth: 1,
            hits: vec![-1, -1, -1, -1, -1, 1, 0, -1, 1, -1, -1, -1, -1, -1, -1, -1],
        }
    );
    assert_eq!(
        report[2],
        mluau::CoverageInfo {
            function: None,
            line_defined: 12,
            depth: 1,
            hits: vec![-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 1, -1, -1],
        }
    );
    assert_eq!(
        report[3],
        mluau::CoverageInfo {
            function: None,
            line_defined: 13,
            depth: 2,
            hits: vec![-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 1, -1, -1],
        }
    );

    Ok(())
}

#[test]
fn test_function_pointer() -> Result<()> {
    let lua = Lua::new();

    let func1 = lua.load("return function() end").into_function()?;
    let func2 = func1.call::<Function>(())?;

    assert_eq!(func1.to_pointer(), func1.clone().to_pointer());
    assert_ne!(func1.to_pointer(), func2.to_pointer());

    Ok(())
}

#[test]
fn test_function_deep_clone() -> Result<()> {
    let lua = Lua::new();

    lua.globals().set("a", 1)?;
    let func1 = lua.load("a += 1; return a").into_function()?;
    let func2 = func1.deep_clone()?;

    assert_ne!(func1.to_pointer(), func2.to_pointer());
    assert_eq!(func1.call::<i32>(())?, 2);
    assert_eq!(func2.call::<i32>(())?, 3);

    // Check that for Rust functions deep_clone is just a clone
    let rust_func = lua.create_function(|_, ()| Ok(42))?;
    let rust_func2 = rust_func.deep_clone()?;
    assert_eq!(rust_func.to_pointer(), rust_func2.to_pointer());

    Ok(())
}

#[test]
fn test_call_with_error_value() -> Result<()> {
    let lua = Lua::new();

    let thrd_main = lua.create_function(|_, ()| Ok(()))?;
    lua.globals().set("main", thrd_main)?;

    let func = lua.load("error({a=1, b=2})").into_function()?;

    let ert = func.call_with_err::<(), ErrorWithTraceback>(()).unwrap_err();

    match ert {
        ErrorWithTraceback::BaseError(e) => return Err(e),
        ErrorWithTraceback::Error { traceback, value, err_code: _ } => {
            let tab = value.as_table().unwrap();
            assert_eq!(tab.raw_get::<i64>("a")?, 1i64);
            assert_eq!(tab.raw_get::<i64>("b")?, 2i64);
            assert!(traceback.contains("in function 'error'"))
        }
    }

    Ok(())
}
