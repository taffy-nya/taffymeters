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

/// On Windows, eframe sets both system icon sizes from the root viewport's icon.
pub(crate) fn large() -> Arc<IconData> {
    Arc::clone(&LARGE)
}

/// egui-winit only sets ICON_SMALL on child viewports (32 px at 200% scaling).
pub(crate) fn small() -> Arc<IconData> {
    Arc::clone(&SMALL)
}
