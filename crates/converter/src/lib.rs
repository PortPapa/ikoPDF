//! 이미지 ↔ PDF 변환 라이브러리.
//!
//! - [`img2pdf`]: 이미지 파일들을 PDF로 묶는다.
//! - [`pdf2img`]: PDF 페이지를 이미지로 렌더링한다 (Windows 내장 렌더러 사용).
//! - [`pdfops`]: PDF를 페이지별로 나누거나 여러 파일을 합친다.
//! - [`job`]: 우클릭 메뉴의 각 항목에 해당하는 작업 단위.

/// 창 제목·메뉴에 쓰는 프로그램 이름.
pub const APP_NAME: &str = "iko PDF 변환";

pub mod img2pdf;
pub mod job;
pub mod naming;
pub mod pdf2img;
pub mod pdfops;

/// 사용자가 진행률 창에서 취소를 눌렀을 때 돌려주는 오류.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("취소되었습니다")
    }
}

impl std::error::Error for Cancelled {}
