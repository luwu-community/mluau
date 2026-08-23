use std::cell::Cell;
use std::fmt::Debug;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::Arc;

use mluau::{
    Compiler, Error, Function, Lua, Result, StdLib, Table, ThreadStatus, Value, Vector, VmState,
};

#[test]
fn test_version() -> Result<()> {
    let lua = Lua::new();
    assert!(lua.globals().get::<String>("_VERSION")?.starts_with("Luau 0.730"));
    Ok(())
}

#[cfg(not(feature = "luau-vector4"))]
#[test]
fn test_vectors() -> Result<()> {
    let lua = Lua::new();

    let v: Vector = lua
        .load("vector.create(1, 2, 3) + vector.create(3, 2, 1)")
        .eval()?;
    assert_eq!(v, [4.0, 4.0, 4.0]);

    // Test conversion into Rust array
    let v: [f64; 3] = lua.load("vector.create(1, 2, 3)").eval()?;
    assert!(v == [1.0, 2.0, 3.0]);

    // Test vector methods
    lua.load(
        r#"
        local v = vector.create(1, 2, 3)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
    "#,
    )
    .call::<()>(())?;

    // Test vector methods (fastcall)
    lua.load(
        r#"
        local v = vector.create(1, 2, 3)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
    "#,
    )
    .set_compiler(Compiler::new().set_vector_ctor("vector"))
    .call::<()>(())?;

    Ok(())
}

#[cfg(feature = "luau-vector4")]
#[test]
fn test_vectors() -> Result<()> {
    let lua = Lua::new();

    let v: Vector = lua
        .load("vector.create(1, 2, 3, 4) + vector.create(4, 3, 2, 1)")
        .eval()?;
    assert_eq!(v, [5.0, 5.0, 5.0, 5.0]);

    // Test conversion into Rust array
    let v: [f64; 4] = lua.load("vector.create(1, 2, 3, 4)").eval()?;
    assert!(v == [1.0, 2.0, 3.0, 4.0]);

    // Test vector methods
    lua.load(
        r#"
        local v = vector.create(1, 2, 3, 4)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
        assert(v.w == 4)
    "#,
    )
    .call::<()>(())?;

    // Test vector methods (fastcall)
    lua.load(
        r#"
        local v = vector.create(1, 2, 3, 4)
        assert(v.x == 1)
        assert(v.y == 2)
        assert(v.z == 3)
        assert(v.w == 4)
    "#,
    )
    .set_compiler(Compiler::new().set_vector_ctor("vector"))
    .call::<()>(())?;

    Ok(())
}

#[test]
fn test_int64() -> Result<()> {
    let lua = Lua::new();

    let v: i64 = lua.load("return 123i").eval()?;
    assert_eq!(v, 123);

    let v: i64 = lua.load("return integer.add(..., 1i)").call(Value::Int64(10))?;
    assert_eq!(v, 11);

    Ok(())
}

#[cfg(not(feature = "luau-vector4"))]
#[test]
fn test_vector_metatable() -> Result<()> {
    let lua = Lua::new();

    let vector_mt = lua
        .load(
            r#"
            {
                __index = {
                    new = vector.create,

                    product = function(a, b)
                        return vector.create(a.x * b.x, a.y * b.y, a.z * b.z)
                    end
                }
            }
    "#,
        )
        .eval::<Table>()?;
    vector_mt.set_metatable(Some(vector_mt.clone()))?;
    lua.set_type_metatable::<Vector>(Some(vector_mt.clone()));
    lua.globals().set("Vector3", vector_mt)?;

    let compiler = Compiler::new()
        .set_vector_ctor("Vector3.new")
        .set_vector_type("Vector3");

    // Test vector methods (fastcall)
    lua.load(
        r#"
        local v = Vector3.new(1, 2, 3)
        local v2 = v:product(Vector3.new(2, 3, 4))
        assert(v2.x == 2 and v2.y == 6 and v2.z == 12)
    "#,
    )
    .set_compiler(compiler)
    .call::<()>(())?;

    Ok(())
}

#[test]
fn test_readonly_table() -> Result<()> {
    let lua = Lua::new();

    let t = lua.create_sequence_from([1])?;
    assert!(!t.is_readonly());
    t.set_readonly(true);
    assert!(t.is_readonly());

    #[track_caller]
    fn check_readonly_error<T: Debug>(res: Result<T>) {
        match res {
            Err(Error::RuntimeError(e)) if e.contains("attempt to modify a readonly table") => {}
            r => panic!("expected RuntimeError(...) with a specific message, got {r:?}"),
        }
    }

    check_readonly_error(t.set("key", "value"));
    check_readonly_error(t.raw_set("key", "value"));
    check_readonly_error(t.raw_insert(1, "value"));
    check_readonly_error(t.raw_remove(1));
    check_readonly_error(t.push("value"));
    check_readonly_error(t.pop::<Value>());
    check_readonly_error(t.raw_push("value"));
    check_readonly_error(t.raw_pop::<Value>());

    // Special case
    match t.set_metatable(None) {
        Err(Error::RuntimeError(e)) if e.contains("attempt to modify a readonly table") => {}
        r => panic!("expected RuntimeError(...) with a specific message, got {r:?}"),
    }

    Ok(())
}

#[test]
fn test_sandbox() -> Result<()> {
    let lua = Lua::new();

    lua.sandbox(true)?;

    lua.load("global = 123").call::<()>(())?;
    let n: i32 = lua.load("return global").eval()?;
    assert_eq!(n, 123);
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(123));

    // Threads should inherit "main" globals
    let f = lua.create_function(|lua, ()| lua.globals().get::<i32>("global"))?;
    let co = lua.create_thread(f.clone())?;
    assert_eq!(co.resume::<Option<i32>, crate::Error>(())?, Some(123));

    // Sandboxed threads should also inherit "main" globals
    let co = lua.create_thread(f)?;
    co.sandbox()?;
    assert_eq!(co.resume::<Option<i32>, crate::Error>(())?, Some(123));

    // collectgarbage should be restricted in sandboxed mode
    let collectgarbage = lua.globals().get::<Function>("collectgarbage")?;
    for arg in ["collect", "stop", "restart", "step", "isrunning"] {
        let err = collectgarbage.call::<()>(arg).err().unwrap().to_string();
        assert!(err.contains("collectgarbage called with invalid option"));
    }
    assert!(collectgarbage.call::<u64>("count").unwrap() > 0);

    lua.sandbox(false)?;

    // Previously set variable `global` should be cleared now
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, None);

    // Readonly flags should be cleared as well
    let table = lua.globals().get::<Table>("table")?;
    table.set("test", "test")?;

    // collectgarbage should work now
    for arg in ["collect", "stop", "restart", "count", "step", "isrunning"] {
        collectgarbage.call::<()>(arg).unwrap();
    }

    Ok(())
}

#[test]
fn test_sandbox_safeenv() -> Result<()> {
    let lua = Lua::new();

    lua.sandbox(true)?;
    lua.globals().set("state", lua.create_table()?)?;
    lua.globals().set_safeenv(false);
    lua.load("state.a = 123").call::<()>(())?;
    let a: i32 = lua.load("state.a = 321; return state.a").eval()?;
    assert_eq!(a, 321);

    Ok(())
}

#[test]
fn test_sandbox_nolibs() -> Result<()> {
    let lua = Lua::new_with(StdLib::NONE).unwrap();

    lua.sandbox(true)?;
    lua.load("global = 123").call::<()>(())?;
    let n: i32 = lua.load("return global").eval()?;
    assert_eq!(n, 123);
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(123));

    lua.sandbox(false)?;
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, None);

    Ok(())
}

#[test]
fn test_sandbox_threads() -> Result<()> {
    let lua = Lua::new();

    let f = lua.create_function(|lua, v: Value| lua.globals().set("global", v))?;

    let co = lua.create_thread(f.clone())?;
    co.resume::<(), crate::Error>(321)?;
    // The main state should see the `global` variable (as the thread is not sandboxed)
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(321));

    let co = lua.create_thread(f.clone())?;
    co.sandbox()?;
    co.resume::<(), crate::Error>(123)?;
    // The main state should see the previous `global` value (as the thread is sandboxed)
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(321));

    // Try to reset the (sandboxed) thread
    co.reset(f)?;
    co.resume::<(), crate::Error>(111)?;
    assert_eq!(lua.globals().get::<Option<i32>>("global")?, Some(111));

    Ok(())
}

#[test]
fn test_interrupts() -> Result<()> {
    let lua = Lua::new();

    let interrupts_count = Arc::new(AtomicU64::new(0));
    let interrupts_count2 = interrupts_count.clone();

    lua.set_interrupt(move |_| {
        interrupts_count2.fetch_add(1, Ordering::Relaxed);
        Ok(VmState::Continue)
    });
    let f = lua
        .load(
            r#"
        local x = 2 + 3
        local y = x * 63
        local z = string.len(x..", "..y)
    "#,
        )
        .into_function()?;
    f.call::<()>(())?;

    assert!(interrupts_count.load(Ordering::Relaxed) > 0);

    //
    // Test yields from interrupt
    //
    let yield_count = Arc::new(AtomicU64::new(0));
    let yield_count2 = yield_count.clone();
    lua.set_interrupt(move |_| {
        if yield_count2.fetch_add(1, Ordering::Relaxed) == 1 {
            return Ok(VmState::Yield);
        }
        Ok(VmState::Continue)
    });
    let co = lua.create_thread(
        lua.load(
            r#"
            local a = {1, 2, 3}
            local b = 0
            for _, x in ipairs(a) do b += x end
            return b
        "#,
        )
        .into_function()?,
    )?;
    co.resume::<(), crate::Error>(())?;
    assert_eq!(co.status(), ThreadStatus::Resumable);
    let result: i32 = co.resume::<_, crate::Error>(())?;
    assert_eq!(result, 6);
    assert_eq!(yield_count.load(Ordering::Relaxed), 7);
    assert_eq!(co.status(), ThreadStatus::Finished);

    // Test no yielding at non-yieldable points
    yield_count.store(0, Ordering::Relaxed);
    let co = lua.create_thread(lua.create_function(|lua, arg: Value| {
        (lua.load("return (function(x) return x end)(...)")).call::<Value>(arg)
    })?)?;
    let res = co.resume::<String, crate::Error>("abc")?;
    assert_eq!(res, "abc".to_string());
    assert_eq!(yield_count.load(Ordering::Relaxed), 3);

    //
    // Test errors in interrupts
    //
    lua.set_interrupt(|_| Err(Error::runtime("error from interrupt")));
    match f.call::<()>(()) {
        Err(Error::RuntimeError(ref msg)) => assert!(msg.contains("error from interrupt")),
        res => panic!("expected `RuntimeError` with a specific message, got {res:?}"),
    }

    lua.remove_interrupt();

    Ok(())
}

#[test]
fn test_fflags() {
    // We cannot really on any particular feature flag to be present
    assert!(Lua::set_fflag("UnknownFlag", true).is_err());
}

// Regression test: enabling Luau's (experimental) user-defined-classes fastflags used
// to panic during `Lua::new()`. The compiler bumps its bytecode version once
// `DebugLuauUserDefinedClasses` is on, and mluau's own bootstrap chunk (loaded during
// `configure_luau`) got misclassified as text by a byte-sniffing heuristic that assumed
// bytecode version numbers would never collide with common leading whitespace bytes.
#[cfg(feature = "luau-classes")]
#[test]
fn test_classes_fflag_does_not_panic() -> Result<()> {
    // This used to panic with:
    // "Error configuring Luau (this is a bug, please file an issue):
    //  SyntaxError { message: \"attempt to load a text chunk (mode is 'b')\" ... }"
    let lua = Lua::new();

    let result: i64 = lua.load("return 1 + 1").eval()?;
    assert_eq!(result, 2);

    Ok(())
}

// With the `luau-classes` feature enabled and the runtime fastflag on, mluau should
// register Luau's `class` global table (mirroring what Luau's own `luaL_openlibs` does
// internally), so `class.isinstance`/`class.classof` are available to scripts.
#[cfg(feature = "luau-classes")]
#[test]
fn test_classes_lib_registered_when_fflag_enabled() -> Result<()> {
    let lua = Lua::new();

    let has_class: bool = lua.load("return class ~= nil").eval()?;
    assert!(has_class, "expected `class` global to be registered");

    let has_functions: bool = lua
        .load("return type(class.isinstance) == 'function' and type(class.classof) == 'function'")
        .eval()?;
    assert!(has_functions, "expected class.isinstance/classof to be functions");

    Ok(())
}

// Classes are constructed by *calling* the class value: Luau gives each class a metatable
// with `__call` set to a constructor that builds a new object (see `luaR_createobject` in
// Luau's VM). This test creates a class and an instance entirely in Luau, round-trips both
// through Rust functions (typed as `mluau::Class`/`mluau::Object`), and checks that identity,
// `class.isinstance`/`classof`, and field access all still work afterwards -- i.e. that our
// `LUA_TCLASS`/`LUA_TOBJECT` push/pop plumbing doesn't corrupt or misidentify the values.
#[cfg(feature = "luau-classes")]
#[test]
fn test_classes_instantiate_and_roundtrip_through_rust() -> Result<()> {
    let lua = Lua::new();

    let receive_class = lua.create_function(|_, class: mluau::Class| Ok(class))?;
    lua.globals().set("receive_class", receive_class)?;

    let receive_object = lua.create_function(|_, object: mluau::Object| Ok(object))?;
    lua.globals().set("receive_object", receive_object)?;

    let result: bool = lua
        .load(
            r#"
            class Point
                public x: number
                public y: number
            end

            local point = Point({ x = 1, y = 2 })

            -- Sanity: the runtime actually produced real class/object values, not e.g. tables.
            assert(type(Point) == "class", "expected Point to be a class, got " .. type(Point))
            assert(type(point) == "object", "expected point to be an object, got " .. type(point))

            local roundtripped_class = receive_class(Point)
            local roundtripped_object = receive_object(point)

            assert(roundtripped_class == Point, "class identity was not preserved across the roundtrip")
            assert(roundtripped_object == point, "object identity was not preserved across the roundtrip")

            assert(class.isinstance(roundtripped_object, roundtripped_class), "isinstance failed after roundtrip")
            assert(class.classof(roundtripped_object) == roundtripped_class, "classof mismatch after roundtrip")

            return roundtripped_object.x == 1 and roundtripped_object.y == 2
            "#,
        )
        .eval()?;

    assert!(result, "object fields did not survive the roundtrip");

    Ok(())
}

// Same roundtrip as above, but going through the untyped `Value` enum instead of the typed
// `Class`/`Object` wrappers, exercising `Value::is_class`/`as_class`/`is_object`/`as_object`
// and `Value::type_name` along the way.
#[cfg(feature = "luau-classes")]
#[test]
fn test_classes_value_enum_roundtrip() -> Result<()> {
    let lua = Lua::new();

    let seen_class: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));
    let seen_object: Arc<std::sync::Mutex<Option<Value>>> = Arc::new(std::sync::Mutex::new(None));

    let seen_class_clone = seen_class.clone();
    let receive_class = lua.create_function(move |_, value: Value| {
        assert!(
            value.is_class(),
            "expected a class value, got {}",
            value.type_name()
        );
        assert!(value.as_class().is_some());
        *seen_class_clone.lock().unwrap() = Some(value.clone());
        Ok(value)
    })?;
    lua.globals().set("receive_class", receive_class)?;

    let seen_object_clone = seen_object.clone();
    let receive_object = lua.create_function(move |_, value: Value| {
        assert!(
            value.is_object(),
            "expected an object value, got {}",
            value.type_name()
        );
        assert!(value.as_object().is_some());
        *seen_object_clone.lock().unwrap() = Some(value.clone());
        Ok(value)
    })?;
    lua.globals().set("receive_object", receive_object)?;

    lua.load(
        r#"
        class Animal
            public name: string
        end

        local cat = Animal({ name = "cat" })
        assert(receive_class(Animal) == Animal)
        assert(receive_object(cat) == cat)
        "#,
    )
    .call::<()>(())?;

    let class_value = seen_class
        .lock()
        .unwrap()
        .take()
        .expect("class value was not captured");
    let object_value = seen_object
        .lock()
        .unwrap()
        .take()
        .expect("object value was not captured");

    assert_eq!(class_value.type_name(), "class");
    assert_eq!(object_value.type_name(), "object");
    assert!(!class_value.is_object());
    assert!(!object_value.is_class());

    Ok(())
}

// Mutate an object's field from a Rust function (via `Object::set`) and check that the change
// is visible back in Luau, then read it back through `Object::get`. Also checks that indexing
// a member that doesn't exist on the class raises a Lua error rather than returning nil (unlike
// tables), and that mluau surfaces that as `Err` instead of panicking.
#[cfg(feature = "luau-classes")]
#[test]
fn test_classes_get_set_object_field_from_rust() -> Result<()> {
    let lua = Lua::new();

    let bump_score = lua.create_function(|_, object: mluau::Object| {
        let current: i64 = object.get("score")?;
        object.set("score", current + 10)?;

        // `nonexistent` isn't a declared member of the class, so Luau throws a hard error on
        // read instead of returning nil -- make sure that comes back as `Err`, not a panic.
        let missing_result: Result<Value> = object.get("nonexistent");
        assert!(
            missing_result.is_err(),
            "expected reading a missing member to error"
        );

        Ok(())
    })?;
    lua.globals().set("bump_score", bump_score)?;

    let result: i64 = lua
        .load(
            r#"
            class Player
                public score: number
            end

            local player = Player({ score = 5 })
            bump_score(player)
            return player.score
            "#,
        )
        .eval()?;

    assert_eq!(result, 15);

    Ok(())
}

#[test]
fn test_thread_events() -> Result<()> {
    let lua = Lua::new();

    let count = Arc::new(AtomicU64::new(0));
    let thread_data: Arc<(AtomicPtr<c_void>, AtomicBool)> = Arc::new(Default::default());

    let (count2, thread_data2) = (count.clone(), thread_data.clone());
    lua.set_thread_creation_callback(move |_, thread| {
        count2.fetch_add(1, Ordering::Relaxed);
        (thread_data2.0).store(thread.to_pointer() as *mut _, Ordering::Relaxed);
        thread_data2.1.store(false, Ordering::Relaxed);
        Ok(())
    });
    let (count3, thread_data3) = (count.clone(), thread_data.clone());
    lua.set_thread_collection_callback(move |thread_ptr| {
        count3.fetch_add(1, Ordering::Relaxed);
        if thread_data3.0.load(Ordering::Relaxed) == thread_ptr.0 {
            thread_data3.1.store(true, Ordering::Relaxed);
        }
    });

    let t = lua.create_thread(lua.load("return 123").into_function()?)?;
    assert_eq!(count.load(Ordering::Relaxed), 1);
    let t_ptr = t.to_pointer();
    assert_eq!(t_ptr, thread_data.0.load(Ordering::Relaxed));
    assert!(!thread_data.1.load(Ordering::Relaxed));

    // Thead will be destroyed after GC cycle
    drop(t);
    lua.gc_collect()?;
    assert_eq!(count.load(Ordering::Relaxed), 2);
    assert_eq!(t_ptr, thread_data.0.load(Ordering::Relaxed));
    assert!(thread_data.1.load(Ordering::Relaxed));

    // Check that recursion is not allowed
    let count4 = count.clone();
    lua.set_thread_creation_callback(move |lua, _value| {
        count4.fetch_add(1, Ordering::Relaxed);
        let _ = lua.create_thread(lua.load("return 123").into_function().unwrap())?;
        Ok(())
    });
    let t = lua.create_thread(lua.load("return 123").into_function()?)?;
    assert_eq!(count.load(Ordering::Relaxed), 3);

    lua.remove_thread_callbacks();
    drop(t);
    lua.gc_collect()?;
    assert_eq!(count.load(Ordering::Relaxed), 3);

    // Test error inside callback
    lua.set_thread_creation_callback(move |_, _| Err(Error::runtime("error when processing thread event")));
    let result = lua.create_thread(lua.load("return 123").into_function()?);
    assert!(result.is_err());
    println!("{:?}", result);
    assert!(
        matches!(result, Err(Error::RuntimeError(err)) if err.contains("error when processing thread event"))
    );

    // Test context switch when running Lua script
    let count = Cell::new(0);
    lua.set_thread_creation_callback(move |_, _| {
        count.set(count.get() + 1);
        if count.get() == 2 {
            return Err(Error::runtime("thread limit exceeded"));
        }
        Ok(())
    });
    let result = lua
        .load(
            r#"
            local co = coroutine.wrap(function() return coroutine.create(print) end)
            co()
    "#,
        )
        .call::<()>(());
    assert!(result.is_err());
    assert!(matches!(result, Err(Error::RuntimeError(err)) if err.contains("thread limit exceeded")));
    lua.gc_collect()?; // Drop the coroutine

    // Lastly test that topointer of thread and thread_ptr in callbacks are the same
    let new_thread = lua.create_thread(lua.load("return 123").into_function()?)?;
    let new_thread_ptr = new_thread.to_pointer();
    thread_data.0.store(new_thread_ptr as *mut _, Ordering::Relaxed);

    let status_ptr = Arc::new(AtomicBool::new(false));
    let status_ptr2 = status_ptr.clone();
    lua.set_thread_collection_callback(move |thread_ptr| {
        let old_thread_ptr = thread_data.0.load(Ordering::Relaxed);
        status_ptr2.store(old_thread_ptr == thread_ptr.0, Ordering::Relaxed);
    });

    // Thead will be destroyed after GC cycle
    drop(new_thread);
    lua.gc_collect()?;

    assert!(status_ptr.load(Ordering::Relaxed));

    Ok(())
}

#[test]
fn test_memory_category() -> Result<()> {
    let lua = Lua::new();

    lua.set_memory_category("main").unwrap();

    // Invalid category names should be rejected
    let err = lua.set_memory_category("invalid$");
    assert!(err.is_err());

    for i in 0..254 {
        let name = format!("category_{}", i);
        lua.set_memory_category(&name).unwrap();
    }
    // 255th category should fail
    let err = lua.set_memory_category("category_254");
    assert!(err.is_err());

    Ok(())
}

// TODO: Fix this test
#[test]
fn test_heap_dump() -> Result<()> {
    let lua = Lua::new();

    // Assign a new memory category and create few objects
    lua.set_memory_category("test_category")?;
    let _t = lua.create_table()?;
    let _ud = lua.create_any_userdata("hello, world", None)?;

    let dump = lua.heap_dump()?;

    assert!(dump.size() > 0);
    let size_by_category = dump.size_by_category();
    assert_eq!(size_by_category.len(), 2);
    assert!(size_by_category.contains_key("test_category"));
    assert!(size_by_category["main"] < dump.size());

    // Check size by type within the category
    let size_by_type = dump.size_by_type(Some("test_category"));
    assert!(!size_by_type.is_empty());
    assert!(size_by_type.contains_key("table"));
    assert!(size_by_type.contains_key("userdata"));
    // Try non-existent category
    let size_by_type2 = dump.size_by_type(Some("non_existent_category"));
    assert!(size_by_type2.is_empty());
    // Remove category filter
    let size_by_type_all = dump.size_by_type(None);
    assert!(size_by_type.len() < size_by_type_all.len());

    Ok(())
}

#[test]
fn test_thread_state_change_event() -> Result<()> {
    let lua = Lua::new();

    let state_changes = Arc::new(std::sync::Mutex::new(Vec::new()));

    let changes2 = state_changes.clone();
    lua.set_thread_state_change_callback(move |lua, _thread, status, args| {
        let mut changes = changes2.lock().unwrap();
        if status == mluau::ThreadStatus::Resumable {
            changes.push(("yield", args.into_vec().len()));
        } else if status == mluau::ThreadStatus::Finished {
            changes.push(("ok", args.into_vec().len()));
        } else {
            changes.push(("error", args.into_vec().len()));
        }
        lua.create_function(|_lua, _: ()| Ok(1u32))?;
        Ok(())
    });

    lua.set_thread_creation_callback(|_lua, th| {
        th.attach_thread_state_change_callback(); 
        Ok(())
    });

    let thread = lua.create_thread(lua.load(r#"
        coroutine.yield(1, 2)
        return 3, 4, 5
    "#).into_function()?)?;

    thread.resume::<(), crate::Error>(())?;
    thread.resume::<(), crate::Error>(())?;

    {
        let changes = state_changes.lock().unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0], ("yield", 2));
        assert_eq!(changes[1], ("ok", 3));
    }

    // Check error case
    let thread2 = lua.create_thread(lua.load(r#"
        local a = 1
        local b = 2
        error("test error")
    "#).into_function()?)?;

    let _ = thread2.resume::<(), crate::Error>(());
    let changes = state_changes.lock().unwrap();
    assert_eq!(changes.len(), 3);
    assert_eq!(changes[2].0, "error");
    // We expect the stack to have the error, maybe some locals. We just check it's > 0.
    assert!(changes[2].1 > 0);

    lua.remove_thread_state_change_callback();

    Ok(())
}

#[path = "luau/require.rs"]
mod require;
