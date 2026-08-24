use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use mluau::{
    AnyUserData, Error, Function, Lua, LuaUserDataExt, LuaUserDataMutExt, MetaMethod, Result, String, TypedUserData as UserDataRef, UserData, UserDataMethods, UserDataMethodsMut, UserDataMut, UserDataMutBorrowExt, Value, Variadic, direct_userdata_get_field
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

    // Direct field access for Ud of dget
    lua.set_userdata_direct_field_get_cb::<55, _>(|field, ud| {
        let ud = match ud.borrow::<Ud>() {
            Some(u) => u,
            None => return mluau::UserDataDirectFieldGet::Nil
        };

        match field {
            "dget" => mluau::UserDataDirectFieldGet::Number(123.0),
            "uget" => mluau::UserDataDirectFieldGet::Integer(ud.0 as i64),
            _ => mluau::UserDataDirectFieldGet::Nil
        }
    });
    direct_userdata_get_field!(dget, "dget");
    direct_userdata_get_field!(uget, "uget");
    lua.register_userdata_direct_field_get::<55, dget>();
    lua.register_userdata_direct_field_get::<55, uget>();

    let my_ud = lua.create_userdata(Ud(128))?;
    assert_eq!(my_ud.borrow_with_tag::<Ud, 55>().unwrap().0, 128);
    assert_eq!(my_ud.get::<Function>("get")?.call::<i32>(my_ud.clone())?, 129);

    lua.globals().set("my_ud", my_ud)?;
    lua.load("
local ud = my_ud
assert(ud.dget == 123, 'dget must be 123')
assert(ud.uget == 128, 'uget must be 128')
").call::<()>(())?;

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

#[test]
fn test_alignment() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROP_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[repr(C)]
    #[repr(align(64))] // Forces a huge alignment gap to catch any under-allocation/padding bugs
    pub struct AlignmentStressTester {
        pub magic_id: u64,
        pub payload: [f32; 4], 
        _padding_trap: u8,     // Explicitly offsets struct size to create non-standard padding trailing bytes
    }

    impl AlignmentStressTester {
        pub fn new(id: u64) -> Self {
            Self {
                magic_id: id,
                payload: [1.0, 2.0, 3.0, 4.0],
                _padding_trap: 0xAA,
            }
        }
    }

    impl Drop for AlignmentStressTester {
        fn drop(&mut self) {
            // Increment global counter to ensure drop_fn actually executed the inner drop
            DROP_COUNT.fetch_add(1, Ordering::SeqCst);
        }
    }

    let lua = Lua::new();
    lua.create_any_userdata(AlignmentStressTester::new(0xDEADBEEF_12345678), None)?;
    Ok(())
}

#[test]
fn test_wacky_high_alignment_stress() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROP_COUNT_64: AtomicUsize = AtomicUsize::new(0);
    static DROP_COUNT_128: AtomicUsize = AtomicUsize::new(0);
    static DROP_COUNT_256: AtomicUsize = AtomicUsize::new(0);
    static DROP_COUNT_512: AtomicUsize = AtomicUsize::new(0);
    static DROP_COUNT_1024: AtomicUsize = AtomicUsize::new(0);
    static DROP_COUNT_4096: AtomicUsize = AtomicUsize::new(0);

    #[repr(C, align(64))]
    struct Align64 {
        magic: u64,
        data: [u8; 48],
    }
    impl Drop for Align64 {
        fn drop(&mut self) {
            assert_eq!(self.magic, 0x1111_2222_3333_4444);
            DROP_COUNT_64.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[repr(C, align(128))]
    struct Align128 {
        magic: u64,
        values: [f64; 8],
    }
    impl Drop for Align128 {
        fn drop(&mut self) {
            assert_eq!(self.magic, 0x2222_3333_4444_5555);
            DROP_COUNT_128.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[repr(C, align(256))]
    struct Align256 {
        magic: u64,
        table: [u32; 32],
    }
    impl Drop for Align256 {
        fn drop(&mut self) {
            assert_eq!(self.magic, 0x3333_4444_5555_6666);
            DROP_COUNT_256.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[repr(C, align(512))]
    struct Align512 {
        magic: u64,
        payload: [u64; 32],
    }
    impl Drop for Align512 {
        fn drop(&mut self) {
            assert_eq!(self.magic, 0x4444_5555_6666_7777);
            DROP_COUNT_512.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[repr(C, align(1024))]
    struct Align1024 {
        magic: u64,
        buffer: [u8; 512],
    }
    impl Drop for Align1024 {
        fn drop(&mut self) {
            assert_eq!(self.magic, 0x5555_6666_7777_8888);
            DROP_COUNT_1024.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[repr(C, align(4096))] // Page-aligned!
    struct Align4096 {
        magic: u64,
        page_chunk: [u64; 256],
    }
    impl Drop for Align4096 {
        fn drop(&mut self) {
            assert_eq!(self.magic, 0x6666_7777_8888_9999);
            DROP_COUNT_4096.fetch_add(1, Ordering::SeqCst);
        }
    }

    const COUNT_PER_TIER: usize = 50;

    let lua = Lua::new();

    // Verify alignment predicate at compile-time / runtime
    assert!(std::mem::align_of::<Align64>() == 64);
    assert!(std::mem::align_of::<Align128>() == 128);
    assert!(std::mem::align_of::<Align256>() == 256);
    assert!(std::mem::align_of::<Align512>() == 512);
    assert!(std::mem::align_of::<Align1024>() == 1024);
    assert!(std::mem::align_of::<Align4096>() == 4096);

    {
        let mut u64_vec = Vec::new();
        let mut u128_vec = Vec::new();
        let mut u256_vec = Vec::new();
        let mut u512_vec = Vec::new();
        let mut u1024_vec = Vec::new();
        let mut u4096_vec = Vec::new();

        for i in 0..COUNT_PER_TIER {
            // Tier 64
            let ud64 = lua.create_any_userdata(
                Align64 {
                    magic: 0x1111_2222_3333_4444,
                    data: [i as u8; 48],
                },
                None,
            )?;
            let r64: mluau::TypedUserData<Align64> = ud64.clone().into::<Align64>().unwrap();
            let ptr64 = &*r64 as *const Align64;
            assert_eq!(ptr64 as usize % 64, 0, "Align64 pointer was not 64-byte aligned!");
            assert_eq!(r64.magic, 0x1111_2222_3333_4444);
            assert_eq!(r64.data[0], i as u8);
            u64_vec.push(ud64);

            // Tier 128
            let ud128 = lua.create_any_userdata(
                Align128 {
                    magic: 0x2222_3333_4444_5555,
                    values: [i as f64 * 1.5; 8],
                },
                None,
            )?;
            let r128: mluau::TypedUserData<Align128> = ud128.clone().into::<Align128>().unwrap();
            let ptr128 = &*r128 as *const Align128;
            assert_eq!(ptr128 as usize % 128, 0, "Align128 pointer was not 128-byte aligned!");
            assert_eq!(r128.magic, 0x2222_3333_4444_5555);
            assert_eq!(r128.values[3], i as f64 * 1.5);
            u128_vec.push(ud128);

            // Tier 256
            let ud256 = lua.create_any_userdata(
                Align256 {
                    magic: 0x3333_4444_5555_6666,
                    table: [i as u32 + 100; 32],
                },
                None,
            )?;
            let r256: mluau::TypedUserData<Align256> = ud256.clone().into::<Align256>().unwrap();
            let ptr256 = &*r256 as *const Align256;
            assert_eq!(ptr256 as usize % 256, 0, "Align256 pointer was not 256-byte aligned!");
            assert_eq!(r256.magic, 0x3333_4444_5555_6666);
            assert_eq!(r256.table[15], i as u32 + 100);
            u256_vec.push(ud256);

            // Tier 512
            let ud512 = lua.create_any_userdata(
                Align512 {
                    magic: 0x4444_5555_6666_7777,
                    payload: [i as u64 * 1000; 32],
                },
                None,
            )?;
            let r512: mluau::TypedUserData<Align512> = ud512.clone().into::<Align512>().unwrap();
            let ptr512 = &*r512 as *const Align512;
            assert_eq!(ptr512 as usize % 512, 0, "Align512 pointer was not 512-byte aligned!");
            assert_eq!(r512.magic, 0x4444_5555_6666_7777);
            assert_eq!(r512.payload[7], i as u64 * 1000);
            u512_vec.push(ud512);

            // Tier 1024
            let ud1024 = lua.create_any_userdata(
                Align1024 {
                    magic: 0x5555_6666_7777_8888,
                    buffer: [0x5A; 512],
                },
                None,
            )?;
            let r1024: mluau::TypedUserData<Align1024> = ud1024.clone().into::<Align1024>().unwrap();
            let ptr1024 = &*r1024 as *const Align1024;
            assert_eq!(ptr1024 as usize % 1024, 0, "Align1024 pointer was not 1024-byte aligned!");
            assert_eq!(r1024.magic, 0x5555_6666_7777_8888);
            assert_eq!(r1024.buffer[255], 0x5A);
            u1024_vec.push(ud1024);

            // Tier 4096 (4KB page alignment)
            let ud4096 = lua.create_any_userdata(
                Align4096 {
                    magic: 0x6666_7777_8888_9999,
                    page_chunk: [i as u64; 256],
                },
                None,
            )?;
            let r4096: mluau::TypedUserData<Align4096> = ud4096.clone().into::<Align4096>().unwrap();
            let ptr4096 = &*r4096 as *const Align4096;
            assert_eq!(ptr4096 as usize % 4096, 0, "Align4096 pointer was not 4096-byte aligned!");
            assert_eq!(r4096.magic, 0x6666_7777_8888_9999);
            assert_eq!(r4096.page_chunk[128], i as u64);
            u4096_vec.push(ud4096);
        }

        // Test passing extreme-aligned userdata across Lua function boundary
        let check_fn = lua.create_function(|_, ud: mluau::AnyUserData| {
            let r4096: mluau::TypedUserData<Align4096> = ud.into::<Align4096>().unwrap();
            let ptr = &*r4096 as *const Align4096;
            assert_eq!(ptr as usize % 4096, 0);
            assert_eq!(r4096.magic, 0x6666_7777_8888_9999);
            Ok(true)
        })?;

        for ud in &u4096_vec {
            let res: bool = check_fn.call(ud.clone())?;
            assert!(res);
        }
    }

    // Run garbage collection and ensure every single boxed high-alignment object is cleanly dropped
    lua.gc_collect()?;
    lua.gc_collect()?;

    assert_eq!(DROP_COUNT_64.load(Ordering::SeqCst), COUNT_PER_TIER);
    assert_eq!(DROP_COUNT_128.load(Ordering::SeqCst), COUNT_PER_TIER);
    assert_eq!(DROP_COUNT_256.load(Ordering::SeqCst), COUNT_PER_TIER);
    assert_eq!(DROP_COUNT_512.load(Ordering::SeqCst), COUNT_PER_TIER);
    assert_eq!(DROP_COUNT_1024.load(Ordering::SeqCst), COUNT_PER_TIER);
    assert_eq!(DROP_COUNT_4096.load(Ordering::SeqCst), COUNT_PER_TIER);

    Ok(())
}