use std::{fs, path::Path};

use image::DynamicImage;
use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg::{Options, Tree},
};

use crate::error::{Errx, Resultx};

/// Decodes `path` into pixels. SVGs are rasterized at their natural size.
pub fn load(path: &Path) -> Resultx<DynamicImage> {
    if is_svg(path) {
        return rasterize_svg(path);
    }

    image::open(path).map_err(|e| Errx::e_any(e, format!("decoding {}", path.display())))
}

fn is_svg(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
}

fn rasterize_svg(path: &Path) -> Resultx<DynamicImage> {
    let data = fs::read(path).map_err(|e| Errx::e_io(e, format!("reading {}", path.display())))?;

    let tree = Tree::from_data(&data, &Options::default())
        .map_err(|e| Errx::e_any(e, format!("parsing {}", path.display())))?;

    let size = tree.size().to_int_size();
    let mut pixmap = Pixmap::new(size.width(), size.height())
        .ok_or_else(|| Errx::any(format!("{} has no drawable area", path.display())))?;
    resvg::render(&tree, Transform::default(), &mut pixmap.as_mut());

    image::RgbaImage::from_raw(size.width(), size.height(), pixmap.take())
        .map(DynamicImage::ImageRgba8)
        .ok_or_else(|| Errx::any(format!("rasterizing {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testdir::TempDir;

    /// A 2x1 PNG: one red pixel, one blue one.
    const PNG: [u8; 70] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x7b,
        0x40, 0xe8, 0xdd, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0x00, 0x04, 0xff, 0x01, 0x07, 0x00, 0x01, 0xff, 0xe2, 0x23, 0x9e, 0x59, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
        <rect width="40" height="20" fill="red"/>
    </svg>"#;

    #[test]
    fn decodes_a_png() {
        let tmp = TempDir::new();
        let path = tmp.file("pixel.png", PNG);

        let img = load(&path).unwrap();

        assert_eq!((img.width(), img.height()), (2, 1));
    }

    #[test]
    fn rasterizes_an_svg_at_its_natural_size() {
        let tmp = TempDir::new();
        let path = tmp.file("box.svg", SVG);

        let img = load(&path).unwrap();

        assert_eq!((img.width(), img.height()), (40, 20));
    }

    #[test]
    fn recognises_svgs_regardless_of_extension_case() {
        let tmp = TempDir::new();
        let path = tmp.file("box.SVG", SVG);

        // A bitmap decoder would reject this content, so succeeding proves it took the SVG path.
        assert!(load(&path).is_ok());
    }

    #[test]
    fn a_malformed_svg_fails() {
        let tmp = TempDir::new();
        let path = tmp.file("broken.svg", "<svg><unclosed>");

        assert!(load(&path).is_err());
    }

    #[test]
    fn an_svg_extension_on_non_svg_content_fails() {
        let tmp = TempDir::new();
        let path = tmp.file("liar.svg", PNG);

        assert!(load(&path).is_err());
    }

    #[test]
    fn a_truncated_png_fails() {
        let tmp = TempDir::new();
        let path = tmp.file("cut.png", &PNG[..30]);

        assert!(load(&path).is_err());
    }

    #[test]
    fn a_text_file_named_png_fails() {
        let tmp = TempDir::new();
        let path = tmp.file("notreally.png", "just some words");

        assert!(load(&path).is_err());
    }

    #[test]
    fn a_missing_file_fails() {
        let tmp = TempDir::new();

        assert!(load(&tmp.path().join("gone.png")).is_err());
        assert!(load(&tmp.path().join("gone.svg")).is_err());
    }
}
