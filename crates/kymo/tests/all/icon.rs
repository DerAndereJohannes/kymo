//! The app's icon: the window's, and the PNGs rendered from the SVGs for the packages.

use std::path::Path;

use eframe::egui::IconData;

/// The RGBA pixel at (x, y).
fn pixel(icon: &IconData, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * icon.width + x) * 4) as usize;
    icon.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn window_icon_is_a_rounded_tile() {
    let icon = kymo::app::icon();
    assert_eq!((icon.width, icon.height), (256, 256));
    assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    // The tile fills the image but for its rounded corners.
    for (x, y) in [(2, 2), (253, 2), (2, 253), (253, 253)] {
        assert_eq!(pixel(&icon, x, y)[3], 0, "corner ({x}, {y})");
    }
    for (x, y) in [(128, 2), (2, 128), (253, 128), (128, 253), (128, 128)] {
        assert_eq!(pixel(&icon, x, y)[3], 255, "({x}, {y})");
    }
}

#[test]
fn every_png_has_its_size() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/png");
    for size in [16, 24, 32, 48, 64, 128, 256, 512, 1024] {
        let path = dir.join(format!("kymo-{size}.png"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let icon = eframe::icon_data::from_png_bytes(&bytes).unwrap();
        assert_eq!(
            (icon.width, icon.height),
            (size, size),
            "{}",
            path.display()
        );
        let center = pixel(&icon, size / 2, size / 2);
        assert_eq!(center[3], 255, "{}", path.display());
    }
}
