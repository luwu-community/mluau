macro_rules! bug_msg {
    ($arg:expr) => {
        concat!(
            "mlua internal error: ",
            $arg,
            " (this is a bug, please file an issue)"
        )
    };
}

macro_rules! cstr {
    ($s:expr) => {
        concat!($s, "\0") as *const str as *const [::std::os::raw::c_char] as *const ::std::os::raw::c_char
    };
}

macro_rules! mlua_panic {
    ($msg:expr) => {
        panic!(bug_msg!($msg))
    };

    ($msg:expr,) => {
        mlua_panic!($msg)
    };

    ($msg:expr, $($arg:expr),+) => {
        panic!(bug_msg!($msg), $($arg),+)
    };

    ($msg:expr, $($arg:expr),+,) => {
        mlua_panic!($msg, $($arg),+)
    };
}

macro_rules! mlua_assert {
    ($cond:expr, $msg:expr) => {
        assert!($cond, bug_msg!($msg));
    };

    ($cond:expr, $msg:expr,) => {
        mlua_assert!($cond, $msg);
    };

    ($cond:expr, $msg:expr, $($arg:expr),+) => {
        assert!($cond, bug_msg!($msg), $($arg),+);
    };

    ($cond:expr, $msg:expr, $($arg:expr),+,) => {
        mlua_assert!($cond, $msg, $($arg),+);
    };
}

macro_rules! mlua_debug_assert {
    ($cond:expr, $msg:expr) => {
        debug_assert!($cond, bug_msg!($msg));
    };

    ($cond:expr, $msg:expr,) => {
        mlua_debug_assert!($cond, $msg);
    };

    ($cond:expr, $msg:expr, $($arg:expr),+) => {
        debug_assert!($cond, bug_msg!($msg), $($arg),+);
    };

    ($cond:expr, $msg:expr, $($arg:expr),+,) => {
        mlua_debug_assert!($cond, $msg, $($arg),+);
    };
}

macro_rules! mlua_expect {
    ($res:expr, $msg:expr) => {
        $res.expect(bug_msg!($msg))
    };

    ($res:expr, $msg:expr,) => {
        mlua_expect!($res, $msg)
    };
}

// Like `protect_lua!`, but runs the closure on the caller's stack with no Lua call frame and no error handler
// (`protect_lua_raw_closure`). Only for raw API calls that can't run Lua code: allocations, raw gets and sets. The closure
// must index from the top, since positive indices count from the caller's frame.
macro_rules! protect_lua_raw {
    ($state:expr, $nargs:expr, $nresults:expr, $f:expr) => {
        crate::util::protect_lua_raw_closure($state, $nargs, $nresults, $f)
    };
}

macro_rules! protect_lua {
    ($state:expr, $nargs:expr, $nresults:expr, $f:expr) => {
        crate::util::protect_lua_closure($state, $nargs, $nresults, $f)
    };

    ($state:expr, $nargs:expr, $nresults:expr, fn($state_inner:ident) $code:expr) => {
        crate::util::protect_lua_closure($state, $nargs, $nresults, |$state_inner| {
            $code;
        })
    };
}
