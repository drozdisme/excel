//! Minimal HKEY_CURRENT_USER / HKEY_LOCAL_MACHINE registry access via advapi32 (no crates).
#![allow(clippy::upper_case_acronyms)]
use std::ffi::c_void;

type HKEY = *mut c_void;
pub const HKEY_CURRENT_USER: HKEY = 0x8000_0001u32 as usize as HKEY;
pub const HKEY_LOCAL_MACHINE: HKEY = 0x8000_0002u32 as usize as HKEY;
const KEY_READ: u32 = 0x20019;
const KEY_ALL_ACCESS: u32 = 0xF003F;
const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;
const ERROR_SUCCESS: i32 = 0;

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegCreateKeyExW(k: HKEY, sub: *const u16, r: u32, class: *const u16, opt: u32, sam: u32, sa: *const c_void, out: *mut HKEY, disp: *mut u32) -> i32;
    fn RegOpenKeyExW(k: HKEY, sub: *const u16, opt: u32, sam: u32, out: *mut HKEY) -> i32;
    fn RegCloseKey(k: HKEY) -> i32;
    fn RegSetValueExW(k: HKEY, name: *const u16, r: u32, kind: u32, data: *const u8, len: u32) -> i32;
    fn RegQueryValueExW(k: HKEY, name: *const u16, r: *mut u32, kind: *mut u32, data: *mut u8, len: *mut u32) -> i32;
    fn RegDeleteValueW(k: HKEY, name: *const u16) -> i32;
    fn RegDeleteTreeW(k: HKEY, sub: *const u16) -> i32;
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn check(rc: i32, what: &str) -> Result<(), String> {
    if rc == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry {what}: error {rc}")) }
}

impl Key {
    pub fn create(root: HKEY, path: &str) -> Result<Key, String> {
        let mut h = std::ptr::null_mut();
        let rc = unsafe { RegCreateKeyExW(root, w(path).as_ptr(), 0, std::ptr::null(), 0, KEY_ALL_ACCESS, std::ptr::null(), &mut h, std::ptr::null_mut()) };
        check(rc, path).map(|_| Key(h))
    }
    pub fn open(root: HKEY, path: &str, write: bool) -> Option<Key> {
        let mut h = std::ptr::null_mut();
        let rc = unsafe { RegOpenKeyExW(root, w(path).as_ptr(), 0, if write { KEY_ALL_ACCESS } else { KEY_READ }, &mut h) };
        (rc == ERROR_SUCCESS).then_some(Key(h))
    }
    pub fn set_str(&self, name: &str, v: &str) -> Result<(), String> {
        let data = w(v);
        let rc = unsafe { RegSetValueExW(self.0, w(name).as_ptr(), 0, REG_SZ, data.as_ptr() as *const u8, (data.len() * 2) as u32) };
        check(rc, name)
    }
    pub fn set_dword(&self, name: &str, v: u32) -> Result<(), String> {
        let rc = unsafe { RegSetValueExW(self.0, w(name).as_ptr(), 0, REG_DWORD, &v as *const u32 as *const u8, 4) };
        check(rc, name)
    }
    pub fn get_str(&self, name: &str) -> Option<String> {
        let n = w(name);
        let (mut kind, mut len) = (0u32, 0u32);
        if unsafe { RegQueryValueExW(self.0, n.as_ptr(), std::ptr::null_mut(), &mut kind, std::ptr::null_mut(), &mut len) } != ERROR_SUCCESS || kind != REG_SZ {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
        let mut len2 = (buf.len() * 2) as u32;
        if unsafe { RegQueryValueExW(self.0, n.as_ptr(), std::ptr::null_mut(), &mut kind, buf.as_mut_ptr() as *mut u8, &mut len2) } != ERROR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
    pub fn delete_value(&self, name: &str) {
        unsafe { RegDeleteValueW(self.0, w(name).as_ptr()) };
    }
}

pub fn delete_tree(root: HKEY, path: &str) {
    unsafe { RegDeleteTreeW(root, w(path).as_ptr()) };
}
