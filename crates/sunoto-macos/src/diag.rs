//! What else the Mac is doing during a dictation, as counts for the
//! accuracy record (docs/accuracy-reports-plan-2026-10-28.md): how many
//! windows are on screen, how busy the GPU is, memory pressure and thermal
//! state. Never window titles or owners. Each call takes a few milliseconds
//! at most; the daemon makes them off its event loop.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};

use crate::ffi;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iterator: *mut u32) -> c_int;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> c_int;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: ffi::CFStringRef,
        allocator: ffi::CFAllocatorRef,
        options: u32,
    ) -> ffi::CFTypeRef;
}

unsafe extern "C" {
    fn sysctlbyname(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> c_int;
}

/// Ordinary app windows on screen now (layer 0, every Space shown), counted
/// without reading their titles or owners.
pub fn on_screen_window_count() -> Option<u32> {
    // SAFETY: the copied array is released before returning; its
    // dictionaries are borrowed from it.
    unsafe {
        let array =
            ffi::CGWindowListCopyWindowInfo(ffi::kCGWindowListOptionOnScreenOnly, ffi::kCGNullWindowID);
        if array.is_null() {
            return None;
        }
        let count = ffi::CFArrayGetCount(array);
        let windows = (0..count)
            .map(|index| ffi::CFArrayGetValueAtIndex(array, index))
            .filter(|dict| {
                !dict.is_null() && crate::insertion::number_value(*dict, ffi::kCGWindowLayer) == Some(0)
            })
            .count();
        ffi::CFRelease(array);
        Some(windows as u32)
    }
}

/// The GPU's "Device Utilization %" from IOKit's accelerator statistics,
/// the figure Activity Monitor's GPU History draws. The busiest GPU wins on
/// a Mac with more than one.
pub fn gpu_utilization_percent() -> Option<u32> {
    let class = CString::new("IOAccelerator").ok()?;
    let key = cf_string("PerformanceStatistics")?;
    let field = cf_string("Device Utilization %");
    let mut busiest = None;
    // SAFETY: IOServiceGetMatchingServices consumes `matching`; every
    // registry entry, the iterator and every copied property are released.
    unsafe {
        let matching = IOServiceMatching(class.as_ptr());
        let mut iterator = 0;
        if !matching.is_null() && IOServiceGetMatchingServices(0, matching, &mut iterator) == 0 {
            loop {
                let entry = IOIteratorNext(iterator);
                if entry == 0 {
                    break;
                }
                let stats = IORegistryEntryCreateCFProperty(entry, key, ffi::NULL_ALLOCATOR, 0);
                if !stats.is_null() {
                    if let Some(field) = field {
                        let value = number_of(stats, field);
                        busiest = busiest.max(value);
                    }
                    ffi::CFRelease(stats);
                }
                IOObjectRelease(entry);
            }
            IOObjectRelease(iterator);
        }
        ffi::CFRelease(key);
        if let Some(field) = field {
            ffi::CFRelease(field);
        }
    }
    busiest.map(|percent| percent.clamp(0, 100) as u32)
}

/// The kernel's memory pressure: 1 normal, 2 warning, 4 critical.
pub fn memory_pressure_level() -> Option<u32> {
    let name = CString::new("kern.memorystatus_vm_pressure_level").ok()?;
    let mut level: c_int = 0;
    let mut size = std::mem::size_of::<c_int>();
    // SAFETY: writes at most `size` bytes into `level`.
    let status = unsafe {
        sysctlbyname(
            name.as_ptr(),
            &mut level as *mut c_int as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0).then_some(level as u32)
}

/// The Mac's model identifier ("Mac14,9"), not its serial or name.
pub fn mac_model() -> Option<String> {
    sysctl_string("hw.model")
}

/// "26.1"
pub fn macos_version() -> Option<String> {
    sysctl_string("kern.osproductversion")
}

fn sysctl_string(name: &str) -> Option<String> {
    let name = CString::new(name).ok()?;
    let mut buffer = [0u8; 128];
    let mut size = buffer.len();
    // SAFETY: writes at most `size` bytes into `buffer`.
    let status = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    let text = std::ffi::CStr::from_bytes_until_nul(&buffer[..size.min(buffer.len())]).ok()?;
    Some(text.to_str().ok()?.to_string()).filter(|text| !text.is_empty())
}

/// SAFETY: `dict` must be a live CFDictionary and `key` a live CFString.
unsafe fn number_of(dict: ffi::CFTypeRef, key: ffi::CFStringRef) -> Option<i64> {
    let value = unsafe { ffi::CFDictionaryGetValue(dict, key) };
    if value.is_null() {
        return None;
    }
    let mut out: i64 = 0;
    let ok = unsafe {
        ffi::CFNumberGetValue(
            value as ffi::CFNumberRef,
            ffi::kCFNumberSInt64Type,
            &mut out as *mut i64 as *mut c_void,
        )
    };
    (ok == 1).then_some(out)
}

fn cf_string(text: &str) -> Option<ffi::CFStringRef> {
    // SAFETY: the bytes are valid for the call; the caller releases it.
    let string = unsafe {
        ffi::CFStringCreateWithBytes(
            ffi::NULL_ALLOCATOR,
            text.as_ptr(),
            text.len() as ffi::CFIndex,
            ffi::kCFStringEncodingUTF8,
            0,
        )
    };
    (!string.is_null()).then_some(string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_answer_on_this_mac() {
        // Window count and GPU need a login session; CI may have neither.
        let level = memory_pressure_level().expect("sysctl answers");
        assert!([1, 2, 4].contains(&level), "pressure level {level}");
        if let Some(percent) = gpu_utilization_percent() {
            assert!(percent <= 100);
        }
        let _ = on_screen_window_count();
        assert!(crate::thermal_state().is_some_and(|state| state <= 3));
        assert!(mac_model().is_some_and(|model| model.starts_with("Mac")));
        assert!(macos_version().is_some_and(|version| version.contains('.') || version.parse::<u32>().is_ok()));
    }
}
