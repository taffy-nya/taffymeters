//! Window icons from `assets/taffymeters.ico` (RGBA extracted by `build.rs`).

use std::sync::{Arc, LazyLock};

use eframe::egui::IconData;

fn icon(rgba: &[u8], size: u32) -> Arc<IconData> {
    Arc::new(IconData {
        rgba: rgba.to_vec(),
        width: size,
        height: size,
    })
}

static LARGE: LazyLock<Arc<IconData>> = LazyLock::new(|| {
    icon(
        include_bytes!(concat!(env!("OUT_DIR"), "/icon-128.rgba")),
        128,
    )
});
static SMALL: LazyLock<Arc<IconData>> = LazyLock::new(|| {
    icon(
        include_bytes!(concat!(env!("OUT_DIR"), "/icon-32.rgba")),
        32,
    )
});

/// Root viewport icon. On Windows eframe rescales it to the system icon sizes and
/// sets both the big and the small icon; it also replaces eframe's default egui logo.
pub(crate) fn large() -> Arc<IconData> {
    Arc::clone(&LARGE)
}

/// Icon of every other viewport. egui-winit only sets the small icon (`ICON_SMALL`),
/// which Windows shows at 32 px under 200% scaling.
pub(crate) fn small() -> Arc<IconData> {
    Arc::clone(&SMALL)
}
