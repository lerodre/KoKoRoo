fn main() {
    // Embed application icon in the Windows executable.
    // This makes the .exe show the logo in Explorer, taskbar, Alt-Tab, etc.
    #[cfg(target_os = "windows")]
    {
        // Create a .ico file wrapping the PNG (ICO format supports PNG-compressed entries).
        let png_data = std::fs::read("assets/logo.png").expect("assets/logo.png not found");
        let ico_path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("logo.ico");

        let mut ico = Vec::new();
        // ICO header: reserved(2) + type=1(2) + count=1(2)
        ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
        // Directory entry (16 bytes):
        ico.push(0); // width: 0 means 256+ (PNG stores actual dimensions)
        ico.push(0); // height: 0 means 256+
        ico.push(0); // color palette count
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // color planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(png_data.len() as u32).to_le_bytes()); // image data size
        ico.extend_from_slice(&22u32.to_le_bytes()); // offset: 6 header + 16 entry = 22
        // Image data: raw PNG
        ico.extend_from_slice(&png_data);

        std::fs::write(&ico_path, &ico).expect("failed to write .ico");

        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.compile().expect("failed to compile Windows resources");
    }
}
