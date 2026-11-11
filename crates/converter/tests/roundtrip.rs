//! 실제 파일로 왕복 변환을 확인한다: 이미지 → PDF → (Windows 렌더러) → 이미지.

use std::path::{Path, PathBuf};

use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use ikopdf::img2pdf::images_to_pdf;
use ikopdf::job::{self, Mode, Progress};
use ikopdf::pdf2img::{page_count, pdf_to_images, OutputFormat};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("imgpdf-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 왼쪽 절반은 빨강, 오른쪽 절반은 파랑인 이미지.
fn red_blue(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, _| {
        if x < width / 2 {
            Rgb([220, 20, 20])
        } else {
            Rgb([20, 20, 220])
        }
    })
}

fn save(img: &RgbImage, path: &Path, format: ImageFormat) {
    DynamicImage::ImageRgb8(img.clone())
        .save_with_format(path, format)
        .unwrap();
}

fn is_reddish(p: Rgba<u8>) -> bool {
    p[0] > 150 && p[1] < 90 && p[2] < 90
}

fn is_bluish(p: Rgba<u8>) -> bool {
    p[2] > 150 && p[0] < 90 && p[1] < 90
}

#[test]
fn jpeg_to_pdf_and_back_keeps_layout() {
    let dir = TempDir::new("jpeg");
    let jpg = dir.path("가로 사진.jpg");
    save(&red_blue(400, 200), &jpg, ImageFormat::Jpeg);

    let pdf = dir.path("가로 사진.pdf");
    images_to_pdf(&[jpg], &pdf, |_| true).unwrap();
    assert_eq!(page_count(&pdf).unwrap(), 1);

    let outputs = pdf_to_images(&pdf, OutputFormat::Png, 96.0, |_, _| true).unwrap();
    assert_eq!(outputs, vec![dir.path("가로 사진.png")]);

    let rendered = image::open(&outputs[0]).unwrap();
    let (w, h) = rendered.dimensions();
    assert!((w as f32 / h as f32 - 2.0).abs() < 0.02, "비율 유지: {w}x{h}");
    assert!(is_reddish(rendered.get_pixel(w / 4, h / 2)), "왼쪽은 빨강");
    assert!(is_bluish(rendered.get_pixel(w * 3 / 4, h / 2)), "오른쪽은 파랑");
}

#[test]
fn transparent_png_is_flattened_on_white() {
    let dir = TempDir::new("png");
    let png = dir.path("투명.png");
    let img = RgbaImage::from_fn(100, 100, |x, _| {
        if x < 50 {
            Rgba([0, 0, 0, 0]) // 완전 투명
        } else {
            Rgba([20, 20, 220, 255])
        }
    });
    DynamicImage::ImageRgba8(img).save(&png).unwrap();

    let pdf = dir.path("투명.pdf");
    images_to_pdf(&[png], &pdf, |_| true).unwrap();
    let outputs = pdf_to_images(&pdf, OutputFormat::Jpg, 96.0, |_, _| true).unwrap();
    assert_eq!(outputs[0].extension().unwrap(), "jpg");

    let rendered = image::open(&outputs[0]).unwrap();
    let (w, h) = rendered.dimensions();
    let left = rendered.get_pixel(w / 4, h / 2);
    assert!(left[0] > 235 && left[1] > 235 && left[2] > 235, "투명 영역은 흰색: {left:?}");
    assert!(is_bluish(rendered.get_pixel(w * 3 / 4, h / 2)));
}

#[test]
fn merge_orders_pages_naturally_and_splits_into_folder() {
    let dir = TempDir::new("merge");
    // 10번이 2번보다 뒤에 와야 한다.
    let red = RgbImage::from_pixel(120, 160, Rgb([220, 20, 20]));
    let blue = RgbImage::from_pixel(120, 160, Rgb([20, 20, 220]));
    save(&blue, &dir.path("스캔_10.png"), ImageFormat::Png);
    save(&red, &dir.path("스캔_2.png"), ImageFormat::Png);

    let progress = Progress::default();
    let inputs = job::prepare_inputs(
        Mode::Merge,
        vec![dir.path("스캔_10.png"), dir.path("스캔_2.png")],
    );
    let outcome = job::run(Mode::Merge, inputs, &progress);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let merged = dir.path("스캔_2_합본.pdf");
    assert_eq!(outcome.created, vec![merged.clone()]);
    assert_eq!(page_count(&merged).unwrap(), 2);

    let outcome = job::run(Mode::ToPng, vec![merged], &progress);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let folder = dir.path("스캔_2_합본");
    assert_eq!(
        outcome.created,
        vec![folder.join("스캔_2_합본_001.png"), folder.join("스캔_2_합본_002.png")]
    );
    let first = image::open(&outcome.created[0]).unwrap();
    let second = image::open(&outcome.created[1]).unwrap();
    assert!(is_reddish(first.get_pixel(first.width() / 2, first.height() / 2)));
    assert!(is_bluish(second.get_pixel(second.width() / 2, second.height() / 2)));
}

#[test]
fn per_file_conversion_never_overwrites() {
    let dir = TempDir::new("perfile");
    let jpg = dir.path("a.jpg");
    save(&red_blue(64, 64), &jpg, ImageFormat::Jpeg);
    std::fs::write(dir.path("a.pdf"), b"existing").unwrap();

    let outcome = job::run(Mode::ToPdf, vec![jpg], &Progress::default());
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.created, vec![dir.path("a (2).pdf")]);
    assert_eq!(std::fs::read(dir.path("a.pdf")).unwrap(), b"existing");
}

#[test]
fn reports_broken_inputs_instead_of_crashing() {
    let dir = TempDir::new("broken");
    let fake_jpg = dir.path("가짜.jpg");
    std::fs::write(&fake_jpg, b"this is not an image").unwrap();
    let fake_pdf = dir.path("가짜.pdf");
    std::fs::write(&fake_pdf, b"this is not a pdf").unwrap();

    let outcome = job::run(Mode::ToPdf, vec![fake_jpg], &Progress::default());
    assert_eq!(outcome.failures.len(), 1);
    assert!(outcome.created.is_empty());

    let outcome = job::run(Mode::ToJpg, vec![fake_pdf], &Progress::default());
    assert_eq!(outcome.failures.len(), 1);
    assert!(outcome.created.is_empty());
}

fn is_greenish(p: Rgba<u8>) -> bool {
    p[1] > 150 && p[0] < 90 && p[2] < 90
}

fn center(path: &Path) -> Rgba<u8> {
    let img = image::open(path).unwrap();
    img.get_pixel(img.width() / 2, img.height() / 2)
}

/// 세 가지 색 이미지로 3쪽짜리 PDF를 만든다 (1쪽 빨강, 2쪽 파랑, 3쪽 초록).
fn three_page_pdf(dir: &TempDir, name: &str) -> PathBuf {
    let colors = [Rgb([220, 20, 20]), Rgb([20, 20, 220]), Rgb([20, 200, 20])];
    let mut images = Vec::new();
    for (index, color) in colors.iter().enumerate() {
        let path = dir.path(&format!("{name}_src_{}.png", index + 1));
        save(&RgbImage::from_pixel(90, 120, *color), &path, ImageFormat::Png);
        images.push(path);
    }
    let pdf = dir.path(&format!("{name}.pdf"));
    images_to_pdf(&images, &pdf, |_| true).unwrap();
    pdf
}

#[test]
fn split_makes_one_pdf_per_page() {
    let dir = TempDir::new("split");
    let pdf = three_page_pdf(&dir, "보고서");

    let outcome = job::run(Mode::Split, vec![pdf], &Progress::default());
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let folder = dir.path("보고서");
    assert_eq!(
        outcome.created,
        vec![
            folder.join("보고서_001.pdf"),
            folder.join("보고서_002.pdf"),
            folder.join("보고서_003.pdf"),
        ]
    );
    for part in &outcome.created {
        assert_eq!(page_count(part).unwrap(), 1, "{}", part.display());
    }
    // 2쪽만 따로 그려 보면 파란색이어야 한다.
    let rendered = pdf_to_images(&outcome.created[1], OutputFormat::Png, 96.0, |_, _| true).unwrap();
    assert!(is_bluish(center(&rendered[0])));
}

#[test]
fn split_refuses_single_page_pdf() {
    let dir = TempDir::new("split1");
    let png = dir.path("한장.png");
    save(&red_blue(60, 60), &png, ImageFormat::Png);
    let pdf = dir.path("한장.pdf");
    images_to_pdf(&[png], &pdf, |_| true).unwrap();

    let outcome = job::run(Mode::Split, vec![pdf], &Progress::default());
    assert_eq!(outcome.failures.len(), 1);
    assert!(outcome.created.is_empty());
    assert!(!dir.path("한장").exists(), "빈 폴더를 남기지 않는다");
}

#[test]
fn merge_keeps_given_order_across_pdfs_and_images() {
    let dir = TempDir::new("mergepdf");
    let first = three_page_pdf(&dir, "앞");
    let second = three_page_pdf(&dir, "뒤");
    let photo = dir.path("사진.jpg");
    save(&RgbImage::from_pixel(80, 80, Rgb([20, 200, 20])), &photo, ImageFormat::Jpeg);

    // 사용자가 정한 순서: 사진 → 뒤.pdf → 앞.pdf (이름순과 다르다)
    let inputs = vec![photo, second, first];
    let outcome = job::run(Mode::Merge, inputs, &Progress::default());
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    let merged = dir.path("사진_합본.pdf");
    assert_eq!(outcome.created, vec![merged.clone()]);
    assert_eq!(page_count(&merged).unwrap(), 7);

    let pages = pdf_to_images(&merged, OutputFormat::Png, 72.0, |_, _| true).unwrap();
    assert_eq!(pages.len(), 7);
    assert!(is_greenish(center(&pages[0])), "1쪽 = 사진");
    assert!(is_reddish(center(&pages[1])), "2쪽 = 뒤.pdf 1쪽");
    assert!(is_bluish(center(&pages[2])));
    assert!(is_greenish(center(&pages[3])));
    assert!(is_reddish(center(&pages[4])), "5쪽 = 앞.pdf 1쪽");
    assert!(is_greenish(center(&pages[6])));
}

/// JPEG 맨 앞에 "방향" 정보만 담은 EXIF 블록을 끼워 넣는다.
fn with_exif_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
    let mut exif: Vec<u8> = Vec::new();
    exif.extend_from_slice(b"Exif\0\0");
    exif.extend_from_slice(b"II\x2A\x00\x08\x00\x00\x00"); // TIFF 헤더 (little endian)
    exif.extend_from_slice(&[0x01, 0x00]); // 항목 1개
    exif.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, orientation, 0, 0, 0]);
    exif.extend_from_slice(&[0, 0, 0, 0]); // 다음 IFD 없음
    let length = (exif.len() + 2) as u16;

    let mut out = Vec::with_capacity(jpeg.len() + exif.len() + 4);
    out.extend_from_slice(&jpeg[..2]); // SOI
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&exif);
    out.extend_from_slice(&jpeg[2..]);
    out
}

#[test]
fn phone_photo_rotation_is_respected() {
    let dir = TempDir::new("exif");
    // 저장된 픽셀은 가로(왼쪽 빨강/오른쪽 파랑)지만 "시계 방향 90도 돌려서 보라"는 표시가 붙은 사진.
    let plain = dir.path("plain.jpg");
    save(&red_blue(200, 100), &plain, ImageFormat::Jpeg);
    let rotated = dir.path("세로사진.jpg");
    std::fs::write(&rotated, with_exif_orientation(&std::fs::read(&plain).unwrap(), 6)).unwrap();

    let pdf = dir.path("세로사진.pdf");
    images_to_pdf(&[rotated], &pdf, |_| true).unwrap();
    let outputs = pdf_to_images(&pdf, OutputFormat::Png, 96.0, |_, _| true).unwrap();
    let rendered = image::open(&outputs[0]).unwrap();
    let (w, h) = rendered.dimensions();
    assert!((h as f32 / w as f32 - 2.0).abs() < 0.03, "세로로 서야 한다: {w}x{h}");
    assert!(is_reddish(rendered.get_pixel(w / 2, h / 4)), "위쪽 = 원래 왼쪽(빨강)");
    assert!(is_bluish(rendered.get_pixel(w / 2, h * 3 / 4)), "아래쪽 = 원래 오른쪽(파랑)");
}