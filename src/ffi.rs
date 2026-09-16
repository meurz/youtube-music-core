use std::ffi::{c_char, CStr, CString};

/// Execute a UTF-8 JSON request. The returned allocation belongs to this library.
///
/// # Safety
/// input must be null or point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_core_call(input: *const c_char) -> *mut c_char {
    let output = std::panic::catch_unwind(|| {
        if input.is_null() {
            return crate::envelope(Err(crate::Error::InvalidInput("null input".into())))
                .to_string();
        }
        // SAFETY: upheld by caller; UTF-8 is checked before parsing JSON.
        match unsafe { CStr::from_ptr(input) }.to_str() {
            Ok(input) => crate::core_call(input),
            Err(_) => crate::envelope(Err(crate::Error::InvalidInput(
                "input must be UTF-8".into(),
            )))
            .to_string(),
        }
    })
    .unwrap_or_else(|_| {
        "{\"ok\":false,\"error\":{\"code\":\"panic\",\"message\":\"internal panic\"}}".into()
    });
    // JSON serialization escapes embedded NUL bytes.
    CString::new(output)
        .expect("JSON contains no literal NUL")
        .into_raw()
}

/// Release a returned JSON string; null is accepted.
///
/// # Safety
/// output must be null or an unfreed pointer returned by ytmusic_core_call.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_string_free(output: *mut c_char) {
    if !output.is_null() {
        // SAFETY: caller transfers the allocation back exactly once.
        drop(unsafe { CString::from_raw(output) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_rejects_null_invalid_utf8_and_malformed_json() {
        let malformed = CString::new("bad json").unwrap();
        let invalid_utf8 = [255u8, 0];
        for input in [
            std::ptr::null(),
            malformed.as_ptr(),
            invalid_utf8.as_ptr().cast(),
        ] {
            // SAFETY: each input is null or points to a live NUL-terminated allocation.
            unsafe {
                let output = ytmusic_core_call(input);
                let v: serde_json::Value =
                    serde_json::from_str(CStr::from_ptr(output).to_str().unwrap()).unwrap();
                assert_eq!(v["error"]["code"], "invalid_input");
                ytmusic_string_free(output);
            }
        }
        // SAFETY: null is explicitly accepted.
        unsafe { ytmusic_string_free(std::ptr::null_mut()) };
    }
}
