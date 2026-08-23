use std::borrow::Cow;
use std::collections::HashSet;

use mluau::{Lua, Result, String};

#[test]
fn test_string_compare() {
    fn with_str<F: FnOnce(String)>(s: &str, f: F) {
        f(Lua::new().create_string(s).unwrap());
    }

    // Tests that all comparisons we want to have are usable
    with_str("teststring", |t| assert_eq!(t, "teststring")); // &str
    with_str("teststring", |t| assert_eq!(t, b"teststring")); // &[u8]
    with_str("teststring", |t| assert_eq!(t, b"teststring".to_vec())); // Vec<u8>
    with_str("teststring", |t| assert_eq!(t, "teststring".to_string())); // String
    with_str("teststring", |t| assert_eq!(t, t)); // mluau::String
    with_str("teststring", |t| assert_eq!(t, Cow::from(b"teststring".as_ref()))); // Cow (borrowed)
    with_str("bla", |t| assert_eq!(t, Cow::from(b"bla".to_vec()))); // Cow (owned)

    // Test ordering
    with_str("a", |a| {
        assert!(!(a < a));
        assert!(!(a > a));
    });
    with_str("a", |a| assert!(a < "b"));
    with_str("a", |a| assert!(a < b"b"));
    with_str("a", |a| with_str("b", |b| assert!(a < b)));
}

#[test]
fn test_string_views() -> Result<()> {
    let lua = Lua::new();

    lua.load(
        r#"
        ok = "null bytes are valid utf-8, wh\0 knew?"
        err = "but \255 isn't :("
        empty = ""
    "#,
    )
    .call::<()>(())?;

    let globals = lua.globals();
    let ok: String = globals.get("ok")?;
    let err: String = globals.get("err")?;
    let empty: String = globals.get("empty")?;

    assert_eq!(ok.to_str()?, "null bytes are valid utf-8, wh\0 knew?");
    assert_eq!(ok.to_string_lossy(), "null bytes are valid utf-8, wh\0 knew?");
    assert_eq!(ok.as_bytes(), &b"null bytes are valid utf-8, wh\0 knew?"[..]);

    assert!(err.to_str().is_err());
    assert_eq!(err.as_bytes(), &b"but \xff isn't :("[..]);

    assert_eq!(empty.to_str()?, "");
    assert_eq!(empty.as_bytes_with_nul(), &[0]);
    assert_eq!(empty.as_bytes(), &[]);

    Ok(())
}

#[test]
fn test_string_from_bytes() -> Result<()> {
    let lua = Lua::new();

    let rs = lua.create_string(&[0, 1, 2, 3, 0, 1, 2, 3])?;
    assert_eq!(rs.as_bytes(), &[0, 1, 2, 3, 0, 1, 2, 3]);

    Ok(())
}

#[test]
fn test_string_hash() -> Result<()> {
    let lua = Lua::new();

    let set: HashSet<String> = lua.load(r#"{"hello", "world", "abc", 321}"#).eval()?;
    assert_eq!(set.len(), 4);
    assert!(set.contains(&lua.create_string("hello")?));
    assert!(set.contains(&lua.create_string("world")?));
    assert!(set.contains(&lua.create_string("abc")?));
    assert!(set.contains(&lua.create_string("321")?));
    assert!(!set.contains(&lua.create_string("Hello")?));

    Ok(())
}

#[test]
fn test_string_fmt_debug() -> Result<()> {
    let lua = Lua::new();

    // Valid utf8
    let s = lua.create_string("hello")?;
    assert_eq!(format!("{s:?}"), r#""hello""#);
    assert_eq!(format!("{:?}", s.to_str()?), r#""hello""#);
    assert_eq!(format!("{:?}", s.as_bytes()), "[104, 101, 108, 108, 111]");

    // Invalid utf8
    let s = lua.create_string(b"hello\0world\r\n\t\xf0\x90\x80")?;
    assert_eq!(format!("{s:?}"), r#"b"hello\0world\r\n\t\xf0\x90\x80""#);

    Ok(())
}

#[test]
fn test_string_pointer() -> Result<()> {
    let lua = Lua::new();

    let str1 = lua.create_string("hello")?;
    let str2 = lua.create_string("hello")?;

    // Lua uses string interning, so these should be the same
    assert_eq!(str1.to_pointer(), str2.to_pointer());

    Ok(())
}

#[test]
fn test_string_display() -> Result<()> {
    let lua = Lua::new();

    let s = lua.create_string("hello")?;
    assert_eq!(format!("{}", s.display()), "hello");

    // With invalid utf8
    let s = lua.create_string(b"hello\0world\xFF")?;
    assert_eq!(format!("{}", s.display()), "hello\0world�");

    Ok(())
}

#[test]
fn test_string_wrap() -> Result<()> {
    let lua = Lua::new();

    let s = String::wrap("hello, world");
    lua.globals().set("s", s)?;
    assert_eq!(lua.globals().get::<String>("s")?, "hello, world");

    let s2 = String::wrap("hello, world (owned)".to_string());
    lua.globals().set("s2", s2)?;
    assert_eq!(lua.globals().get::<String>("s2")?, "hello, world (owned)");

    Ok(())
}

#[test]
fn test_bytes_into_iter() -> Result<()> {
    let lua = Lua::new();

    let s = lua.create_string("hello")?;
    let bytes = s.as_bytes();

    for (i, &b) in bytes.into_iter().enumerate() {
        assert_eq!(b, s.as_bytes()[i]);
    }

    Ok(())
}

#[test]
fn test_external_string() -> Result<()> {
    let lua = Lua::new();

    // Statically allocated strings
    let static_str: &'static str = "hello static external\0";
    let s = lua.create_external_string(static_str)?;
    assert_eq!(s.to_str()?, "hello static external");

    // Vec<u8> string
    let vec_str = b"hello vec external".to_vec();
    let s = lua.create_external_string(vec_str)?;
    assert_eq!(s.to_str()?, "hello vec external");

    // Standard string
    let std_str = "hello std external".to_string();
    let s = lua.create_external_string(std_str)?;
    assert_eq!(s.to_str()?, "hello std external");

    // BString
    let bstr_str = bstr::BString::from("hello bstr external");
    let s = lua.create_external_string(bstr_str)?;
    assert_eq!(s.to_str()?, "hello bstr external");

    // Validate that Luau code can interact seamlessly with it
    lua.globals().set("ext_str", s)?;
    let result: bool = lua
        .load(
            r#"
        local str = ext_str
        assert(string.len(str) == 19)
        assert(string.sub(str, 1, 5) == "hello")
        assert(string.match(str, "bstr") == "bstr")
        return str == "hello bstr external"
    "#,
        )
        .eval()?;
    assert!(result, "Luau code failed to process the external string properly");

    // Bytes strings
    #[cfg(feature = "bytes")]
    {
        let bytes_str = bytes::Bytes::from_static(b"hello bytes external\0");
        let s = lua.create_external_string(bytes_str)?;
        assert_eq!(s.to_str()?, "hello bytes external");

        let mut bytes_mut_str = bytes::BytesMut::new();
        bytes_mut_str.extend_from_slice(b"hello bytes mut external");
        let s = lua.create_external_string(bytes_mut_str)?;
        assert_eq!(s.to_str()?, "hello bytes mut external");
    }

    Ok(())
}
