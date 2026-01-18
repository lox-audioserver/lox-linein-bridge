fn main() {
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rerun-if-changed=src/alsa_silence.c");
        cc::Build::new()
            .file("src/alsa_silence.c")
            .compile("alsa_silence");
    }
}
