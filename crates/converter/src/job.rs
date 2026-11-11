//! 우클릭 메뉴 항목 하나가 하는 일.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use crate::naming::{is_image, is_pdf, sort_natural, stem, unique_path};
use crate::pdf2img::{pdf_to_images, OutputFormat, DEFAULT_DPI};
use crate::{img2pdf, pdfops, Cancelled};

/// 진행률 한 파일 분량.
const UNITS_PER_FILE: u32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 이미지마다 PDF 하나씩.
    ToPdf,
    /// 선택한 이미지·PDF를 한 PDF로.
    Merge,
    /// PDF 페이지를 JPG로.
    ToJpg,
    /// PDF 페이지를 PNG로.
    ToPng,
    /// PDF를 페이지마다 하나의 PDF로.
    Split,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "to-pdf" => Some(Mode::ToPdf),
            "merge" => Some(Mode::Merge),
            "to-jpg" => Some(Mode::ToJpg),
            "to-png" => Some(Mode::ToPng),
            "split" => Some(Mode::Split),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Mode::ToPdf => "PDF로 변환",
            Mode::Merge => "하나의 PDF로 합치기",
            Mode::ToJpg => "JPG로 변환",
            Mode::ToPng => "PNG로 변환",
            Mode::Split => "페이지별로 나누기",
        }
    }

    fn accepts(self, path: &Path) -> bool {
        match self {
            Mode::ToPdf => is_image(path),
            Mode::Merge => is_image(path) || is_pdf(path),
            Mode::ToJpg | Mode::ToPng | Mode::Split => is_pdf(path),
        }
    }
}

/// 선택한 파일 중 이 작업이 다룰 수 있는 것만 골라 이름순(자연 정렬)으로 늘어놓는다.
pub fn prepare_inputs(mode: Mode, paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut inputs: Vec<PathBuf> = paths.into_iter().filter(|p| mode.accepts(p)).collect();
    sort_natural(&mut inputs);
    inputs
}

/// 작업 스레드와 화면(진행률 창) 사이에 공유하는 상태.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU32,
    pub total: AtomicU32,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

impl Progress {
    fn set(&self, done: u32, current: &str) {
        self.done.store(done, Ordering::Relaxed);
        if let Ok(mut text) = self.current.lock() {
            if *text != current {
                *text = current.to_string();
            }
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// 변환에 실패한 파일과 이유.
#[derive(Debug)]
pub struct Failure {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// 새로 만든 파일들.
    pub created: Vec<PathBuf>,
    pub failures: Vec<Failure>,
    pub cancelled: bool,
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// 오류 사슬을 한 줄로 (`바깥 설명: 안쪽 원인`).
fn describe(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

/// `inputs` 를 주어진 순서 그대로 처리한다([`prepare_inputs`] 로 고른 목록을 넘길 것).
pub fn run(mode: Mode, inputs: Vec<PathBuf>, progress: &Progress) -> Outcome {
    let mut outcome = Outcome::default();
    let inputs: Vec<PathBuf> = inputs.into_iter().filter(|p| mode.accepts(p)).collect();

    if inputs.is_empty() {
        outcome.failures.push(Failure {
            file: String::new(),
            reason: match mode {
                Mode::ToPdf => "선택한 파일 중에 이미지가 없습니다.",
                Mode::Merge => "선택한 파일 중에 이미지나 PDF가 없습니다.",
                Mode::ToJpg | Mode::ToPng | Mode::Split => "선택한 파일 중에 PDF가 없습니다.",
            }
            .to_string(),
        });
        return outcome;
    }

    let count = inputs.len() as u32;
    progress
        .total
        .store(count.saturating_mul(UNITS_PER_FILE), Ordering::Relaxed);

    match mode {
        Mode::Merge => {
            let first = &inputs[0];
            let parent = first.parent().unwrap_or_else(|| Path::new("."));
            let output = unique_path(&parent.join(format!("{}_합본.pdf", stem(first))));
            let on_item = |index: usize| {
                progress.set(index as u32 * UNITS_PER_FILE, &file_name(&inputs[index]));
                !progress.cancelled()
            };
            // 이미지뿐이면 바로 PDF를 만들고, PDF가 섞여 있으면 페이지를 옮겨 붙인다.
            let result = if inputs.iter().all(|p| is_image(p)) {
                img2pdf::images_to_pdf(&inputs, &output, on_item)
            } else {
                pdfops::merge_to_pdf(&inputs, &output, on_item)
            };
            match result {
                Ok(()) => outcome.created.push(output),
                Err(e) if e.is::<Cancelled>() => outcome.cancelled = true,
                Err(e) => outcome.failures.push(Failure {
                    file: file_name(&output),
                    reason: describe(&e),
                }),
            }
        }
        Mode::ToPdf => {
            for (index, input) in inputs.iter().enumerate() {
                if progress.cancelled() {
                    outcome.cancelled = true;
                    break;
                }
                progress.set(index as u32 * UNITS_PER_FILE, &file_name(input));
                let output = unique_path(&input.with_extension("pdf"));
                match img2pdf::images_to_pdf(std::slice::from_ref(input), &output, |_| true) {
                    Ok(()) => outcome.created.push(output),
                    Err(e) => outcome.failures.push(Failure {
                        file: file_name(input),
                        reason: describe(&e),
                    }),
                }
            }
        }
        Mode::ToJpg | Mode::ToPng | Mode::Split => {
            for (index, input) in inputs.iter().enumerate() {
                if progress.cancelled() {
                    outcome.cancelled = true;
                    break;
                }
                let name = file_name(input);
                let base = index as u32 * UNITS_PER_FILE;
                progress.set(base, &name);
                let on_page = |page: u32, total: u32| {
                    let within = (page as u64 * UNITS_PER_FILE as u64 / total.max(1) as u64) as u32;
                    progress.set(base + within, &format!("{name} ({}/{total}쪽)", page + 1));
                    !progress.cancelled()
                };
                let result = match mode {
                    Mode::ToJpg => pdf_to_images(input, OutputFormat::Jpg, DEFAULT_DPI, on_page),
                    Mode::ToPng => pdf_to_images(input, OutputFormat::Png, DEFAULT_DPI, on_page),
                    _ => pdfops::split_pdf(input, on_page),
                };
                match result {
                    Ok(files) => outcome.created.extend(files),
                    Err(e) if e.is::<Cancelled>() => {
                        outcome.cancelled = true;
                        break;
                    }
                    Err(e) => outcome.failures.push(Failure {
                        file: name,
                        reason: describe(&e),
                    }),
                }
            }
        }
    }

    progress
        .done
        .store(count.saturating_mul(UNITS_PER_FILE), Ordering::Relaxed);
    outcome
}
