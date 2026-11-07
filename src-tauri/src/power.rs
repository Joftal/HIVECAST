//! 阻止系统空闲休眠：录制与观看期间让机器保持唤醒。
//!
//! macOS 用 IOKit 的电源断言（PreventUserIdleSystemSleep），Windows 用
//! `SetThreadExecutionState` 并让置位的线程常驻——这个 API 是按线程生效的，
//! 线程一退出标记就没了。Linux 不在 v1 支持范围内，直接空实现。

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_void, CString};
    use std::os::raw::{c_char, c_uint};
    use std::sync::Mutex;

    type CFStringRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const c_char,
            encoding: c_uint,
        ) -> CFStringRef;
        fn CFRelease(value: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: c_uint,
            name: CFStringRef,
            assertion_id: *mut c_uint,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: c_uint) -> i32;
    }

    /// kCFStringEncodingUTF8
    const UTF8: c_uint = 0x0800_0100;
    /// kIOPMAssertionLevelOn
    const LEVEL_ON: c_uint = 255;
    const ASSERTION_TYPE: &str = "PreventUserIdleSystemSleep";
    // pmset 只认 ASCII 名称：用中文名创建断言，系统会显示成空字符串。
    const ASSERTION_NAME: &str = "HIVECAST is recording or watching";

    // IOPMAssertionID 是 UInt32，不能用 c_ulong（macOS 上是 64 位），否则会写坏栈。
    static ASSERTION: Mutex<Option<c_uint>> = Mutex::new(None);
    // IOKit 不会拷贝断言名称，只保留我们传进去的 CFString 指针；提前 CFRelease
    // 会让 pmset -g assertions 里显示成空字符串。所以名称跟着断言一起活。
    static NAME: Mutex<usize> = Mutex::new(0);

    fn cf_string(value: &str) -> Option<CFStringRef> {
        let text = CString::new(value).ok()?;
        let handle = unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) };
        if handle.is_null() {
            None
        } else {
            Some(handle)
        }
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let mut guard = ASSERTION.lock().map_err(|_| "电源断言锁失效".to_string())?;
        if enabled {
            if guard.is_some() {
                return Ok(());
            }
            let kind = cf_string(ASSERTION_TYPE).ok_or_else(|| "无法创建断言类型".to_string())?;
            let name = cf_string(ASSERTION_NAME).ok_or_else(|| "无法创建断言名称".to_string())?;
            let mut id: c_uint = 0;
            let status = unsafe { IOPMAssertionCreateWithName(kind, LEVEL_ON, name, &mut id) };
            unsafe {
                CFRelease(kind);
            }
            if status != 0 {
                unsafe {
                    CFRelease(name);
                }
                return Err(format!("IOPMAssertionCreateWithName 返回 {status}"));
            }
            *guard = Some(id);
            if let Ok(mut slot) = NAME.lock() {
                if *slot == 0 {
                    *slot = name as usize;
                } else {
                    unsafe {
                        CFRelease(name);
                    }
                }
            }
            Ok(())
        } else {
            if let Some(id) = guard.take() {
                unsafe {
                    IOPMAssertionRelease(id);
                }
            }
            if let Ok(mut slot) = NAME.lock() {
                if *slot != 0 {
                    unsafe {
                        CFRelease(*slot as CFStringRef);
                    }
                    *slot = 0;
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    pub fn enabled() -> bool {
        ASSERTION
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::mpsc::{channel, Sender};
    use std::sync::Mutex;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;

    static WAKE: Mutex<Option<Sender<()>>> = Mutex::new(None);

    pub fn set(enabled: bool) -> Result<(), String> {
        let mut guard = WAKE.lock().map_err(|_| "电源标记锁失效".to_string())?;
        if enabled {
            if guard.is_some() {
                return Ok(());
            }
            let (tx, rx) = channel::<()>();
            std::thread::Builder::new()
                .name("hivecast-wake".into())
                .spawn(move || {
                    // ES_CONTINUOUS 让标记一直有效，直到本线程改成别的值或退出。
                    unsafe {
                        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
                    }
                    let _ = rx.recv();
                    unsafe {
                        SetThreadExecutionState(ES_CONTINUOUS);
                    }
                })
                .map_err(|err| err.to_string())?;
            *guard = Some(tx);
            Ok(())
        } else {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
            Ok(())
        }
    }

    #[cfg(test)]
    pub fn enabled() -> bool {
        WAKE.lock().map(|guard| guard.is_some()).unwrap_or(false)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    pub fn set(_enabled: bool) -> Result<(), String> {
        Ok(())
    }

    #[cfg(test)]
    pub fn enabled() -> bool {
        false
    }
}

/// 开/关防休眠；重复调用是幂等的。
pub fn apply(enabled: bool) -> Result<(), String> {
    imp::set(enabled)
}

/// 当前是否真的持有电源断言（设置里存的是意愿，这里问的是实际状态）。
#[cfg(test)]
pub fn enabled() -> bool {
    imp::enabled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_is_idempotent() {
        // 不校验平台 API 的返回值，只确认重复开关不会报错、状态跟着走。
        apply(false).expect("off");
        assert!(!enabled());
        apply(false).expect("off again");
        apply(true).expect("on");
        assert!(enabled());
        apply(true).expect("on again");
        apply(false).expect("off");
        assert!(!enabled());
    }
}
