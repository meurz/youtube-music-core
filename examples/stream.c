#include "youtube_music_core.h"
#include <stdio.h>

int main(void) {
    char *result = ytmusic_core_call("{\"request\":{\"op\":\"stream\",\"video_id\":\"4D7u5KF7SP8\",\"format\":\"mp4\"}}");
    if (!result) return 1;
    puts(result);
    ytmusic_string_free(result);
    return 0;
}
