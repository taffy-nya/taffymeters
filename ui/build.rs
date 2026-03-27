fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/taffy.ico");
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/taffy.ico");
        res.compile().unwrap();
    }
}
