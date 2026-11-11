//! 이미지 → PDF.
//!
//! - JPEG은 다시 압축하지 않고 원본 데이터를 그대로 넣는다(DCTDecode) → 화질 손실 없음.
//! - 그 밖의 형식은 디코딩해서 무손실(Flate)로 넣고, 투명한 부분은 흰색으로 채운다.
//! - 휴대폰 사진의 EXIF 회전 정보를 반영한다.
//! - 페이지는 이미지 비율 그대로, A4 안에 꽉 차게 맞춘다(여백 없음).

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use image::{ColorType, DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref};

use crate::Cancelled;

const A4_SHORT: f32 = 595.28;
const A4_LONG: f32 = 841.89;

enum Pixels {
    /// 원본 JPEG 파일 내용 그대로.
    Jpeg(Vec<u8>),
    /// zlib으로 압축한 8비트 픽셀 (회색 1채널 또는 RGB 3채널).
    Flate(Vec<u8>),
}

struct PageImage {
    /// 저장된 픽셀 크기 (회전 적용 전).
    width: u32,
    height: u32,
    gray: bool,
    /// EXIF 방향 값 1~8. 디코딩한 이미지는 이미 바로 세웠으므로 항상 1.
    orientation: u8,
    pixels: Pixels,
}

struct JpegInfo {
    width: u32,
    height: u32,
    components: u8,
    precision: u8,
    /// PDF가 그대로 받아들이는 방식(baseline/extended/progressive Huffman)인지.
    pdf_compatible: bool,
}

/// JPEG 헤더에서 크기와 색 채널 수를 읽는다. JPEG이 아니면 `None`.
fn jpeg_info(data: &[u8]) -> Option<JpegInfo> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    loop {
        while i < data.len() && data[i] != 0xFF {
            i += 1;
        }
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            return None;
        }
        let marker = data[i];
        i += 1;
        match marker {
            0x01 | 0xD0..=0xD8 => continue, // 길이 필드가 없는 마커
            0xD9 | 0xDA => return None,     // 프레임 헤더 없이 끝남
            _ => {}
        }
        if i + 2 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if len < 2 || i + len > data.len() {
            return None;
        }
        let is_frame_header = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_frame_header {
            if len < 8 {
                return None;
            }
            return Some(JpegInfo {
                precision: data[i + 2],
                height: u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32,
                width: u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32,
                components: data[i + 7],
                pdf_compatible: matches!(marker, 0xC0..=0xC2),
            });
        }
        i += len;
    }
}

fn jpeg_orientation(bytes: &[u8]) -> u8 {
    ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg)
        .into_decoder()
        .ok()
        .and_then(|mut decoder| decoder.orientation().ok())
        .map(|orientation| orientation.to_exif())
        .unwrap_or(1)
}

fn load_image(path: &Path) -> Result<PageImage> {
    let bytes = std::fs::read(path).context("파일을 읽을 수 없습니다")?;

    if let Some(info) = jpeg_info(&bytes) {
        let passthrough = info.pdf_compatible
            && info.precision == 8
            && matches!(info.components, 1 | 3)
            && info.width > 0
            && info.height > 0;
        if passthrough {
            return Ok(PageImage {
                width: info.width,
                height: info.height,
                gray: info.components == 1,
                orientation: jpeg_orientation(&bytes),
                pixels: Pixels::Jpeg(bytes),
            });
        }
    }

    // JPEG이 아니거나(또는 CMYK 등 특수한 JPEG) → 디코딩해서 넣는다.
    let reader = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .context("이미지 형식을 알 수 없습니다")?;
    let mut decoder = reader
        .into_decoder()
        .context("지원하지 않거나 손상된 이미지입니다")?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).context("이미지를 해석할 수 없습니다")?;
    if let Some(orientation) = orientation {
        img.apply_orientation(orientation);
    }

    let (width, height) = (img.width(), img.height());
    if width == 0 || height == 0 {
        bail!("이미지 크기가 0입니다");
    }

    let is_gray = matches!(img.color(), ColorType::L8 | ColorType::L16);
    let raw = if is_gray {
        img.into_luma8().into_raw()
    } else if img.color().has_alpha() {
        flatten_on_white(&img.into_rgba8())
    } else {
        img.into_rgb8().into_raw()
    };
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);

    Ok(PageImage {
        width,
        height,
        gray: is_gray,
        orientation: 1,
        pixels: Pixels::Flate(compressed),
    })
}

/// 투명도가 있는 이미지를 흰 배경 위에 합성해 RGB로 만든다.
fn flatten_on_white(rgba: &image::RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.width() as usize * rgba.height() as usize * 3);
    for pixel in rgba.pixels() {
        let [r, g, b, a] = pixel.0;
        let a = a as u32;
        for channel in [r, g, b] {
            let blended = (channel as u32 * a + 255 * (255 - a) + 127) / 255;
            rgb.push(blended as u8);
        }
    }
    rgb
}

/// 화면에 보이는 크기(회전 적용 후)를 A4에 맞춘 페이지 크기(pt)를 구한다.
fn page_size(display_width: f32, display_height: f32) -> (f32, f32) {
    let (max_w, max_h) = if display_width > display_height {
        (A4_LONG, A4_SHORT)
    } else {
        (A4_SHORT, A4_LONG)
    };
    let scale = (max_w / display_width).min(max_h / display_height);
    (display_width * scale, display_height * scale)
}

/// 단위 정사각형에 그려지는 이미지를 페이지 전체로 옮기는 변환 행렬.
/// `w`,`h`는 회전 적용 *전* 이미지의 페이지상 크기(pt).
fn placement_matrix(orientation: u8, w: f32, h: f32) -> [f32; 6] {
    match orientation {
        2 => [-w, 0.0, 0.0, h, w, 0.0],   // 좌우 반전
        3 => [-w, 0.0, 0.0, -h, w, h],    // 180도
        4 => [w, 0.0, 0.0, -h, 0.0, h],   // 상하 반전
        5 => [0.0, -w, -h, 0.0, h, w],    // 대각선 반전
        6 => [0.0, -w, h, 0.0, 0.0, w],   // 시계 방향 90도
        7 => [0.0, w, h, 0.0, 0.0, 0.0],  // 반대 대각선 반전
        8 => [0.0, w, -h, 0.0, h, 0.0],   // 반시계 방향 90도
        _ => [w, 0.0, 0.0, h, 0.0, 0.0],
    }
}

/// `inputs`의 이미지를 순서대로 한 페이지씩 넣은 PDF를 `output`에 만든다.
///
/// `on_page(i)`는 i번째 이미지를 처리하기 직전에 호출되며, `false`를 돌려주면 취소한다.
pub fn images_to_pdf(
    inputs: &[PathBuf],
    output: &Path,
    on_page: impl FnMut(usize) -> bool,
) -> Result<()> {
    let bytes = images_to_pdf_bytes(inputs, on_page)?;
    std::fs::write(output, bytes).context("PDF 파일을 저장할 수 없습니다")?;
    Ok(())
}

/// [`images_to_pdf`] 와 같지만 파일로 쓰지 않고 PDF 내용을 돌려준다.
pub fn images_to_pdf_bytes(
    inputs: &[PathBuf],
    mut on_page: impl FnMut(usize) -> bool,
) -> Result<Vec<u8>> {
    if inputs.is_empty() {
        bail!("변환할 이미지가 없습니다");
    }

    let mut alloc = Ref::new(1);
    let catalog_id = alloc.bump();
    let page_tree_id = alloc.bump();

    let mut pdf = Pdf::new();
    let mut page_ids = Vec::with_capacity(inputs.len());
    let image_name = Name(b"Im0");

    for (index, input) in inputs.iter().enumerate() {
        if !on_page(index) {
            return Err(Cancelled.into());
        }
        let file_name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let img = load_image(input).with_context(|| file_name)?;

        let rotated = img.orientation >= 5;
        let (display_w, display_h) = if rotated {
            (img.height as f32, img.width as f32)
        } else {
            (img.width as f32, img.height as f32)
        };
        let (page_w, page_h) = page_size(display_w, display_h);
        let (stored_w, stored_h) = if rotated { (page_h, page_w) } else { (page_w, page_h) };

        let page_id = alloc.bump();
        let image_id = alloc.bump();
        let content_id = alloc.bump();
        page_ids.push(page_id);

        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, page_w, page_h));
        page.parent(page_tree_id);
        page.contents(content_id);
        page.resources().x_objects().pair(image_name, image_id);
        page.finish();

        let (data, filter) = match &img.pixels {
            Pixels::Jpeg(data) => (data, Filter::DctDecode),
            Pixels::Flate(data) => (data, Filter::FlateDecode),
        };
        let mut xobject = pdf.image_xobject(image_id, data);
        xobject.filter(filter);
        xobject.width(img.width as i32);
        xobject.height(img.height as i32);
        if img.gray {
            xobject.color_space().device_gray();
        } else {
            xobject.color_space().device_rgb();
        }
        xobject.bits_per_component(8);
        xobject.finish();

        let mut content = Content::new();
        content.save_state();
        content.transform(placement_matrix(img.orientation, stored_w, stored_h));
        content.x_object(image_name);
        content.restore_state();
        pdf.stream(content_id, &content.finish());
    }

    pdf.catalog(catalog_id).pages(page_tree_id);
    pdf.pages(page_tree_id)
        .kids(page_ids.iter().copied())
        .count(page_ids.len() as i32);

    Ok(pdf.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_fits_inside_a4_keeping_aspect() {
        let (w, h) = page_size(3000.0, 4000.0);
        assert!((w / h - 0.75).abs() < 1e-4);
        assert!(w <= A4_SHORT + 0.01 && h <= A4_LONG + 0.01);

        let (w, h) = page_size(4000.0, 3000.0);
        assert!(w > h, "가로 사진은 가로 페이지");
        assert!(w <= A4_LONG + 0.01 && h <= A4_SHORT + 0.01);
    }

    #[test]
    fn rotation_matrices_keep_image_on_page() {
        // 단위 정사각형의 네 꼭짓점이 모두 페이지 안에 들어와야 한다.
        for orientation in 1..=8u8 {
            let (w, h) = (300.0f32, 200.0f32);
            let (page_w, page_h) = if orientation >= 5 { (h, w) } else { (w, h) };
            let [a, b, c, d, e, f] = placement_matrix(orientation, w, h);
            for (x, y) in [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let px = a * x + c * y + e;
                let py = b * x + d * y + f;
                assert!(
                    (-0.01..=page_w + 0.01).contains(&px) && (-0.01..=page_h + 0.01).contains(&py),
                    "orientation {orientation}: ({px}, {py}) 가 페이지 밖"
                );
            }
        }
    }

    #[test]
    fn reads_jpeg_header() {
        let img = image::RgbImage::from_pixel(40, 30, image::Rgb([200, 10, 10]));
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        let info = jpeg_info(&bytes).expect("JPEG 헤더");
        assert_eq!((info.width, info.height, info.components), (40, 30, 3));
        assert!(info.pdf_compatible);
        assert!(jpeg_info(b"\x89PNG....").is_none());
    }
}
