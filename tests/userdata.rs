use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use mluau::{
    AnyUserData, Error, Function, Lua, LuaUserDataExt, LuaUserDataMutExt, MetaMethod, Result, String, TypedUserData as UserDataRef, UserData, UserDataMethods, UserDataMethodsMut, UserDataMut, UserDataMutBorrowExt, Value, Variadic
};

#[test]
fn test_userdata() -> Result<()> {
    struct UserData1(i64);
    struct UserData2(Box<i64>);

    impl UserData for UserData1 {}
    impl UserData for UserData2 {}

    let lua = Lua::new();
    let userdata1 = lua.create_userdata(UserData1(1))?;
    let userdata2 = lua.create_userdata(UserData2(Box::new(2)))?;

    assert_eq!(userdata1.borrow::<UserData1>().unwrap().0, 1);
    assert_eq!(*userdata2.borrow::<UserData2>().unwrap().0, 2);

    let userdata1 = lua.create_any_userdata_with_tag::<_, 127>(1292, None)?;
    assert_eq!(*userdata1.borrow_with_tag::<i32, 127>().unwrap(), 1292);

    struct Ud(i32);
    impl UserData<55> for Ud {
        fn add_methods<M: UserDataMethods<Self, 55>>(methods: &mut M) {
            methods.add_method("get", |_, data, ()| Ok(data.0 + 1));
        }
    }
    let my_ud = lua.create_userdata(Ud(128))?;
    assert_eq!(my_ud.borrow_with_tag::<Ud, 55>().unwrap().0, 128);
    assert_eq!(my_ud.get::<Function>("get")?.call::<i32>(my_ud)?, 129);
    Ok(())
}

#[test]
fn test_method_variadic() -> Result<()> {
    struct MyUserData(AtomicI64);

    impl UserData for MyUserData {
        fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
            methods.add_method("get", |_, data, ()| Ok(data.0.load(std::sync::atomic::Ordering::SeqCst)));
            methods.add_method("add", |_, data, vals: Variadic<i64>| {
                data.0.fetch_add(vals.into_iter().sum::<i64>(), std::sync::atomic::Ordering::SeqCst);
                Ok(())
            });
        }
    }

    let lua = Lua::new();
    let globals = lua.globals();
    globals.set("userdata", MyUserData(0.into()))?;
    lua.load("userdata:add(1, 5, -10)").call::<()>(())?;
    let ud: UserDataRef<MyUserData> = globals.get("userdata")?;
    assert_eq!(ud.0.load(std::sync::atomic::Ordering::SeqCst), -4);

    Ok(())
}

#[test]
fn test_metamethods() -> Result<()> {
    #[derive(Copy, Clone)]
    struct MyUserData(i64);

    impl UserData for MyUserData {
        fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
            methods.add_method("get", |_, data, ()| {
                println!("Called get!");
                Ok(data.0)
            });
            methods.add_meta_function(
                MetaMethod::Add,
                |_, (lhs, rhs): (UserDataRef<Self>, UserDataRef<Self>)| Ok(MyUserData(lhs.0 + rhs.0)),
            );
            methods.add_meta_function(
                MetaMethod::Sub,
                |_, (lhs, rhs): (UserDataRef<Self>, UserDataRef<Self>)| Ok(MyUserData(lhs.0 - rhs.0)),
            );
            methods.add_meta_function(
                MetaMethod::Eq,
                |_, (lhs, rhs): (UserDataRef<Self>, UserDataRef<Self>)| Ok(lhs.0 == rhs.0),
            );
            methods.add_meta_method(MetaMethod::Index, |_, data, index: String| {
                if index.to_str()? == "inner" {
                    Ok(data.0)
                } else {
                    Err(mluau::Error::external("no such custom index"))
                }
            });
        }
    }

    let lua = Lua::new();
    let globals = lua.globals();
    globals.set("userdata1", MyUserData(7))?;
    globals.set("userdata2", MyUserData(3))?;
    globals.set("userdata3", MyUserData(3))?;
    assert_eq!(
        lua.load("userdata1 + userdata2")
            .eval::<UserDataRef<MyUserData>>()?
            .0,
        10
    );

    assert_eq!(
        lua.load("userdata1 - userdata2")
            .eval::<UserDataRef<MyUserData>>()?
            .0,
        4
    );
    assert_eq!(lua.load("userdata1:get()").eval::<i64>()?, 7);
    assert_eq!(lua.load("userdata2.inner").eval::<i64>()?, 3);
    assert!(lua.load("userdata2.nonexist_field").eval::<()>().is_err());

    let userdata2: Value = globals.get("userdata2")?;
    let userdata3: Value = globals.get("userdata3")?;

    assert!(lua.load("userdata2 == userdata3").eval::<bool>()?);
    assert!(userdata2 != userdata3); // because references are differ
    assert!(userdata2.equals(&userdata3)?);

    let userdata1: AnyUserData = globals.get("userdata1")?;
    assert!(userdata1.metatable().unwrap().contains_key(MetaMethod::Add)?);
    assert!(userdata1.metatable().unwrap().contains_key(MetaMethod::Sub)?);
    assert!(userdata1.metatable().unwrap().contains_key(MetaMethod::Index)?);
    assert!(!userdata1.metatable().unwrap().contains_key(MetaMethod::Pow)?);

    Ok(())
}

#[test]
fn test_gc_userdata() -> Result<()> {
    struct MyUserdata {
        id: u8,
    }

    impl UserData for MyUserdata {
        fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
            methods.add_method("access", |_, this, ()| {
                assert_eq!(this.id, 123);
                Ok(())
            });
        }
    }

    let lua = Lua::new();
    lua.globals().set("userdata", MyUserdata { id: 123 })?;

    assert!(lua
        .load(
            r#"
            local tbl = setmetatable({
                userdata = userdata
            }, { __gc = function(self)
                -- resurrect userdata
                hatch = self.userdata
            end })

            tbl = nil
            userdata = nil  -- make table and userdata collectable
            collectgarbage("collect")
            hatch:access()
        "#
        )
        .call::<()>(())
        .is_err());

    Ok(())
}

#[test]
fn test_functions() -> Result<()> {
    struct MyUserData(i64);

    impl UserData for MyUserData {
        const USE_NAMECALL: bool = false;
        fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
            methods.add_function("get_value_fn", |_, ud: AnyUserData| {
                Ok(ud.borrow::<MyUserData>().unwrap().0)
            });
            methods.add_function("get_constant", |_, ()| Ok(7));
            methods.add_function("not_me", |_, ud: AnyUserData| {
                Ok(ud.borrow::<MyUserData>().is_none())
            });
        }
    }

    let lua = Lua::new();
    let globals = lua.globals();
    let userdata = lua.create_userdata(MyUserData(42))?;
    globals.set("userdata", &userdata)?;
    lua.load(
        r#"
        function get_it()
            return userdata:get_value_fn()
        end

        function get_constant()
            return userdata.get_constant()
        end

        function not_me()
            local s = newproxy(true)
            return userdata.not_me(s)
        end
    "#,
    )
    .call::<()>(())?;
    let get = globals.get::<Function>("get_it")?;
    let get_constant = globals.get::<Function>("get_constant")?;
    assert_eq!(get.call::<i64>(())?, 42);
    assert_eq!(get.call::<i64>(())?, 42);
    assert_eq!(get_constant.call::<i64>(())?, 7);

    assert!(globals.get::<Function>("not_me")?.call::<bool>(()).unwrap());

    Ok(())
}

#[test]
fn test_metatable() -> Result<()> {
    #[derive(Copy, Clone)]
    struct MyUserData;

    impl UserData for MyUserData {
        const USE_NAMECALL: bool = false;
        fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
            methods.add_function("my_type_name", |_, data: AnyUserData| {
                let metatable = data.metatable().unwrap();
                metatable.get::<String>(MetaMethod::Type)
            });
        }
    }

    let lua = Lua::new();
    let globals = lua.globals();
    globals.set("ud", MyUserData)?;
    lua.load(r#"assert(ud:my_type_name() == "MyUserData")"#).call::<()>(())?;

    lua.load(r#"assert(tostring(ud):sub(1, 11) == "MyUserData:")"#)
        .call::<()>(())?;

    lua.load(r#"assert(typeof(ud) == "MyUserData")"#).call::<()>(())?;

    let ud: AnyUserData = globals.get("ud")?;
    let metatable = ud.metatable().unwrap();

    let mut methods = metatable
        .pairs()
        .map(|kv: Result<(std::string::String, Value)>| Ok(kv?.0))
        .collect::<Result<Vec<_>>>()?;
    methods.sort();

    assert_eq!(methods, vec!["__index", "__metatable", MetaMethod::Type]);

    Ok(())
}

#[test]
fn test_userdata_method_errors() -> Result<()> {
    struct MyUserData(i64);

    impl UserData for MyUserData {
        fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
            methods.add_method("get_value", |_, data, ()| Ok(data.0));
        }
    }

    let lua = Lua::new();

    let ud = lua.create_userdata(MyUserData(123))?;
    let res = ud.get::<Function>("get_value")?.call::<()>("not a userdata");
    match res {
        Err(Error::RuntimeError(msg)) => {
            assert!(msg.contains("bad argument #1: error converting Lua"));
            assert!(msg.contains("expected userdata of type"));
        }
        r => panic!("expected RuntimeError, got {r:?}"),
    }

    Ok(())
}

#[test]
fn test_userdata_pointer() -> Result<()> {
    let lua = Lua::new();

    let ud1 = lua.create_any_userdata("hello", None)?;
    let ud2 = lua.create_any_userdata("hello", None)?;

    assert_eq!(ud1.to_pointer(), ud1.clone().to_pointer());
    // Different userdata objects with the same value should have different pointers
    assert_ne!(ud1.to_pointer(), ud2.to_pointer());

    Ok(())
}


#[test]
fn test_nested_userdata_gc() -> Result<()> {
    let lua = Lua::new();

    let counter = Arc::new(());
    let arr = vec![lua.create_any_userdata(counter.clone(), None)?];
    let arr_ud = lua.create_any_userdata(arr, None)?;

    assert_eq!(Arc::strong_count(&counter), 2);
    drop(arr_ud);
    // On first iteration Lua will destroy the array, on second - userdata
    lua.gc_collect()?;
    lua.gc_collect()?;
    assert_eq!(Arc::strong_count(&counter), 1);

    Ok(())
}

#[test]
fn test_userdata_meta_function() -> Result<()> {
    struct MyAddUserData(i32);
    
    impl UserData for MyAddUserData {
        fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
            methods.add_meta_function(MetaMethod::Add, |_lua, (left, right): (crate::Value, crate::Value)| {
                // Determine which one is the userdata and which is the number
                let mut total = 0;
                
                if let crate::Value::UserData(ud) = &left {
                    total += ud.borrow::<MyAddUserData>().unwrap().0;
                } else if let crate::Value::Number(n) = &left {
                    total += *n as i32;
                } else if let crate::Value::Integer(n) = &left {
                    total += *n as i32;
                }
                
                if let crate::Value::UserData(ud) = &right {
                    total += ud.borrow::<MyAddUserData>().unwrap().0;
                } else if let crate::Value::Number(n) = &right {
                    total += *n as i32;
                } else if let crate::Value::Integer(n) = &right {
                    total += *n as i32;
                }
                
                Ok(total)
            });
        }
    }
    
    let lua = Lua::new();
    lua.globals().set("my_obj", lua.create_userdata(MyAddUserData(10))?)?;
    
    let res: i32 = lua.load("return my_obj + 5").eval()?;
    assert_eq!(res, 15);
    
    let res2: i32 = lua.load("return 5 + my_obj").eval()?;
    assert_eq!(res2, 15);
    
    Ok(())
}

#[test]
fn test_methods() -> Result<()> {
    struct MyUserData(i64);

    impl UserDataMut for MyUserData {
        fn add_methods<M: UserDataMethodsMut<Self>>(methods: &mut M) {
            methods.add_method("get_value", |_, data, ()| Ok(data.0));
            methods.add_method_mut("set_value", |_, data, args| {
                data.0 = args;
                Ok(())
            });
        }
    }

    fn check_methods(lua: &Lua, userdata: AnyUserData) -> Result<()> {
        let globals = lua.globals();
        globals.set("userdata", &userdata)?;
        lua.load(
            r#"
            function get_it()
                return userdata:get_value()
            end

            function set_it(i)
                return userdata:set_value(i)
            end
        "#,
        )
        .call::<()>(())?;
        let get = globals.get::<Function>("get_it")?;
        let set = globals.get::<Function>("set_it")?;
        assert_eq!(get.call::<i64>(())?, 42);
        userdata.with_borrow_mut::<MyUserData, _, _>(|x| x.0 = 64)?;
        assert_eq!(get.call::<i64>(())?, 64);
        set.call::<()>(100)?;
        assert_eq!(get.call::<i64>(())?, 100);
        Ok(())
    }

    let lua = Lua::new();

    check_methods(&lua, lua.create_userdata_mut(MyUserData(42))?)?;

    Ok(())
}
