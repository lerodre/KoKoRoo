fn main() {
    // Embed application icon in the Windows executable.
    // This makes the .exe show the logo in Explorer, taskbar, Alt-Tab, etc.
    #[cfg(target_os = "windows")]
    {
        use image::imageops::FilterType;
        use std::io::Cursor;

        let png_data = std::fs::read("assets/logo.png").expect("assets/logo.png not found");
        let img = image::load_from_memory(&png_data).expect("failed to decode logo.png");

        // Generate multiple sizes so Windows picks the right one for each context
        // (list view 16px, small icons 32px, medium 48px, large 64/128px, jumbo 256px)
        let sizes: &[u32] = &[16, 24, 32, 48, 64, 128, 256];
        let mut png_entries: Vec<(u32, Vec<u8>)> = Vec::new();

        for &size in sizes {
            let resized = img.resize_exact(size, size, FilterType::Lanczos3);
            let mut buf = Vec::new();
            resized
                .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
                .expect("failed to encode resized PNG");
            png_entries.push((size, buf));
        }

        // Build ICO file manually with multiple entries
        let num_images = png_entries.len() as u16;
        let mut ico = Vec::new();

        // ICO header: reserved(2) + type=1(2) + count(2)
        ico.extend_from_slice(&[0, 0, 1, 0]);
        ico.extend_from_slice(&num_images.to_le_bytes());

        // Calculate data offset: header(6) + entries(16 each)
        let dir_size = 6 + 16 * png_entries.len();
        let mut data_offset = dir_size;

        // Write directory entries
        for (size, data) in &png_entries {
            let w = if *size >= 256 { 0u8 } else { *size as u8 };
            let h = w;
            ico.push(w);                                           // width
            ico.push(h);                                           // height
            ico.push(0);                                           // color palette
            ico.push(0);                                           // reserved
            ico.extend_from_slice(&1u16.to_le_bytes());            // color planes
            ico.extend_from_slice(&32u16.to_le_bytes());           // bits per pixel
            ico.extend_from_slice(&(data.len() as u32).to_le_bytes()); // data size
            ico.extend_from_slice(&(data_offset as u32).to_le_bytes()); // data offset
            data_offset += data.len();
        }

        // Write image data
        for (_, data) in &png_entries {
            ico.extend_from_slice(data);
        }

        let ico_path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("logo.ico");
        std::fs::write(&ico_path, &ico).expect("failed to write .ico");

        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.compile().expect("failed to compile Windows resources");
    }
}
