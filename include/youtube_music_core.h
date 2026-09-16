#ifndef YOUTUBE_MUSIC_CORE_H
#define YOUTUBE_MUSIC_CORE_H
#ifdef __cplusplus
extern "C" {
#endif
/* Blocking. Input is UTF-8 JSON: {"config":{},"request":{"op":"search","query":"Daft Punk"}}.
 * Output is UTF-8 JSON: {"ok":true,"data":...} or {"ok":false,"error":...}.
 * Keep input valid during the call. Free output exactly once with ytmusic_string_free.
 * Never use the host allocator to release output. NULL input returns a JSON error. */
char *ytmusic_core_call(const char *input);
void ytmusic_string_free(char *output);
#ifdef __cplusplus
}
#endif
#endif
