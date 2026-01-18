#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
mod inner {
    pub fn init() {}
}

pub fn init() {
    inner::init();
}
