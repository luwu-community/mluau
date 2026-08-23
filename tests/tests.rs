#[cfg(not(target_arch = "wasm32"))]
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::string::String as StdString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{error, f32, f64, fmt};

use mluau::{
    ChunkSource, Error, Function, Lua, Nil, Result,
    String, Table, Value, Variadic,
};

#[test]
fn test_weak_lua() {
    let lua = Lua::new();
    let weak_lua = lua.weak();
    assert!(weak_lua.try_upgrade().is_some());
    drop(lua);
    assert!(weak_lua.try_upgrade().is_none());
}

#[test]
#[should_panic(expected = "Lua instance is destroyed")]
fn test_weak_lua_panic() {
    let lua = Lua::new();
    let weak_lua = lua.weak();
    drop(lua);
    let _ = weak_lua.upgrade();
}

#[test]
fn test_load() -> Result<()> {
    let lua = Lua::new();

    let func = lua.load("\treturn 1+2").into_function()?;
    let result: i32 = func.call(())?;
    assert_eq!(result, 3);

    assert!(lua.load("").call::<()>(()).is_ok());
    assert!(lua.load("§$%§&$%&").call::<()>(()).is_err());

    Ok(())
}

#[test]
fn test_exec() -> Result<()> {
    let lua = Lua::new();

    let globals = lua.globals();
    lua.load(
        r#"
        res = 'foo'..'bar'
    "#,
    )
    .call::<()>(())?;
    assert_eq!(globals.get::<String>("res")?, "foobar");

    let module: Table = lua
        .load(
            r#"
            local module = {}

            function module.func()
                return "hello"
            end

            return module
        "#,
        )
        .eval()?;
    assert!(module.contains_key("func")?);
    assert_eq!(module.get::<Function>("func")?.call::<String>(())?, "hello");

    Ok(())
}

#[test]
fn test_eval() -> Result<()> {
    let lua = Lua::new();

    assert_eq!(lua.load("1 + 1").eval::<i32>()?, 2);
    assert_eq!(lua.load("false == false").eval::<bool>()?, true);
    assert_eq!(lua.load("return 1 + 2").eval::<i32>()?, 3);
    match lua.load("if true then").eval::<()>() {
        Err(Error::SyntaxError {
            incomplete_input: true,
            ..
        }) => {}
        r => panic!("expected SyntaxError with incomplete_input=true, got {:?}", r),
    }

    Ok(())
}

#[test]
fn test_replace_globals() -> Result<()> {
    let lua = Lua::new();

    let globals = lua.create_table()?;
    globals.set("foo", "bar")?;

    lua.set_globals(globals.clone())?;
    let val = lua.load("return foo").eval::<StdString>()?;
    assert_eq!(val, "bar");

    Ok(())
}

#[test]
fn test_load_mode() -> Result<()> {
    let lua = unsafe { Lua::unsafe_new() };

    assert_eq!(lua.load("1 + 1").eval::<i32>()?, 2);

    let bytecode = mluau::Compiler::new().compile("return 1 + 1")?;
    // SAFETY: bytecode was just produced by `Compiler::compile` above
    let as_bytecode = || unsafe { ChunkSource::bytecode(bytecode.as_slice()) };
    assert_eq!(lua.load(as_bytecode()).eval::<i32>()?, 2);

    Ok(())
}

#[test]
fn test_lua_multi() -> Result<()> {
    let lua = Lua::new();

    lua.load(
        r#"
        function concat(arg1, arg2)
            return arg1 .. arg2
        end

        function mreturn()
            return 1, 2, 3, 4, 5, 6
        end
    "#,
    )
    .call::<()>(())?;

    let globals = lua.globals();
    let concat = globals.get::<Function>("concat")?;
    let mreturn = globals.get::<Function>("mreturn")?;

    assert_eq!(concat.call::<String>(("foo", "bar"))?, "foobar");
    let (a, b) = mreturn.call::<(u64, u64)>(())?;
    assert_eq!((a, b), (1, 2));
    let (a, b, v) = mreturn.call::<(u64, u64, Variadic<u64>)>(())?;
    assert_eq!((a, b), (1, 2));
    assert_eq!(v[..], [3, 4, 5, 6]);

    Ok(())
}

#[test]
fn test_coercion() -> Result<()> {
    let lua = Lua::new();

    lua.load(
        r#"
        int = 123
        str = "123"
        num = 123.0
        func = function() end
    "#,
    )
    .call::<()>(())?;

    let globals = lua.globals();
    assert_eq!(globals.get::<String>("int")?, "123");
    assert_eq!(globals.get::<i32>("str")?, 123);
    assert_eq!(globals.get::<i32>("num")?, 123);
    assert!(globals.get::<String>("func").is_err());

    Ok(())
}

#[test]
fn test_error() -> Result<()> {
    #[derive(Debug)]
    pub struct TestError;

    impl fmt::Display for TestError {
        fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
            write!(fmt, "test error")
        }
    }

    impl error::Error for TestError {}

    let lua = Lua::new();

    let globals = lua.globals();
    lua.load(
        r#"
        function no_error()
        end

        function lua_error()
            error("this is a lua error")
        end

        function rust_error()
            rust_error_function()
        end

        function return_error()
            local status, res = pcall(rust_error_function)
            assert(not status)
            return res
        end

        function return_string_error()
            return "this should be converted to an error"
        end

        function test_pcall()
            local testvar = 0

            pcall(function(arg)
                testvar = testvar + arg
                error("should be ignored")
            end, 3)

            local function handler(err)
                if string.match(_VERSION, " 5%.1$")
                    or string.match(_VERSION, " 5%.2$")
                    or string.match(_VERSION, "Luau")
                then
                    -- Special case for Lua 5.1/5.2 and Luau
                    local caps = string.match(err, ': (%d+)$')
                    if caps then
                        err = caps
                    end
                end
                testvar = testvar + err
                return "should be ignored"
            end

            local status, res = xpcall(function()
                error(5)
            end, handler)
            assert(not status)

            if testvar ~= 8 then
                error("testvar had the wrong value, pcall / xpcall misbehaving "..testvar)
            end
        end

        function understand_recursion()
            understand_recursion()
        end
    "#,
    )
    .call::<()>(())?;

    let rust_error_function = lua.create_function(|_, ()| -> Result<()> { Err(mluau::Error::external(TestError)) })?;
    globals.set("rust_error_function", rust_error_function)?;

    let no_error = globals.get::<Function>("no_error")?;
    assert!(no_error.call::<()>(()).is_ok());

    let lua_error = globals.get::<Function>("lua_error")?;
    assert!(lua_error.call::<()>(()).is_err());

    let rust_error = globals.get::<Function>("rust_error")?;
    assert!(rust_error.call::<()>(()).is_err());

    let _return_error = globals.get::<Function>("return_error")?;
    let test_pcall = globals.get::<Function>("test_pcall")?;
    test_pcall.call::<()>(())?;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let understand_recursion = globals.get::<Function>("understand_recursion")?;
        assert!(understand_recursion.call::<()>(()).is_err());
    }

    Ok(())
}



#[cfg(target_pointer_width = "64")]
#[test]
fn test_safe_integers() -> Result<()> {
    const MAX_SAFE_INTEGER: i64 = 2i64.pow(53) - 1;
    const MIN_SAFE_INTEGER: i64 = -2i64.pow(53) + 1;

    let lua = Lua::new();
    let f = lua.load("return ...").into_function()?;

    assert_eq!(f.call::<i64>(MAX_SAFE_INTEGER)?, MAX_SAFE_INTEGER);
    assert_eq!(f.call::<i64>(MIN_SAFE_INTEGER)?, MIN_SAFE_INTEGER);

    // For Lua versions that does not support 64-bit integers, the values will be converted to f64
    {
        assert_ne!(f.call::<i64>(MAX_SAFE_INTEGER + 2)?, MAX_SAFE_INTEGER + 2);
        assert_ne!(f.call::<i64>(MIN_SAFE_INTEGER - 2)?, MIN_SAFE_INTEGER - 2);
        assert_eq!(f.call::<f64>(i64::MAX)?, i64::MAX as f64);
    }

    Ok(())
}

#[test]
fn test_num_conversion() -> Result<()> {
    let lua = Lua::new();

    assert_eq!(
        lua.coerce_integer(Value::String(lua.create_string("1")?))?,
        Some(1)
    );
    assert_eq!(
        lua.coerce_integer(Value::String(lua.create_string("1.0")?))?,
        Some(1)
    );
    assert_eq!(
        lua.coerce_integer(Value::String(lua.create_string("1.5")?))?,
        None
    );

    assert_eq!(
        lua.coerce_number(Value::String(lua.create_string("1")?))?,
        Some(1.0)
    );
    assert_eq!(
        lua.coerce_number(Value::String(lua.create_string("1.0")?))?,
        Some(1.0)
    );
    assert_eq!(
        lua.coerce_number(Value::String(lua.create_string("1.5")?))?,
        Some(1.5)
    );

    assert_eq!(lua.load("1.0").eval::<i64>()?, 1);
    assert_eq!(lua.load("1.0").eval::<f64>()?, 1.0);
    assert_eq!(lua.load("1.0").eval::<String>()?, "1");

    assert_eq!(lua.load("1.5").eval::<i64>()?, 1);
    assert_eq!(lua.load("1.5").eval::<f64>()?, 1.5);
    assert_eq!(lua.load("1.5").eval::<String>()?, "1.5");

    assert!(lua.load("-1").eval::<u64>().is_err());
    assert_eq!(lua.load("-1").eval::<i64>()?, -1);

    assert!(lua.unpack::<u64>(lua.pack(1u128 << 64)?).is_err());
    assert!(lua.load("math.huge").eval::<i64>().is_err());

    assert_eq!(lua.unpack::<f64>(lua.pack(f32::MAX)?)?, f32::MAX as f64);
    assert_eq!(lua.unpack::<f64>(lua.pack(f32::MIN)?)?, f32::MIN as f64);
    assert_eq!(lua.unpack::<f32>(lua.pack(f64::MAX)?)?, f32::INFINITY);
    assert_eq!(lua.unpack::<f32>(lua.pack(f64::MIN)?)?, f32::NEG_INFINITY);

    assert_eq!(lua.unpack::<i128>(lua.pack(1i128 << 64)?)?, 1i128 << 64);

    // Negative zero
    let negative_zero = lua.load("-0.0").eval::<f64>()?;
    assert_eq!(negative_zero, 0.0);
    // LuaJIT treats -0.0 as a positive zero
    #[cfg(not(feature = "luajit"))]
    assert!(negative_zero.is_sign_negative());

    // In Lua <5.3 all numbers are floats
    #[cfg(not(any(feature = "lua54", feature = "lua53", feature = "luajit")))]
    {
        let negative_zero = lua.load("-0").eval::<f64>()?;
        assert_eq!(negative_zero, 0.0);
        assert!(negative_zero.is_sign_negative());
    }

    Ok(())
}

#[test]
fn test_pcall_xpcall() -> Result<()> {
    let lua = Lua::new();
    let globals = lua.globals();

    // make sure that we handle not enough arguments

    assert!(lua.load("pcall()").call::<()>(()).is_err());
    assert!(lua.load("xpcall()").call::<()>(()).is_err());
    assert!(lua.load("xpcall(function() end)").call::<()>(()).is_err());

    // Lua >= 5.2 compatible version of xpcall for 5.1
    #[cfg(feature = "lua51")]
    lua.load(
        r#"
        local xpcall_orig = xpcall
        function xpcall(f, err, ...)
            return xpcall_orig(function() return f(unpack(arg)) end, err)
        end
    "#,
    )
    .call::<()>(())?;

    // Make sure that the return values from are correct on success

    let (r, e) = lua
        .load("pcall(function(p) return p end, 'foo')")
        .eval::<(bool, String)>()?;
    assert!(r);
    assert_eq!(e, "foo");

    let (r, e) = lua
        .load("xpcall(function(p) return p end, print, 'foo')")
        .eval::<(bool, String)>()?;
    assert!(r);
    assert_eq!(e, "foo");

    // Make sure that the return values are correct on errors, and that error handling works

    lua.load(
        r#"
        pcall_error = nil
        pcall_status, pcall_error = pcall(error, "testerror")

        xpcall_error = nil
        xpcall_status, _ = xpcall(error, function(err) xpcall_error = err end, "testerror")
    "#,
    )
    .call::<()>(())?;

    assert_eq!(globals.get::<bool>("pcall_status")?, false);
    assert_eq!(globals.get::<String>("pcall_error")?, "testerror");

    assert_eq!(globals.get::<bool>("xpcall_statusr")?, false);

    // Make sure that weird xpcall error recursion at least doesn't cause unsafety or panics.
    lua.load(
        r#"
        function xpcall_recursion()
            xpcall(error, function(err) error(err) end, "testerror")
        end
    "#,
    )
    .call::<()>(())?;
    let _ = globals.get::<Function>("xpcall_recursion")?.call::<()>(());

    Ok(())
}

#[test]
fn test_set_metatable_nil() -> Result<()> {
    let lua = Lua::new();
    lua.load(
        r#"
        a = {}
        setmetatable(a, nil)
    "#,
    )
    .call::<()>(())?;
    Ok(())
}

#[test]
#[cfg(not(panic = "abort"))]
fn test_application_data() -> Result<()> {
    let lua = Lua::new();

    lua.set_app_data("test1");
    lua.set_app_data(vec!["test2"]);

    // Borrow &str immutably and Vec<&str> mutably
    let s = lua.app_data_ref::<&str>().unwrap();
    let mut v = lua.app_data_mut::<Vec<&str>>().unwrap();
    v.push("test3");

    // Insert of new data or removal should fail now
    assert!(lua.try_set_app_data::<i32>(123).is_err());
    match catch_unwind(AssertUnwindSafe(|| lua.set_app_data::<i32>(123))) {
        Ok(_) => panic!("expected panic"),
        Err(_) => {}
    }
    match catch_unwind(AssertUnwindSafe(|| lua.remove_app_data::<i32>())) {
        Ok(_) => panic!("expected panic"),
        Err(_) => {}
    }

    // Check display and debug impls
    assert_eq!(format!("{s}"), "test1");
    assert_eq!(format!("{s:?}"), "\"test1\"");

    // Borrowing immutably and mutably of the same type is not allowed
    assert!(lua.try_app_data_mut::<&str>().is_err());
    match catch_unwind(AssertUnwindSafe(|| lua.app_data_mut::<&str>().unwrap())) {
        Ok(_) => panic!("expected panic"),
        Err(_) => {}
    }
    assert!(lua.try_app_data_ref::<Vec<&str>>().is_err());
    drop((s, v));

    // Test that application data is accessible from anywhere
    let f = lua.create_function(|lua, ()| {
        let mut data1 = lua.app_data_mut::<&str>().unwrap();
        assert_eq!(*data1, "test1");
        *data1 = "test4";

        let data2 = lua.app_data_ref::<Vec<&str>>().unwrap();
        assert_eq!(*data2, vec!["test2", "test3"]);

        Ok(())
    })?;
    f.call::<()>(())?;

    assert_eq!(*lua.app_data_ref::<&str>().unwrap(), "test4");
    assert_eq!(*lua.app_data_ref::<Vec<&str>>().unwrap(), vec!["test2", "test3"]);

    lua.remove_app_data::<Vec<&str>>();
    assert!(matches!(lua.app_data_ref::<Vec<&str>>(), None));

    Ok(())
}

#[test]
fn test_rust_function() -> Result<()> {
    let lua = Lua::new();

    let globals = lua.globals();
    lua.load(
        r#"
        function lua_function()
            return rust_function()
        end

        -- Test to make sure chunk return is ignored
        return 1
    "#,
    )
    .call::<()>(())?;

    let lua_function = globals.get::<Function>("lua_function")?;
    let rust_function = lua.create_function(|_, ()| Ok("hello"))?;

    globals.set("rust_function", rust_function)?;
    assert_eq!(lua_function.call::<String>(())?, "hello");

    Ok(())
}

#[test]
fn test_c_function() -> Result<()> {
    let lua = Lua::new();

    extern "C-unwind" fn c_function(state: *mut mluau::lua_State) -> std::os::raw::c_int {
        unsafe {
            ffi::lua_pushboolean(state, 1);
            ffi::lua_setglobal(state, b"c_function\0" as *const _ as *const _);
        }
        0
    }

    let func = unsafe { lua.create_c_function(c_function)? };
    func.call::<()>(())?;
    assert_eq!(lua.globals().get::<bool>("c_function")?, true);

    Ok(())
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_recursion() -> Result<()> {
    let lua = Lua::new();

    let f = lua.create_function(move |lua, i: i32| {
        if i < 64 {
            lua.globals().get::<Function>("f")?.call::<()>(i + 1)?;
        }
        Ok(())
    })?;

    lua.globals().set("f", &f)?;
    f.call::<()>(1)?;

    Ok(())
}



#[test]
#[cfg(not(feature = "luajit"))]
#[cfg(not(target_arch = "wasm32"))]
fn test_too_many_recursions() -> Result<()> {
    let lua = Lua::new();

    let f = lua.create_function(move |lua, ()| lua.globals().get::<Function>("f")?.call::<()>(()))?;

    lua.globals().set("f", &f)?;
    assert!(f.call::<()>(()).is_err());

    Ok(())
}
 
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn test_ref_stack_exhaustion() {
    match catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        let lua = Lua::new();
        let mut vals = Vec::new();
        for _ in 0..200000 {
            //println!("Creating table {}", vals.len());
            vals.push(lua.create_table()?);
        }
        Ok(())
    })) {
        Ok(_) => {}
        Err(p) => panic!("got panic: {:?}", p),
    }
}

#[test]
fn test_large_args() -> Result<()> {
    let lua = Lua::new();
    let globals = lua.globals();

    globals.set(
        "c",
        lua.create_function(|_, args: Variadic<usize>| {
            let mut s = 0;
            for i in 0..args.len() {
                s += i;
                assert_eq!(i, args[i]);
            }
            Ok(s)
        })?,
    )?;

    let f: Function = lua
        .load(
            r#"
            return function(...)
                return c(...)
            end
        "#,
        )
        .eval()?;

    assert_eq!(f.call::<usize>((0..100).collect::<Variadic<usize>>())?, 4950);

    Ok(())
}

#[test]
fn test_large_args_ref() -> Result<()> {
    let lua = Lua::new();

    let f = lua.create_function(|_, args: Variadic<String>| {
        for i in 0..args.len() {
            assert_eq!(args[i], i.to_string());
        }
        Ok(())
    })?;

    f.call::<()>((0..100).map(|i| i.to_string()).collect::<Variadic<_>>())?;

    Ok(())
}

#[test]
fn test_chunk_env() -> Result<()> {
    let lua = Lua::new();

    let assert: Function = lua.globals().get("assert")?;

    let env1 = lua.create_table()?;
    env1.set("assert", assert.clone())?;

    let env2 = lua.create_table()?;
    env2.set("assert", assert)?;

    lua.load(
        r#"
        test_var = 1
    "#,
    )
    .set_environment(env1.clone())
    .call::<()>(())?;

    lua.load(
        r#"
        assert(test_var == nil)
        test_var = 2
    "#,
    )
    .set_environment(env2.clone())
    .call::<()>(())?;

    assert_eq!(lua.load("test_var").set_environment(env1).eval::<i32>()?, 1);
    assert_eq!(lua.load("test_var").set_environment(env2).eval::<i32>()?, 2);

    Ok(())
}

#[test]
fn test_context_thread() -> Result<()> {
    let lua = Lua::new();

    let f = lua
        .load(
            r#"
            local thread = ...
            assert(coroutine.running() == thread)
        "#,
        )
        .into_function()?;

    f.call::<()>(Nil)?;

    Ok(())
}

#[test]
fn test_register_module() -> Result<()> {
    let lua = Lua::new();

    let t = lua.create_table()?;
    t.set("name", "my_module")?;
    lua.register_module("@my_module", &t)?;

    lua.load(
        r#"
        local my_module = require("@my_module")
        assert(my_module.name == "my_module")
    "#,
    )
    .call::<()>(())?;

    lua.unload_module("@my_module")?;
    lua.load(
        r#"
        local ok, err = pcall(function() return require("@my_module") end)
        assert(not ok)
        "#,
    )
    .call::<()>(())?;

    {
        // Luau registered modules must have '@' prefix
        let res = lua.register_module("my_module", 123);
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "runtime error: module name must begin with '@'"
        );

        // Luau registered modules (aliases) are case-insensitive
        let res = lua.register_module("@My_Module", &t);
        assert!(res.is_ok());
        lua.load(
            r#"
            local my_module = require("@MY_MODule")
            assert(my_module.name == "my_module")
        "#,
        )
        .call::<()>(())?;
    }

    Ok(())
}


#[test]
fn test_inspect_stack() -> Result<()> {
    let lua = Lua::new();

    // Not inside any function
    assert!(lua.inspect_stack(0, |_| ()).is_none());

    let logline = lua.create_function(|lua, msg: StdString| {
        let r = lua
            .inspect_stack(1, |debug| {
                let source = debug.source().short_src;
                let source = source.as_deref().unwrap_or("?");
                let line = debug.current_line().unwrap();
                format!("{}:{} {}", source, line, msg)
            })
            .unwrap();
        Ok(r)
    })?;
    lua.globals().set("logline", logline)?;

    lua.load(
        r#"
        local function foo()
            local line = logline("hello")
            return line
        end
        local function bar()
            return foo()
        end

        assert(foo() == '[string "chunk"]:3 hello')
        assert(bar() == '[string "chunk"]:3 hello')
        assert(logline("world") == '[string "chunk"]:12 world')
    "#,
    )
    .set_name("chunk")
    .call::<()>(())?;

    let stack_info = lua.create_function(|lua, ()| {
        let stack_info = lua.inspect_stack(1, |debug| debug.stack()).unwrap();
        Ok(format!("{stack_info:?}"))
    })?;
    lua.globals().set("stack_info", stack_info)?;

    lua.load(
        r#"
        local stack_info = stack_info
        local function baz(a, b, c, ...)
            return stack_info()
        end
        assert(baz() == 'DebugStack { num_ups: 1, num_params: 3, is_vararg: true }')
    "#,
    )
    .call::<()>(())?;

    // LuaJIT does not pass this test for some reason
    #[cfg(feature = "lua51")]
    lua.load(
        r#"
        local stack_info = stack_info
        local function baz(a, b, c, ...)
            return stack_info()
        end
        assert(baz() == 'DebugStack { num_ups: 1 }')
    "#,
    )
    .call::<()>(())?;

    // Test retrieving currently running function
    let running_function =
        lua.create_function(|lua, ()| Ok(lua.inspect_stack(1, |debug| debug.function())))?;
    lua.globals().set("running_function", running_function)?;
    lua.load(
        r#"
        local function baz()
            return running_function()
        end
        if jit == nil then
            assert(baz() == baz)
        else
            -- luajit inline the "baz" function and returns the chunk itself
            assert(baz() == running_function())
        end
    "#,
    )
    .call::<()>(())?;

    Ok(())
}

#[test]
fn test_traceback() -> Result<()> {
    let lua = Lua::new();

    // Test traceback at level 0 (not inside any function)
    let _traceback = lua.traceback(None, 0)?.to_string_lossy();

    // Test traceback with a message prefix
    let traceback = lua.traceback(Some("error occurred"), 0)?.to_string_lossy();
    assert!(traceback.starts_with("error occurred"));

    // Test traceback inside a function
    let get_traceback = lua.create_function(|lua, (msg, level): (Option<StdString>, usize)| {
        lua.traceback(msg.as_deref(), level)
    })?;
    lua.globals().set("get_traceback", get_traceback)?;

    lua.load(
        r#"
        local function foo()
            -- Level 1 is inside foo (the caller)
            local traceback = get_traceback(nil, 1)
            return traceback
        end
        local function bar()
            local result = foo()
            return result
        end
        local function baz()
            local result = bar()
            return result
        end
    "#,
    )
    .call::<()>(())?;

    // Test traceback at different levels
    lua.load(
        r#"
        local function foo()
            local tb0 = get_traceback(nil, 0)
            local tb1 = get_traceback(nil, 1)
            local tb2 = get_traceback(nil, 2)
            return tb0, tb1, tb2
        end
        local function bar()
            local tb0, tb1, tb2 = foo()
            return tb0, tb1, tb2
        end

        local tb0, tb1, tb2 = bar()
    "#,
    )
    .call::<()>(())?;

    Ok(())
}

#[test]
fn test_multi_states() -> Result<()> {
    let lua = Lua::new();

    let f = lua.create_function(|_, g: Option<Function>| {
        if let Some(g) = g {
            g.call::<()>(())?;
        }
        Ok(())
    })?;
    lua.globals().set("f", f)?;

    lua.load("f(function() coroutine.wrap(function() f() end)() end)")
        .call::<()>(())?;

    Ok(())
}

#[test]
fn test_exec_raw() -> Result<()> {
    let lua = Lua::new();

    let sum = lua.create_function(|_, args: Variadic<i32>| {
        let mut sum = 0;
        for i in args {
            sum += i;
        }
        Ok(sum)
    })?;
    lua.globals().set("sum", sum)?;

    let n: i32 = unsafe {
        lua.exec_raw((), |state| {
            ffi::lua_getglobal(state, b"sum\0".as_ptr() as _);
            ffi::lua_pushinteger(state, 1);
            ffi::lua_pushinteger(state, 7);
            ffi::lua_call(state, 2, 1);
        })
    }?;
    assert_eq!(n, 8);

    // Test error handling
    let res: Result<()> = unsafe {
        lua.exec_raw("test error", |state| {
            ffi::lua_error(state);
        })
    };
    assert!(matches!(res, Err(Error::RuntimeError(err)) if err.contains("test error")));

    Ok(())
}

#[test]
fn test_gc_drop_ref_thread() -> Result<()> {
    let lua = Lua::new();

    let t = lua.create_table()?;
    lua.create_function(move |_, ()| {
        _ = &t;
        Ok(())
    })?;

    for _ in 0..10000 {
        // GC will run eventually to collect the function and the table above
        lua.create_table()?;
    }

    Ok(())
}


#[test]
fn test_onclose() -> Result<()> {
    let lua = Lua::new();

    let debug_ptr = lua.main_state_address();
    let closed = Arc::new(AtomicBool::new(false));
    let closed_ref = closed.clone();
    lua.set_on_close(move || {
        closed_ref.store(true, Ordering::SeqCst);
        println!("Dropping lua state {}", debug_ptr)
    });

    // Close Lua state
    drop(lua);

    // Check that on_close callback was called
    assert!(closed.load(Ordering::SeqCst));

    Ok(())
}
