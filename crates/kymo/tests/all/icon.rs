//! The app's icon: the window's, and the PNGs rendered from `Kymo.icon` for the packages.

use std::path::Path;

use eframe::egui::IconData;

/// The RGBA pixel at (x, y).
fn pixel(icon: &IconData, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * icon.width + x) * 4) as usize;
    icon.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn window_icon_is_the_drum_without_a_tile() {
    let icon = kymo::app::icon();
    assert_eq!((icon.width, icon.height), (256, 256));
    assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    // No tile behind it: the edges are transparent, corners and middles alike (but for a
    // trace of the shadow's faintest edge).
    for (x, y) in [
        (2, 2),
        (253, 2),
        (2, 253),
        (253, 253),
        (128, 2),
        (2, 128),
        (253, 128),
        (128, 253),
    ] {
        assert!(pixel(&icon, x, y)[3] < 8, "edge ({x}, {y})");
    }
    assert_eq!(pixel(&icon, 128, 128)[3], 255);
}

#[test]
fn macos_dock_icon_leaves_the_icon_grid_margins() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/png/kymo-macos-256.png");
    let icon = eframe::icon_data::from_png_bytes(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!((icon.width, icon.height), (256, 256));
    // The tile is 206 of 256 px (824 of 1024), centered: 25 px of margin on each side.
    for (x, y) in [(10, 128), (245, 128), (128, 10), (128, 245)] {
        assert_eq!(pixel(&icon, x, y)[3], 0, "margin ({x}, {y})");
    }
    for (x, y) in [(30, 128), (225, 128), (128, 30), (128, 225), (128, 128)] {
        assert_eq!(pixel(&icon, x, y)[3], 255, "tile ({x}, {y})");
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

#[test]
fn windows_icon_holds_each_size_as_a_png() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/kymo.ico");
    let ico = std::fs::read(path).unwrap();
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(ico[i..i + 4].try_into().unwrap());
    // An icon file (type 1) with one entry per size.
    assert_eq!((u16_at(0), u16_at(2)), (0, 1));
    let count = u16_at(4) as usize;
    let mut sizes = Vec::new();
    for entry in 0..count {
        let at = 6 + 16 * entry;
        // 0 stands for 256.
        let width = if ico[at] == 0 {
            256
        } else {
            u32::from(ico[at])
        };
        let (len, offset) = (u32_at(at + 8) as usize, u32_at(at + 12) as usize);
        let png = eframe::icon_data::from_png_bytes(&ico[offset..offset + len]).unwrap();
        assert_eq!((png.width, png.height), (width, width));
        sizes.push(width);
    }
    assert_eq!(sizes, [16, 24, 32, 48, 64, 256]);
}
