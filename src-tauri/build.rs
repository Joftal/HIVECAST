fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // The linker embeds one manifest for every final target, including libtest.
        let attrs = tauri_build::WindowsAttributes::new_without_app_manifest();
        tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(attrs))
            .expect("build Windows resources");
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows.manifest");
        println!("cargo:rerun-if-changed=windows.manifest");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    } else {
        tauri_build::build();
    }
}
