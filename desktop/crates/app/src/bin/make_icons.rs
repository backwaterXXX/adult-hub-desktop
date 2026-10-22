use image::GenericImageView;
use image::imageops::FilterType;
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source_path = "desktop/assets/branding/ic_launcher.png";
    println!("Loading source icon from: {}", source_path);

    let src_img = image::open(source_path)?;
    let (src_w, src_h) = src_img.dimensions();
    println!("Source icon dimensions: {}x{}", src_w, src_h);

    let assets_dir = Path::new("desktop/assets");

    let sizes = [16, 24, 32, 48, 64, 128, 256];
    let mut png_data_by_size = Vec::new();

    for &size in &sizes {
        let resized = src_img.resize_exact(size, size, FilterType::Lanczos3);
        let png_filename = format!("icon-{}.png", size);
        let png_path = assets_dir.join(&png_filename);
        resized.save(&png_path)?;
        println!("Saved {}", png_path.display());

        if size == 256 {
            let app_png_path = assets_dir.join("adult-hub-desktop.png");
            resized.save(&app_png_path)?;
            println!("Saved {}", app_png_path.display());
        }

        // Encode to PNG bytes in memory for ICO packaging
        let mut png_bytes = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut png_bytes);
        resized.write_to(&mut cursor, image::ImageFormat::Png)?;
        png_data_by_size.push((size, png_bytes));
    }

    // Pack into multi-resolution ICO file
    let ico_path = assets_dir.join("adult-hub-desktop.ico");
    let mut ico_file = File::create(&ico_path)?;

    let num_images = png_data_by_size.len() as u16;
    // Header: 6 bytes
    ico_file.write_all(&0u16.to_le_bytes())?; // Reserved
    ico_file.write_all(&1u16.to_le_bytes())?; // Type: 1 = ICO
    ico_file.write_all(&num_images.to_le_bytes())?; // Image count

    // Directory entries: 16 bytes each
    let header_size = 6 + (num_images as usize * 16);
    let mut current_offset = header_size as u32;

    for (size, data) in &png_data_by_size {
        let b_size = if *size >= 256 { 0u8 } else { *size as u8 };
        ico_file.write_all(&[b_size])?; // Width (0 means 256)
        ico_file.write_all(&[b_size])?; // Height (0 means 256)
        ico_file.write_all(&[0u8])?; // Color count
        ico_file.write_all(&[0u8])?; // Reserved
        ico_file.write_all(&1u16.to_le_bytes())?; // Color planes
        ico_file.write_all(&32u16.to_le_bytes())?; // Bits per pixel
        ico_file.write_all(&(data.len() as u32).to_le_bytes())?; // Image size
        ico_file.write_all(&current_offset.to_le_bytes())?; // Image offset

        current_offset += data.len() as u32;
    }

    // Write image data
    for (_size, data) in &png_data_by_size {
        ico_file.write_all(data)?;
    }

    println!(
        "Successfully generated multi-resolution ICO at {}",
        ico_path.display()
    );
    Ok(())
}
