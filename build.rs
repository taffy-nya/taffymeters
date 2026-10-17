//! Embeds `assets/taffymeters.rc` (icon and version information, the version
//! from Cargo.toml) into the Windows executable and extracts the 128×128 and
//! 32×32 images of `assets/taffymeters.ico` as raw RGBA for the runtime window
//! icons (`src/icon.rs`).

use std::{env, fs, path::Path};

const ICON: &str = "assets/taffymeters.ico";
const RESOURCE: &str = "assets/taffymeters.rc";
/// Sizes `src/icon.rs` includes as `icon-<size>.rgba`. 128 covers the big
/// system icon (`SM_CXICON`, 32 px per 100% scaling) up to 400%.
const RGBA_SIZES: [u32; 2] = [128, 32];

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed={RESOURCE}");

    let ico = fs::read(ICON).unwrap_or_else(|error| panic!("read {ICON}: {error}"));
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    for size in RGBA_SIZES {
        let rgba = ico_entry_rgba(&ico, size);
        let path = Path::new(&out_dir).join(format!("icon-{size}.rgba"));
        fs::write(&path, rgba).unwrap_or_else(|error| panic!("write {path:?}: {error}"));
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = |part| env::var(format!("CARGO_PKG_VERSION_{part}")).expect(part);
        let macros = [
            format!("VERSION_MAJOR={}", version("MAJOR")),
            format!("VERSION_MINOR={}", version("MINOR")),
            format!("VERSION_PATCH={}", version("PATCH")),
            format!("VERSION_STR=\"{}\"", env::var("CARGO_PKG_VERSION").unwrap()),
        ];
        embed_resource::compile(RESOURCE, macros)
            .manifest_optional()
            .unwrap();
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// Top-down RGBA of the `size`×`size` entry, which must be an uncompressed
/// 32 bpp DIB (BGRA rows bottom-up, real alpha; the AND mask is ignored).
fn ico_entry_rgba(ico: &[u8], size: u32) -> Vec<u8> {
    assert_eq!(u16_at(ico, 2), 1, "{ICON}: not an icon file");
    let count = usize::from(u16_at(ico, 4));
    let entry = (0..count)
        .map(|index| 6 + 16 * index)
        .find(|&entry| {
            // A stored 0 means 256.
            let width = if ico[entry] == 0 {
                256
            } else {
                u32::from(ico[entry])
            };
            width == size
        })
        .unwrap_or_else(|| panic!("{ICON}: no {size}×{size} image"));
    let dib = &ico[u32_at(ico, entry + 12) as usize..];

    let header = u32_at(dib, 0) as usize;
    let width = u32_at(dib, 4);
    let height = u32_at(dib, 8) / 2; // DIB height counts the AND mask too.
    let bits = u16_at(dib, 14);
    let compression = u32_at(dib, 16);
    assert!(
        width == size && height == size && bits == 32 && compression == 0,
        "{ICON}: the {size}×{size} image must be an uncompressed 32 bpp DIB"
    );

    let stride = size as usize * 4;
    let pixels = &dib[header..header + stride * size as usize];
    let mut rgba = Vec::with_capacity(pixels.len());
    for row in pixels.chunks_exact(stride).rev() {
        for &[blue, green, red, alpha] in row.as_chunks::<4>().0 {
            rgba.extend_from_slice(&[red, green, blue, alpha]);
        }
    }
    rgba
}
