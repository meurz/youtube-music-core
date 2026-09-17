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

fn register_client(value: MusicClient) -> Result<Value> {
    let mut registry = clients()
        .lock()
        .map_err(|_| Error::Protocol("client registry unavailable".into()))?;
    if registry.clients.len() >= 128 {
        return Err(Error::InvalidInput(
            "too many live clients; destroy unused handles".into(),
        ));
    }
    let handle = registry
        .last_handle
        .checked_add(1)
        .ok_or_else(|| Error::Protocol("client handles exhausted".into()))?;
    registry.last_handle = handle;
    registry.clients.insert(handle, Arc::new(value));
    Ok(json!({"handle": handle}))
}

struct Operation {
    context: crate::operation::OperationContext,
    state: std::sync::atomic::AtomicU8,
}
#[derive(Default)]
struct Operations {
    last: u64,
    values: HashMap<u64, Arc<Operation>>,
}
fn operations() -> &'static Mutex<Operations> {
    static OPERATIONS: OnceLock<Mutex<Operations>> = OnceLock::new();
    OPERATIONS.get_or_init(Mutex::default)
}
fn operation(id: u64) -> Result<Arc<Operation>> {
    operations()
        .lock()
        .map_err(|_| Error::Protocol("operation registry unavailable".into()))?
        .values
        .get(&id)
        .cloned()
        .ok_or_else(|| Error::InvalidInput("invalid or destroyed operation handle".into()))
}
fn controlled<T>(id: u64, call: impl FnOnce() -> Result<T>) -> Result<T> {
    use std::sync::atomic::Ordering;
    let op = operation(id)?;
    op.state
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| Error::InvalidInput("operation handles are single-use".into()))?;
    struct Finish(Arc<Operation>);
    impl Drop for Finish {
        fn drop(&mut self) {
            self.0.context.set_phase("finished");
            self.0.state.store(2, Ordering::Release);
        }
    }
    let _finish = Finish(op.clone());
    op.context.run(call)
}

/// Allocate a single-use operation before starting a blocking call. Options: {"timeout_ms":120000}.
/// # Safety
/// options must point to a valid NUL-terminated UTF-8 string for this call, or be null.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_operation_create(options: *const c_char) -> *mut c_char {
    output(|| {
        // SAFETY: input validity is the caller's contract.
        let options = serde_json::from_str(unsafe { input(options)? })
            .map_err(|_| Error::InvalidInput("expected operation options JSON".into()))?;
        let context = crate::operation::OperationContext::new(options)?;
        let mut registry = operations()
            .lock()
            .map_err(|_| Error::Protocol("operation registry unavailable".into()))?;
        if registry.values.len() >= 256 {
            return Err(Error::InvalidInput(
                "too many live operations; destroy unused handles".into(),
            ));
        }
        let handle = registry
            .last
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("operation handles exhausted".into()))?;
        registry.last = handle;
        registry.values.insert(
            handle,
            Arc::new(Operation {
                context,
                state: std::sync::atomic::AtomicU8::new(0),
            }),
        );
        Ok(json!({"handle":handle}))
    })
}
/// Signal cancellation. Running HTTP futures and JS execution observe this signal.
#[no_mangle]
pub extern "C" fn ytmusic_operation_cancel(handle: u64) -> *mut c_char {
    output(|| {
        operation(handle)?.context.cancel();
        Ok(json!({"cancelled":true}))
    })
}
/// Read non-secret progress without blocking the client operation.
#[no_mangle]
pub extern "C" fn ytmusic_operation_status(handle: u64) -> *mut c_char {
    output(|| {
        let op = operation(handle)?;
        let mut progress = serde_json::to_value(op.context.progress())
            .map_err(|_| Error::Protocol("progress serialization failed".into()))?;
        progress["state"] = match op.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => "queued",
            1 => "running",
            _ => "finished",
        }
        .into();
        Ok(progress)
    })
}
/// Remove the operation handle and cancel any call that still owns it.
#[no_mangle]
pub extern "C" fn ytmusic_operation_destroy(handle: u64) -> *mut c_char {
    output(|| {
        let removed = operations()
            .lock()
            .map_err(|_| Error::Protocol("operation registry unavailable".into()))?
            .values
            .remove(&handle)
            .ok_or_else(|| Error::InvalidInput("invalid or destroyed operation handle".into()))?;
        removed.context.cancel();
        Ok(json!({"destroyed":true}))
    })
}

/// Create a client with cancellable bootstrap and a whole-operation deadline.
/// # Safety
/// config must point to a valid NUL-terminated UTF-8 string for this call, or be null.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_client_create_with_operation(
    config: *const c_char,
    operation_id: u64,
) -> *mut c_char {
    output(|| {
        controlled(operation_id, || {
            // SAFETY: input validity is the caller's contract.
            let config: Config = serde_json::from_str(unsafe { input(config)? })
                .map_err(|_| Error::InvalidInput("expected client configuration JSON".into()))?;
            register_client(MusicClient::new(config)?)
        })
    })
}
/// Execute with a previously allocated, single-use operation handle.
/// # Safety
/// request must point to a valid NUL-terminated UTF-8 string for this call, or be null.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_client_call_with_operation(
    handle: u64,
    request: *const c_char,
    operation_id: u64,
) -> *mut c_char {
    output(|| {
        controlled(operation_id, || {
            // SAFETY: input validity is the caller's contract.
            let request: Request = serde_json::from_str(unsafe { input(request)? })
                .map_err(|_| Error::InvalidInput("expected request JSON with op".into()))?;
            client(handle)?.execute(request)
        })
    })
}
/// Explicit secret export with cancellable account verification.
#[no_mangle]
pub extern "C" fn ytmusic_client_export_session_with_operation(
    handle: u64,
    operation_id: u64,
) -> *mut c_char {
    output(|| {
        controlled(operation_id, || {
            serde_json::to_value(client(handle)?.browser_session()?)
                .map_err(|_| Error::Protocol("session serialization failed".into()))
        })
    })
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
        register_client(MusicClient::new(config)?)
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

/// Create a new verified account client. The original handle remains unchanged.
/// # Safety
/// selector must point to a valid NUL-terminated UTF-8 JSON object or be null.
#[no_mangle]
pub unsafe extern "C" fn ytmusic_client_select_account(
    handle: u64,
    selector: *const c_char,
    operation_id: u64,
) -> *mut c_char {
    output(|| {
        controlled(operation_id, || {
            // SAFETY: input validity is the caller's contract.
            let selector: crate::discovery::AccountSelector =
                serde_json::from_str(unsafe { input(selector)? })
                    .map_err(|_| Error::InvalidInput("expected account selector JSON".into()))?;
            register_client(client(handle)?.select_account(&selector)?)
        })
    })
}
/// Local protocol/capability discovery; does not create a client or use the network.
#[no_mangle]
pub extern "C" fn ytmusic_capabilities() -> *mut c_char {
    output(|| Ok(crate::capabilities()))
}
/// C ABI revision. Existing v1 entry points remain available.
#[no_mangle]
pub extern "C" fn ytmusic_abi_version() -> u32 {
    2
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
    fn operation_handles_are_single_use_and_cancellation_prevents_execution() {
        let options = CString::new("{}").unwrap();
        let request = CString::new(r#"{"op":"auth_status"}"#).unwrap();
        let handle = create();
        // SAFETY: strings remain alive for each call.
        let op = read(unsafe { ytmusic_operation_create(options.as_ptr()) })["data"]["handle"]
            .as_u64()
            .unwrap();
        assert_eq!(read(ytmusic_operation_cancel(op))["ok"], true);
        // SAFETY: request is a valid NUL-terminated string.
        assert_eq!(
            read(unsafe { ytmusic_client_call_with_operation(handle, request.as_ptr(), op) })
                ["error"]["code"],
            "cancelled"
        );
        assert_eq!(
            read(ytmusic_operation_status(op))["data"]["state"],
            "finished"
        );
        // SAFETY: request remains live; repeated operations must be rejected.
        assert_eq!(
            read(unsafe { ytmusic_client_call_with_operation(handle, request.as_ptr(), op) })
                ["error"]["code"],
            "invalid_input"
        );
        assert_eq!(read(ytmusic_operation_destroy(op))["ok"], true);
        assert_eq!(
            read(ytmusic_operation_cancel(op))["error"]["code"],
            "invalid_input"
        );
        assert_eq!(read(ytmusic_client_destroy(handle))["ok"], true);
        assert_eq!(ytmusic_abi_version(), 2);
        assert_eq!(
            read(ytmusic_capabilities())["data"]["protocol_version"],
            "1.2"
        );
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
