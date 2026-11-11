//! PDF → 이미지. Windows에 내장된 PDF 렌더러(`Windows.Data.Pdf`)를 쓴다.
//!
//! - 1페이지짜리 PDF: `문서.jpg` 를 PDF 옆에 만든다.
//! - 여러 페이지: `문서\문서_001.jpg …` 처럼 하위 폴더에 모은다.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use windows::core::HSTRING;
use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::StorageFile;
use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::naming::{output_folder, stem, unique_path};
use crate::Cancelled;

/// 기본 렌더링 해상도.
pub const DEFAULT_DPI: f32 = 200.0;
/// PDF 페이지 크기 단위(1/96 인치).
const PDF_UNITS_PER_INCH: f32 = 96.0;
/// 한 변의 최대 픽셀 수 (도면처럼 아주 큰 페이지에서 메모리가 터지지 않도록).
const MAX_SIDE_PIXELS: f32 = 12000.0;

const HRESULT_WRONG_PASSWORD: i32 = 0x8007_052Bu32 as i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Jpg,
    Png,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Jpg => "jpg",
            OutputFormat::Png => "png",
        }
    }
}

fn open_document(pdf: &Path) -> Result<PdfDocument> {
    // 이미 초기화된 스레드면 실패 코드를 돌려주지만 그대로 써도 된다.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

    let absolute = std::path::absolute(pdf).context("경로를 확인할 수 없습니다")?;
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(absolute.as_os_str()))
        .and_then(|op| op.join())
        .map_err(|e| anyhow!("파일을 열 수 없습니다: {}", e.message()))?;
    PdfDocument::LoadFromFileAsync(&file)
        .and_then(|op| op.join())
        .map_err(|e| {
            if e.code().0 == HRESULT_WRONG_PASSWORD {
                anyhow!("암호가 걸린 PDF는 변환할 수 없습니다")
            } else {
                anyhow!("PDF를 열 수 없습니다(손상되었거나 지원하지 않는 형식): {}", e.message())
            }
        })
}

/// PDF의 페이지 수.
pub fn page_count(pdf: &Path) -> Result<u32> {
    Ok(open_document(pdf)?.PageCount()?)
}

/// PDF의 모든 페이지를 이미지로 저장하고, 만든 파일 경로들을 돌려준다.
///
/// `on_page(i, total)`은 i번째(0부터) 페이지를 그리기 직전에 호출되며 `false`면 취소한다.
pub fn pdf_to_images(
    pdf: &Path,
    format: OutputFormat,
    dpi: f32,
    mut on_page: impl FnMut(u32, u32) -> bool,
) -> Result<Vec<PathBuf>> {
    let document = open_document(pdf)?;
    let total = document.PageCount()?;
    if total == 0 {
        bail!("페이지가 없는 PDF입니다");
    }

    let parent = pdf.parent().unwrap_or_else(|| Path::new("."));
    let name = stem(pdf);
    let ext = format.extension();

    // 여러 페이지면 PDF 이름으로 폴더를 만들어 그 안에 모은다.
    let output_dir = if total > 1 {
        Some(output_folder(parent, &name).context("결과 폴더를 만들 수 없습니다")?)
    } else {
        None
    };
    let digits = total.to_string().len().max(3);

    let mut outputs = Vec::with_capacity(total as usize);
    for index in 0..total {
        if !on_page(index, total) {
            return Err(Cancelled.into());
        }
        let bytes = render_page(&document, index, format, dpi)
            .with_context(|| format!("{}쪽을 변환하지 못했습니다", index + 1))?;
        let target = match &output_dir {
            Some(dir) => unique_path(&dir.join(format!("{name}_{:0digits$}.{ext}", index + 1))),
            None => unique_path(&parent.join(format!("{name}.{ext}"))),
        };
        std::fs::write(&target, bytes).context("이미지 파일을 저장할 수 없습니다")?;
        outputs.push(target);
    }
    Ok(outputs)
}

fn render_page(document: &PdfDocument, index: u32, format: OutputFormat, dpi: f32) -> Result<Vec<u8>> {
    let page = document.GetPage(index)?;
    let size = page.Size()?;
    if size.Width <= 0.0 || size.Height <= 0.0 {
        bail!("페이지 크기가 올바르지 않습니다");
    }

    let mut scale = dpi / PDF_UNITS_PER_INCH;
    let longest = size.Width.max(size.Height) * scale;
    if longest > MAX_SIDE_PIXELS {
        scale *= MAX_SIDE_PIXELS / longest;
    }
    let width = (size.Width * scale).round().max(1.0) as u32;
    let height = (size.Height * scale).round().max(1.0) as u32;

    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(width)?;
    options.SetDestinationHeight(height)?;
    let encoder = match format {
        OutputFormat::Jpg => BitmapEncoder::JpegEncoderId()?,
        OutputFormat::Png => BitmapEncoder::PngEncoderId()?,
    };
    options.SetBitmapEncoderId(encoder)?;

    let stream = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&stream, &options)?.join()?;

    let length = u32::try_from(stream.Size()?).context("이미지가 너무 큽니다")?;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(length)?.join()?;
    let mut bytes = vec![0u8; length as usize];
    reader.ReadBytes(&mut bytes)?;

    let _ = reader.Close();
    let _ = stream.Close();
    let _ = page.Close();
    Ok(bytes)
}
