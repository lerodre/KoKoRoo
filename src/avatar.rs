use image::{GenericImageView, ImageFormat, RgbaImage};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

/// Avatar size in pixels (both width and height).
pub const AVATAR_SIZE: u32 = 128;

/// Maximum allowed avatar file size (64 KB).
pub const MAX_AVATAR_BYTES: usize = 65536;

/// PNG magic bytes.
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

fn data_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kokoroo")
}

/// Path to our own avatar.
pub fn own_avatar_path() -> PathBuf {
    data_dir().join("avatar.png")
}

/// Path to a contact's received avatar.
pub fn contact_avatar_path(contact_id: &str) -> PathBuf {
    data_dir().join("avatars").join(format!("{contact_id}.png"))
}

/// Load image from bytes (PNG or JPEG), apply a 1:1 crop, resize to 128×128, encode as PNG.
///
/// `crop_x`, `crop_y`: top-left of the crop square in original image pixels.
/// `crop_size`: side length of the crop square in original image pixels.
pub fn process_avatar(
    image_bytes: &[u8],
    crop_x: u32,
    crop_y: u32,
    crop_size: u32,
) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| format!("Failed to decode image: {e}"))?;
    let (w, h) = img.dimensions();

    // Clamp crop to image bounds
    let cx = crop_x.min(w.saturating_sub(1));
    let cy = crop_y.min(h.saturating_sub(1));
    let cs = crop_size.min(w - cx).min(h - cy).max(1);

    let cropped = img.crop_imm(cx, cy, cs, cs);
    let resized = image::imageops::resize(
        &cropped.to_rgba8(),
        AVATAR_SIZE,
        AVATAR_SIZE,
        image::imageops::FilterType::Lanczos3,
    );

    let mut buf = Cursor::new(Vec::new());
    resized
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode PNG: {e}"))?;
    Ok(buf.into_inner())
}

/// Validate received avatar bytes: PNG format, decodable, exactly 128×128, size capped.
pub fn validate_received_avatar(data: &[u8]) -> bool {
    if data.len() > MAX_AVATAR_BYTES || data.len() < 8 {
        return false;
    }
    if data[..8] != PNG_MAGIC {
        return false;
    }
    match image::load_from_memory(data) {
        Ok(img) => {
            let (w, h) = img.dimensions();
            w == AVATAR_SIZE && h == AVATAR_SIZE
        }
        Err(_) => false,
    }
}

/// Save our own avatar PNG to ~/.kokoroo/avatar.png.
pub fn save_own_avatar(data: &[u8]) -> Result<(), String> {
    let dir = data_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    fs::write(own_avatar_path(), data).map_err(|e| format!("write: {e}"))
}

/// Save a contact's avatar PNG to ~/.kokoroo/avatars/{contact_id}.png.
pub fn save_contact_avatar(contact_id: &str, data: &[u8]) -> Result<(), String> {
    let dir = data_dir().join("avatars");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    fs::write(contact_avatar_path(contact_id), data).map_err(|e| format!("write: {e}"))
}

/// Read our own avatar bytes from disk, if it exists.
pub fn load_own_avatar() -> Option<Vec<u8>> {
    fs::read(own_avatar_path()).ok()
}

/// Read a contact's avatar bytes from disk, if it exists.
pub fn load_contact_avatar(contact_id: &str) -> Option<Vec<u8>> {
    fs::read(contact_avatar_path(contact_id)).ok()
}

/// Compute SHA-256 hash of avatar bytes.
pub fn avatar_sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

// ── Group avatar storage ──

/// Path to a group's avatar.
pub fn group_avatar_path(group_id: &str) -> PathBuf {
    data_dir().join("groups").join("avatars").join(format!("{group_id}.png"))
}

/// Save a group's avatar PNG to ~/.kokoroo/groups/avatars/{group_id}.png.
pub fn save_group_avatar(group_id: &str, data: &[u8]) -> Result<(), String> {
    let dir = data_dir().join("groups").join("avatars");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    fs::write(group_avatar_path(group_id), data).map_err(|e| format!("write: {e}"))
}

/// Read a group's avatar bytes from disk, if it exists.
pub fn load_group_avatar(group_id: &str) -> Option<Vec<u8>> {
    fs::read(group_avatar_path(group_id)).ok()
}

/// Delete a group's avatar from disk.
pub fn delete_group_avatar(group_id: &str) {
    let path = group_avatar_path(group_id);
    if path.exists() {
        fs::remove_file(path).ok();
    }
}

/// Apply a circular alpha mask with anti-aliased edges to an RgbaImage.
/// Pixels outside the inscribed circle get alpha=0, edge pixels get smooth falloff.
pub fn apply_circle_mask(img: &mut RgbaImage) {
    let w = img.width() as f32;
    let h = img.height() as f32;
    let cx = w / 2.0;
    let cy = h / 2.0;
    let r = w.min(h) / 2.0;
    // 1.0px feather zone for smooth antialiasing
    let r_inner = r - 1.0;
    for y in 0..img.height() {
        for x in 0..img.width() {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > r {
                img.get_pixel_mut(x, y)[3] = 0;
            } else if dist > r_inner {
                // Smooth falloff in the feather zone
                let t = 1.0 - (dist - r_inner) / (r - r_inner);
                let original_alpha = img.get_pixel(x, y)[3] as f32;
                img.get_pixel_mut(x, y)[3] = (original_alpha * t) as u8;
            }
        }
    }
}
