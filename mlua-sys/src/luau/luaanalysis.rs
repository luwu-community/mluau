//! Contains definitions from `luaanalysis.h`, Luwu's type checking and linting API.

use std::marker::{PhantomData, PhantomPinned};
use std::os::raw::{c_char, c_int};

use super::lua::lua_State;

#[repr(C)]
pub struct luwu_Analysis {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

// Values of luwu_Diagnostic::severity (luwu_DiagnosticSeverity)
pub const LUWU_DIAGNOSTIC_ERROR: c_int = 0;
pub const LUWU_DIAGNOSTIC_WARNING: c_int = 1;
pub const LUWU_DIAGNOSTIC_NOTE: c_int = 2;

#[repr(C)]
pub struct luwu_Diagnostic {
    pub module: *const c_char,
    pub kind: *const c_char,
    pub message: *const c_char,
    pub severity: c_int,
    pub beginline: c_int,
    pub begincolumn: c_int,
    pub endline: c_int,
    pub endcolumn: c_int,
}

// No function raises into its caller: the C API catches every C++ exception and returns a status instead.
unsafe extern "C" {
    pub fn luwu_analysis_enabledefaultflags();
    pub fn luwu_analysis_new() -> *mut luwu_Analysis;
    pub fn luwu_analysis_free(analysis: *mut luwu_Analysis);
    pub fn luwu_analysis_loaddefinitions(analysis: *mut luwu_Analysis, path: *const c_char) -> c_int;
    pub fn luwu_analysis_setresolver(analysis: *mut luwu_Analysis, state: *mut lua_State, r#ref: c_int);
    pub fn luwu_analysis_queue(analysis: *mut luwu_Analysis, path: *const c_char);
    pub fn luwu_analysis_check(analysis: *mut luwu_Analysis) -> c_int;
    pub fn luwu_analysis_diagnosticcount(analysis: *mut luwu_Analysis) -> usize;
    pub fn luwu_analysis_getdiagnostic(
        analysis: *mut luwu_Analysis,
        index: usize,
        out: *mut luwu_Diagnostic,
    ) -> c_int;
    pub fn luwu_analysis_legend(analysis: *mut luwu_Analysis) -> *const c_char;
    pub fn luwu_analysis_lasterror(analysis: *mut luwu_Analysis) -> *const c_char;
}
