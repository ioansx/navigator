use std::path::PathBuf;

use resvg::{tiny_skia::Pixmap, usvg::Tree};

pub fn is_svg(path: &PathBuf) -> bool {
    path.extension()
        .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("svg"))
}

pub fn rasterize_svg(path: &PathBuf) -> Option<image::DynamicImage> {
    let data: Vec<u8> = std::fs::read(path).ok()?;
    let tree = Tree::from_data(&data, &Default::default()).ok()?;
    let size = tree.size().to_int_size();
    let mut pixmap = Pixmap::new(size.width(), size.height())?;
    resvg::render(&tree, Default::default(), &mut pixmap.as_mut());
    let img = image::RgbaImage::from_raw(size.width(), size.height(), pixmap.take())?;
    Some(image::DynamicImage::ImageRgba8(img))
}
