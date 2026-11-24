fn main() {
    tauri_build::build();

    // tauri-build 只为应用主程序嵌入 manifest；测试可执行文件没有它时，
    // Windows 会以 0xc0000139（Common Controls v6 入口找不到）直接退出。
    #[cfg(windows)]
    {
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-test.manifest");
        println!("cargo:rerun-if-changed=windows-test.manifest");
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
