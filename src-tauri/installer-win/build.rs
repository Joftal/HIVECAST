fn main() {
    #[cfg(windows)]
    {
        let mut res = tauri_winres::WindowsResource::new();
        res.set_icon("../icons/icon.ico");
        res.set("ProductName", "HIVECAST");
        res.set("FileDescription", "HIVECAST Setup");
        res.set("CompanyName", "HIVECAST");
        if let Err(err) = res.compile() {
            panic!("failed to embed Windows installer resources: {err}");
        }
    }
}
