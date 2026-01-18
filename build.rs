fn main() {
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rerun-if-changed=src/alsa_silence.c");
        let alsa = pkg_config::Config::new()
            .probe("alsa")
            .expect("alsa development files not found (pkg-config)");
        let mut build = cc::Build::new();
        build.file("src/alsa_silence.c");
        for path in alsa.include_paths {
            build.include(path);
        }
        build.compile("alsa_silence");
    }
}
