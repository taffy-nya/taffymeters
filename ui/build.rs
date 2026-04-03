fn main() {
    #[cfg(target_os = "windows")]
    {
        println!("cargo:rerun-if-changed=assets/taffy.ico");
        println!("cargo:rerun-if-changed=./app.rc");
        embed_resource::compile("./app.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}
