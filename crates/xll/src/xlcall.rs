//! Minimal Excel C API (XLCALL) binding: XLOPER12 layout, constants and the Excel12 call.
#![allow(non_upper_case_globals, non_camel_case_types, dead_code, clippy::upper_case_acronyms)]
use std::ffi::c_void;

pub type RW = i32;
pub type COL = i32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct XLREF12 {
    pub rw_first: RW,
    pub rw_last: RW,
    pub col_first: COL,
    pub col_last: COL,
}

#[repr(C)]
pub struct XLMREF12 {
    pub count: u16,
    pub reftbl: [XLREF12; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SRef {
    pub count: u16,
    pub r: XLREF12,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MRef {
    pub lpmref: *mut XLMREF12,
    pub id_sheet: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Arr {
    pub lparray: *mut XLOPER12,
    pub rows: RW,
    pub columns: COL,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union Val {
    pub num: f64,
    pub str_: *mut u16,
    pub xbool: i32,
    pub err: i32,
    pub w: i32,
    pub sref: SRef,
    pub mref: MRef,
    pub array: Arr,
    pub pad: [u64; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XLOPER12 {
    pub val: Val,
    pub xltype: u32,
}

const _: () = assert!(std::mem::size_of::<XLOPER12>() == 32);

pub const xltypeNum: u32 = 0x0001;
pub const xltypeStr: u32 = 0x0002;
pub const xltypeBool: u32 = 0x0004;
pub const xltypeRef: u32 = 0x0008;
pub const xltypeErr: u32 = 0x0010;
pub const xltypeMulti: u32 = 0x0040;
pub const xltypeMissing: u32 = 0x0080;
pub const xltypeNil: u32 = 0x0100;
pub const xltypeSRef: u32 = 0x0400;
pub const xltypeInt: u32 = 0x0800;
pub const xlbitXLFree: u32 = 0x1000;
pub const xlbitDLLFree: u32 = 0x4000;

pub const xlerrNull: i32 = 0;
pub const xlerrDiv0: i32 = 7;
pub const xlerrValue: i32 = 15;
pub const xlerrRef: i32 = 23;
pub const xlerrName: i32 = 29;
pub const xlerrNum: i32 = 36;
pub const xlerrNA: i32 = 42;
pub const xlerrGettingData: i32 = 43;

pub const xlretSuccess: i32 = 0;

const xlCommand: i32 = 0x8000;
const xlSpecial: i32 = 0x4000;
pub const xlFree: i32 = xlSpecial;
pub const xlCoerce: i32 = 2 | xlSpecial;
pub const xlSet: i32 = 3 | xlSpecial;
pub const xlSheetId: i32 = 4 | xlSpecial;
pub const xlSheetNm: i32 = 5 | xlSpecial;
pub const xlAbort: i32 = 6 | xlSpecial;
pub const xlGetHwnd: i32 = 8 | xlSpecial;
pub const xlGetName: i32 = 9 | xlSpecial;

pub const xlfCaller: i32 = 89;
pub const xlfNow: i32 = 74;
pub const xlfRegister: i32 = 149;
pub const xlfAddMenu: i32 = 152;
pub const xlfDeleteMenu: i32 = 158;
pub const xlfGetBar: i32 = 182;
pub const xlfGetDocument: i32 = 188;
pub const xlfUnregister: i32 = 201;
pub const xlfGetWorkbook: i32 = 268;

// Numbers cross-checked against Excel-DNA's XlCall.cs (mirror of the SDK's xlcall.h).
pub const xlcCalculateNow: i32 = 31 | xlCommand;
pub const xlcFormula: i32 = 96 | xlCommand;
pub const xlcAlert: i32 = 118 | xlCommand;
pub const xlcMessage: i32 = 122 | xlCommand;
pub const xlcEcho: i32 = 141 | xlCommand;
pub const xlcOnTime: i32 = 148 | xlCommand;
pub const xlcOptionsCalculation: i32 = 318 | xlCommand;
pub const xlcSaveCopyAs: i32 = 456 | xlCommand;

type MdCallBack12 = unsafe extern "system" fn(i32, i32, *mut *mut XLOPER12, *mut XLOPER12) -> i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(m: *mut c_void, name: *const u8) -> *mut c_void;
}

fn callback() -> Option<MdCallBack12> {
    static CB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CB.get_or_init(|| unsafe { GetProcAddress(GetModuleHandleW(std::ptr::null()), c"MdCallBack12".as_ptr() as *const u8) as usize });
    // SAFETY: MdCallBack12 is exported by EXCEL.EXE with this signature.
    (p != 0).then(|| unsafe { std::mem::transmute::<usize, MdCallBack12>(p) })
}

/// An XLOPER12 owned by Excel (freed with xlFree on drop).
pub struct XlResult(pub XLOPER12);

impl Drop for XlResult {
    fn drop(&mut self) {
        let t = self.0.xltype & !(xlbitXLFree | xlbitDLLFree);
        if matches!(t, xltypeStr | xltypeMulti | xltypeRef) {
            let mut p: *mut XLOPER12 = &mut self.0;
            let mut dummy = nil();
            if let Some(cb) = callback() {
                unsafe { cb(xlFree, 1, &mut p, &mut dummy) };
            }
        }
    }
}

impl XlResult {
    pub fn num(&self) -> Option<f64> {
        match self.0.xltype & 0xFFF {
            xltypeNum => Some(unsafe { self.0.val.num }),
            xltypeInt => Some(unsafe { self.0.val.w } as f64),
            xltypeBool => Some(unsafe { self.0.val.xbool } as f64),
            _ => None,
        }
    }
    pub fn text(&self) -> Option<String> {
        if self.0.xltype & 0xFFF != xltypeStr {
            return None;
        }
        unsafe {
            let p = self.0.val.str_;
            if p.is_null() {
                return Some(String::new());
            }
            let len = *p as usize;
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(p.add(1), len)))
        }
    }
}

/// Call Excel. Arguments are borrowed for the duration of the call.
pub fn excel(func: i32, args: &mut [XLOPER12]) -> Result<XlResult, i32> {
    let cb = callback().ok_or(-1)?;
    let mut ptrs: Vec<*mut XLOPER12> = args.iter_mut().map(|a| a as *mut XLOPER12).collect();
    let mut res = nil();
    let rc = unsafe { cb(func, ptrs.len() as i32, ptrs.as_mut_ptr(), &mut res) };
    if rc == xlretSuccess { Ok(XlResult(res)) } else { Err(rc) }
}

pub fn nil() -> XLOPER12 {
    XLOPER12 { val: Val { pad: [0; 3] }, xltype: xltypeNil }
}
pub fn missing() -> XLOPER12 {
    XLOPER12 { val: Val { pad: [0; 3] }, xltype: xltypeMissing }
}
pub fn num(v: f64) -> XLOPER12 {
    XLOPER12 { val: Val { num: v }, xltype: xltypeNum }
}
pub fn boolean(b: bool) -> XLOPER12 {
    XLOPER12 { val: Val { xbool: b as i32 }, xltype: xltypeBool }
}
pub fn err(e: i32) -> XLOPER12 {
    XLOPER12 { val: Val { err: e }, xltype: xltypeErr }
}

/// Owned Excel string storage (length-prefixed UTF-16); keep alive while the XLOPER12 is used.
pub struct XlStr(Vec<u16>);
impl XlStr {
    pub fn new(s: &str) -> Self {
        let mut v: Vec<u16> = s.encode_utf16().take(32767).collect();
        v.insert(0, v.len() as u16);
        XlStr(v)
    }
    pub fn oper(&mut self) -> XLOPER12 {
        XLOPER12 { val: Val { str_: self.0.as_mut_ptr() }, xltype: xltypeStr }
    }
}

/// Reference to a rectangle on a given sheet id.
pub struct XlRef {
    mref: Box<XLMREF12>,
    id: usize,
}
impl XlRef {
    pub fn new(id_sheet: usize, r1: u32, c1: u32, r2: u32, c2: u32) -> Self {
        let r = XLREF12 { rw_first: r1 as i32 - 1, rw_last: r2 as i32 - 1, col_first: c1 as i32 - 1, col_last: c2 as i32 - 1 };
        XlRef { mref: Box::new(XLMREF12 { count: 1, reftbl: [r] }), id: id_sheet }
    }
    pub fn oper(&mut self) -> XLOPER12 {
        XLOPER12 { val: Val { mref: MRef { lpmref: &mut *self.mref, id_sheet: self.id } }, xltype: xltypeRef }
    }
}
