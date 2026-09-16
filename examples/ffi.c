#include "youtube_music_core.h"
#include <stdio.h>

int main(void) {
    char *result = ytmusic_core_call("{\"request\":{\"op\":\"search\",\"query\":\"Daft Punk\",\"filter\":\"songs\"}}");
    if (!result) return 1;
    puts(result);
    ytmusic_string_free(result);
    return 0;
}
