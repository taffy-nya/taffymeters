use std::sync::{Arc, LazyLock};

use egui::IconData;

fn icon(rgba: &[u8], size: u32) -> Arc<IconData> {
    Arc::new(IconData {
        rgba: rgba.to_vec(),
        width: size,
        height: size,
    })
}

static SMALL: LazyLock<Arc<IconData>> = LazyLock::new(|| {
    icon(
        include_bytes!(concat!(env!("OUT_DIR"), "/icon-32.rgba")),
        32,
    )
});

/// egui-winit only sets ICON_SMALL on the windows it creates (32 px at 200% scaling).
pub(super) fn small() -> Arc<IconData> {
    Arc::clone(&SMALL)
}
