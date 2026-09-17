use std::{
    collections::HashMap,
    ffi::{c_char, CStr, CString},
    sync::{Arc, Mutex, OnceLock},
};

use serde_json::{json, Value};
use zeroize::Zeroize;

use crate::{Config, Error, MusicClient, Request, Result};

#[derive(Default)]
struct Clients {
    last_handle: u64,
    clients: HashMap<u64, Arc<MusicClient>>,
}

fn clients() -> &'static Mutex<Clients> {
    static CLIENTS: OnceLock<Mutex<Clients>> = OnceLock::new();
    CLIENTS.get_or_init(Mutex::default)
}

fn client(handle: u64) -> Result<Arc<MusicClient>> {
    clients()
        .lock()
        .map_err(|_| Error::Protocol("client registry unavailable".into()))?
        .clients
        .get(&handle)
        .cloned()
        .ok_or_else(|| Error::InvalidInput("invalid or destroyed client handle".into()))
}

fn output(call: impl FnOnce() -> Result<Value>) -> *mut c_char {
    raw_output(|| crate::envelope(call()).to_string())
}

fn raw_output(call: impl FnOnce() -> String) -> *mut c_char {
    let output =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)).unwrap_or_else(|_| {
            "{\"ok\":false,\"error\":{\"code\":\"panic\",\"message\":\"internal panic\"}}".into()
        });
    // JSON serialization escapes embedded NUL bytes.
    CString::new(output)
        .expect("JSON contains no literal NUL")
        .into_raw()
}

/// # Safety
/// input must be null or point to a valid NUL-terminated string for this call.
unsafe fn input<'a>(input: *const c_char) -> Result<&'a str> {
    if input.is_null() {
        return Err(Error::InvalidInput("null input".into()));
    }
    // SAFETY: upheld by caller; UTF-8 is checked before parsing JSON.
    unsafe { CStr::from_ptr(input) }
        .to_str()
        .map_err(|_| Error::InvalidInput("input must be UTF-8".into()))
}

/// Execute a one-shot UTF-8 JSON request. The returned allocation belongs to this library.
///
/// # Safety
/// input must be null or point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_core_call(request: *const c_char) -> *mut c_char {
    raw_output(|| {
        // SAFETY: caller guarantees the input allocation remains valid.
        match unsafe { input(request) } {
            Ok(request) => crate::core_call(request),
            Err(error) => crate::envelope(Err(error)).to_string(),
        }
    })
}

/// Create a persistent client from a Config JSON object (for example, {}).
/// Success returns {"ok":true,"data":{"handle":N}}. Handles are never reused.
/// All calls are blocking; use a worker thread in UI hosts.
///
/// # Safety
/// config must be null or point to a valid NUL-terminated UTF-8 string for this call.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_client_create(config: *const c_char) -> *mut c_char {
    output(|| {
        // SAFETY: caller guarantees the input allocation remains valid.
        let config: Config = serde_json::from_str(unsafe { input(config)? }).map_err(|error| {
            Error::InvalidInput(
                if error.to_string().contains(crate::auth::LEGACY_AUTH_MESSAGE) {
                    crate::auth::LEGACY_AUTH_MESSAGE.into()
                } else {
                    "expected a client configuration JSON object".into()
                },
            )
        })?;
        let client = Arc::new(MusicClient::new(config)?);
        let mut registry = clients()
            .lock()
            .map_err(|_| Error::Protocol("client registry unavailable".into()))?;
        let handle = registry
            .last_handle
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("client handles exhausted".into()))?;
        registry.last_handle = handle;
        registry.clients.insert(handle, client);
        Ok(json!({"handle": handle}))
    })
}

/// Execute a Request JSON object (for example, {"op":"auth_status"}).
/// Concurrent calls share this client's session and connections.
///
/// # Safety
/// request must be null or point to a valid NUL-terminated UTF-8 string for this call.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_client_call(handle: u64, request: *const c_char) -> *mut c_char {
    output(|| {
        let client = client(handle)?;
        // SAFETY: caller guarantees the input allocation remains valid.
        let request: Request = serde_json::from_str(unsafe { input(request)? })
            .map_err(|_| Error::InvalidInput("expected a request JSON object with op".into()))?;
        client.execute(request)
    })
}

/// Export the verified session snapshot as data (or null for an anonymous client).
/// SECRET OUTPUT: serialize only to a secure credential store, never to UI or logs.
/// This may block to verify the current session before exporting it.
#[no_mangle]
pub extern "C" fn ytmusic_client_export_session(handle: u64) -> *mut c_char {
    output(|| {
        serde_json::to_value(client(handle)?.browser_session()?)
            .map_err(|_| Error::Protocol("could not serialize session".into()))
    })
}

/// Remove a handle. In-flight calls retain ownership and finish safely.
/// Further calls, including a second destroy, return invalid_input.
#[no_mangle]
pub extern "C" fn ytmusic_client_destroy(handle: u64) -> *mut c_char {
    output(|| {
        let removed = clients()
            .lock()
            .map_err(|_| Error::Protocol("client registry unavailable".into()))?
            .clients
            .remove(&handle)
            .ok_or_else(|| Error::InvalidInput("invalid or destroyed client handle".into()))?;
        // Drop outside the registry lock: an HTTP client may wait for its worker.
        drop(removed);
        Ok(json!({"destroyed": true}))
    })
}

/// Release a returned JSON string; null is accepted. Clears the allocation first.
///
/// # Safety
/// output must be null or an unfreed pointer returned by any ytmusic_* JSON function.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_string_free(output: *mut c_char) {
    if !output.is_null() {
        // SAFETY: caller transfers the allocation back exactly once.
        let mut bytes = unsafe { CString::from_raw(output) }.into_bytes_with_nul();
        bytes.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(output: *mut c_char) -> Value {
        // SAFETY: tests pass a live library result, then free it exactly once.
        unsafe {
            let value = serde_json::from_str(CStr::from_ptr(output).to_str().unwrap()).unwrap();
            ytmusic_string_free(output);
            value
        }
    }

    fn create() -> u64 {
        let config = CString::new(r#"{"client_version":"test.fixture"}"#).unwrap();
        // SAFETY: config remains live through the call. Explicit version avoids network I/O.
        let value = read(unsafe { ytmusic_client_create(config.as_ptr()) });
        assert_eq!(value["ok"], true);
        value["data"]["handle"].as_u64().unwrap()
    }

    #[test]
    fn ffi_rejects_null_invalid_utf8_and_malformed_json() {
        let malformed = CString::new("bad json").unwrap();
        let invalid_utf8 = [255u8, 0];
        let handle = create();
        for input in [
            std::ptr::null(),
            malformed.as_ptr(),
            invalid_utf8.as_ptr().cast(),
        ] {
            // SAFETY: each input is null or points to a live NUL-terminated allocation.
            for value in unsafe {
                [
                    read(ytmusic_core_call(input)),
                    read(ytmusic_client_create(input)),
                    read(ytmusic_client_call(handle, input)),
                ]
            } {
                assert_eq!(value["error"]["code"], "invalid_input");
            }
        }
        assert_eq!(read(ytmusic_client_destroy(handle))["ok"], true);
        // SAFETY: null is explicitly accepted.
        unsafe { ytmusic_string_free(std::ptr::null_mut()) };
    }

    #[test]
    fn persistent_lifecycle_and_stale_handles_are_safe() {
        let handle = create();
        let status = CString::new(r#"{"op":"auth_status"}"#).unwrap();
        // SAFETY: request remains valid throughout the call; anonymous status is local.
        let value = read(unsafe { ytmusic_client_call(handle, status.as_ptr()) });
        assert_eq!(value["ok"], true);
        assert_eq!(value["data"]["state"], "signed_out");
        assert_eq!(
            read(ytmusic_client_export_session(handle))["data"],
            Value::Null
        );
        let invalid_request = CString::new(r#"{"op":"search","query":""}"#).unwrap();
        // SAFETY: request remains live throughout the call. Invalid query avoids network I/O.
        assert_eq!(
            read(unsafe { ytmusic_client_call(handle, invalid_request.as_ptr()) })["error"]["code"],
            "invalid_input"
        );
        assert_eq!(read(ytmusic_client_destroy(handle))["ok"], true);
        for stale in [0, handle, u64::MAX] {
            assert_eq!(
                read(ytmusic_client_destroy(stale))["error"]["code"],
                "invalid_input"
            );
            assert_eq!(
                read(ytmusic_client_export_session(stale))["error"]["code"],
                "invalid_input"
            );
            // SAFETY: the input remains live; stale handles are rejected before execution.
            assert_eq!(
                read(unsafe { ytmusic_client_call(stale, invalid_request.as_ptr()) })["error"]
                    ["code"],
                "invalid_input"
            );
        }
        let replacement = create();
        assert_ne!(replacement, handle);
        assert_eq!(read(ytmusic_client_destroy(replacement))["ok"], true);
    }

    #[test]
    fn one_shot_envelope_remains_compatible() {
        let request = CString::new(
            r#"{"config":{"client_version":"test.fixture"},"request":{"op":"auth_status"}}"#,
        )
        .unwrap();
        // SAFETY: input is a live NUL-terminated allocation; version avoids bootstrap.
        let value = read(unsafe { ytmusic_core_call(request.as_ptr()) });
        assert_eq!(value["ok"], true);
        assert_eq!(value["data"]["state"], "signed_out");
        assert!(value["data"].get("ok").is_none());
    }

    #[test]
    fn destruction_does_not_invalidate_an_in_flight_owner() {
        let handle = create();
        let acquired = client(handle).unwrap();
        let worker = std::thread::spawn(move || {
            assert_eq!(read(ytmusic_client_destroy(handle))["ok"], true);
            assert!(client(handle).is_err());
        });
        worker.join().unwrap();
        assert!(acquired.browser_session().unwrap().is_none());
    }
}
