//! egui's bundled fonts have no CJK glyphs, yet device names come in the user's language.
//! Installed Windows fonts are mapped read-only as fallbacks: pages load only for glyphs
//! that are drawn, so unused fallbacks cost address space, not memory.

use std::{
    fs::{File, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    sync::Arc,
};

use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    Globalization::GetUserDefaultUILanguage,
    Storage::FileSystem::FILE_SHARE_READ,
    System::Memory::{CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY},
};

/// `(file, collection index)`; the four CJK families of a stock Windows install.
const SIMPLIFIED: (&str, u32) = ("msyh.ttc", 0);
const TRADITIONAL: (&str, u32) = ("msjh.ttc", 0);
const JAPANESE: (&str, u32) = ("YuGothM.ttc", 0);
const KOREAN: (&str, u32) = ("malgun.ttf", 0);

/// Han characters differ in shape by region, so the UI language's font goes first.
fn preferred_order(language: u16) -> [(&'static str, u32); 4] {
    // Primary language and sublanguage of a LANGID.
    match (language & 0x3FF, language >> 10) {
        (0x04, 0x01 | 0x03 | 0x05) => [TRADITIONAL, SIMPLIFIED, JAPANESE, KOREAN],
        (0x11, _) => [JAPANESE, SIMPLIFIED, TRADITIONAL, KOREAN],
        (0x12, _) => [KOREAN, SIMPLIFIED, TRADITIONAL, JAPANESE],
        _ => [SIMPLIFIED, TRADITIONAL, JAPANESE, KOREAN],
    }
}

pub(crate) fn install_fallbacks(ctx: &Context) {
    let fonts_dir = std::env::var_os("WINDIR")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("Fonts");
    // SAFETY: no preconditions.
    let language = unsafe { GetUserDefaultUILanguage() };
    let mut definitions = FontDefinitions::default();
    for (file, index) in preferred_order(language) {
        let Some(bytes) = map_read_only(&fonts_dir.join(file)) else {
            continue;
        };
        let mut data = FontData::from_static(bytes);
        data.index = index;
        definitions
            .font_data
            .insert(file.to_owned(), Arc::new(data));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            definitions
                .families
                .entry(family)
                .or_default()
                .push(file.to_owned());
        }
    }
    ctx.set_fonts(definitions);
}

/// The file stays open, shared for reading only, and mapped for the life of the process,
/// so no other process can write the bytes behind the returned slice.
fn map_read_only(path: &Path) -> Option<&'static [u8]> {
    let file: File = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .ok()?;
    let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if length == 0 {
        return None;
    }
    // SAFETY: the handle belongs to `file`, alive for the call; a null name creates an
    // anonymous mapping of the whole file.
    let mapping = unsafe {
        CreateFileMappingW(
            file.as_raw_handle(),
            std::ptr::null(),
            PAGE_READONLY,
            0,
            0,
            std::ptr::null(),
        )
    };
    if mapping.is_null() {
        return None;
    }
    // SAFETY: `mapping` is a valid read-only mapping; zero length maps all of it.
    let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
    // SAFETY: the view keeps the mapping object alive after its handle closes.
    unsafe { CloseHandle(mapping) };
    if view.Value.is_null() {
        return None;
    }
    std::mem::forget(file);
    // SAFETY: the view spans `length` readable bytes, is never unmapped, and nothing
    // can write the file while the leaked handle denies write sharing.
    Some(unsafe { std::slice::from_raw_parts(view.Value.cast::<u8>(), length) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ui_language_picks_the_first_han_font() {
        assert_eq!(preferred_order(0x0804)[0], SIMPLIFIED); // zh-CN
        assert_eq!(preferred_order(0x0404)[0], TRADITIONAL); // zh-TW
        assert_eq!(preferred_order(0x0C04)[0], TRADITIONAL); // zh-HK
        assert_eq!(preferred_order(0x0411)[0], JAPANESE);
        assert_eq!(preferred_order(0x0412)[0], KOREAN);
        assert_eq!(preferred_order(0x0409)[0], SIMPLIFIED); // en-US
    }
}
