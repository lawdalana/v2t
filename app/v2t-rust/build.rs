fn main() {
    #[cfg(windows)]
    {
        let icon = "../V2t.Windows/Assets/v2t.ico";
        println!("cargo:rerun-if-changed={icon}");
        winresource::WindowsResource::new()
            .set_icon(icon)
            .set("ProductName", "v2t")
            .set("OriginalFilename", "V2t.exe")
            .compile()
            .expect("could not compile the Windows application resources");
    }
}
