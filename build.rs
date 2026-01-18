fn main() {
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rerun-if-changed=src/alsa_silence.c");
        let alsa = match pkg_config::Config::new().probe("alsa") {
            Ok(alsa) => alsa,
            Err(err) => {
                println!("cargo:warning=alsa pkg-config not found: {err}");
                return;
            }
        };
        let header_found = alsa.include_paths.iter().any(|path| {
            let candidate = path.join("alsa").join("asoundlib.h");
            candidate.exists()
        });
        if !header_found {
            println!("cargo:warning=alsa headers not found in include paths");
            return;
        }
        let mut build = cc::Build::new();
        build.file("src/alsa_silence.c");
        for path in alsa.include_paths {
            build.include(path);
        }
        build.compile("alsa_silence");
        println!("cargo:rustc-cfg=alsa_silence");
    }
}
