#[cfg(all(target_os = "linux", alsa_silence))]
mod inner {
    extern "C" {
        fn lox_alsa_silence_init();
    }

    pub fn init() {
        unsafe {
            lox_alsa_silence_init();
        }
    }
}

#[cfg(any(not(target_os = "linux"), not(alsa_silence)))]
mod inner {
    pub fn init() {}
}

pub fn init() {
    inner::init();
}
