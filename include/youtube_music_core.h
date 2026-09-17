#ifndef YOUTUBE_MUSIC_CORE_H
#define YOUTUBE_MUSIC_CORE_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* All calls are blocking: use a worker thread, never the UI thread.
 * Inputs are NUL-terminated UTF-8 JSON, valid until the call returns.
 * Outputs: {"ok":true,"data":...} or {"ok":false,"error":...}.
 * Free EVERY returned string exactly once with ytmusic_string_free.
 * Never use the host allocator. NULL input returns a JSON error. */

/* One-shot call: {"config":{},"request":{"op":"search","query":"Daft Punk"}}. */
char *ytmusic_core_call(const char *input);

/* Persistent client: config is a Config JSON object, e.g. {}.
 * Returns data:{"handle":N}. Treat the uint64_t as an opaque ID, not a pointer.
 * Handles share HTTP connections and the evolving Cookie session. */
char *ytmusic_client_create(const char *config);

/* request is a Request object, e.g. {"op":"auth_refresh"}.
 * Thread-safe. Callers may serialize operations to preserve their desired order. */
char *ytmusic_client_call(uint64_t handle, const char *request);

/* SECRET OUTPUT: data is a verified BrowserSession object, or null when anonymous.
 * May perform network I/O to verify the session. Store only with host-provided
 * secure storage. Never log this result or expose it to the UI.
 * Normal call outputs do not contain Cookie snapshots. */
char *ytmusic_client_export_session(uint64_t handle);

/* Removes this handle; in-flight operations complete safely with retained ownership.
 * Does not cancel requests. Future calls and repeated destroy return invalid_input. */
char *ytmusic_client_destroy(uint64_t handle);

/* Clears and releases a JSON result, including secret exports. NULL is accepted. */
void ytmusic_string_free(char *output);
#ifdef __cplusplus
}
#endif
#endif
