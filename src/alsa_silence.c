#include <alsa/asoundlib.h>

static void error_handler(const char *file, int line, const char *function, int err, const char *fmt, ...) {
    (void)file;
    (void)line;
    (void)function;
    (void)err;
    (void)fmt;
}

void lox_alsa_silence_init(void) {
    snd_lib_error_set_handler(error_handler);
}
