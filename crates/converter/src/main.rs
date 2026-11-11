//! ikoPDF.exe — 이미지 ↔ PDF 변환기. 파일 하나로 배포한다.
//!
//! 그냥 실행하면 설정 창이 뜬다(우클릭 메뉴 등록/해제, 업데이트).
//! 우클릭 메뉴는 이 파일을 아래처럼 실행한다.
//!
//!   ikoPDF.exe <to-pdf|merge|to-jpg|to-png|split> [--stdin] [--no-ui] [파일 …]
//!
//! - `--stdin`  : 표준 입력에서 한 줄에 하나씩 파일 경로(UTF-8)를 읽는다.
//! - `--no-ui`  : 창을 띄우지 않고 결과를 표준 오류로만 알린다(테스트용).
//! - `merge`    : 합치기 전에 순서를 정하는 창을 띄운다(`--no-ui` 면 이름순).
//!
//! 관리용:
//!   ikoPDF.exe --register [--classic]   우클릭 메뉴 등록 (--classic: 기존 메뉴로만)
//!   ikoPDF.exe --unregister             우클릭 메뉴 해제
//!   ikoPDF.exe --startup-update         (로그인 때) 조용히 새 버전 확인·설치
//!   ikoPDF.exe --apply-update           (업데이트 과정에서) 자신을 설치 폴더에 반영

#![windows_subsystem = "windows"]
#![allow(non_snake_case)]

mod order_dialog;
mod settings_dialog;
mod setup;
mod update;
mod win;

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ikopdf::job::{self, Mode, Outcome, Progress};
use ikopdf::APP_NAME;
use windows::core::{w, GUID, HSTRING};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::IProgressDialog;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_TOPMOST};

/// CLSID_ProgressDialog
const PROGRESS_DIALOG: GUID = GUID::from_u128(0xF8383852_FCD3_11D1_A6B9_006097DF5BD4);
/// 이 시간 안에 끝나면 진행률 창을 띄우지 않는다.
const PROGRESS_DELAY: Duration = Duration::from_millis(1200);
// IProgressDialog 플래그 (shlobj_core.h)
const PROGDLG_NORMAL: u32 = 0x0000_0000;
const PROGDLG_AUTOTIME: u32 = 0x0000_0002;
const PROGDLG_NOMINIMIZE: u32 = 0x0000_0008;

struct ConvertArgs {
    mode: Mode,
    paths: Vec<PathBuf>,
    no_ui: bool,
}

fn parse_convert_args(mode_text: &str, rest: Vec<std::ffi::OsString>) -> Result<ConvertArgs, String> {
    let mode = Mode::parse(mode_text).ok_or(format!("알 수 없는 작업입니다: {mode_text}"))?;

    let mut paths = Vec::new();
    let mut no_ui = false;
    let mut from_stdin = false;
    for arg in rest {
        match arg.to_str() {
            Some("--stdin") => from_stdin = true,
            Some("--no-ui") => no_ui = true,
            _ => paths.push(PathBuf::from(arg)),
        }
    }

    if from_stdin {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|e| format!("파일 목록을 읽지 못했습니다: {e}"))?;
        let text = String::from_utf8_lossy(&bytes);
        paths.extend(
            text.trim_start_matches('\u{feff}')
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from),
        );
    }

    Ok(ConvertArgs { mode, paths, no_ui })
}

/// Windows 기본 진행률 창. 만들지 못해도 변환은 계속한다.
struct ProgressWindow {
    dialog: IProgressDialog,
    last_line: String,
}

impl ProgressWindow {
    fn open(title: &str) -> Option<ProgressWindow> {
        unsafe {
            let dialog: IProgressDialog =
                CoCreateInstance(&PROGRESS_DIALOG, None, CLSCTX_INPROC_SERVER).ok()?;
            let _ = dialog.SetTitle(&HSTRING::from(format!("{APP_NAME} - {title}")));
            let _ = dialog.SetCancelMsg(w!("취소하는 중…"), None);
            dialog
                .StartProgressDialog(
                    None,
                    None,
                    PROGDLG_NORMAL | PROGDLG_AUTOTIME | PROGDLG_NOMINIMIZE,
                    None,
                )
                .ok()?;
            Some(ProgressWindow {
                dialog,
                last_line: String::new(),
            })
        }
    }

    fn update(&mut self, progress: &Progress) {
        let done = progress.done.load(Ordering::Relaxed);
        let total = progress.total.load(Ordering::Relaxed).max(1);
        let line = progress
            .current
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default();
        unsafe {
            let _ = self.dialog.SetProgress(done.min(total), total);
            if line != self.last_line {
                let _ = self
                    .dialog
                    .SetLine(1, &HSTRING::from(line.as_str()), true, None);
                self.last_line = line;
            }
            if self.dialog.HasUserCancelled().as_bool() {
                progress.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    fn close(self) {
        unsafe {
            let _ = self.dialog.StopProgressDialog();
        }
    }
}

fn summarize_failures(outcome: &Outcome) -> String {
    const MAX_LINES: usize = 12;
    let mut text = String::new();
    if !outcome.created.is_empty() {
        text.push_str(&format!(
            "{}개 파일은 만들었고, 아래 항목은 변환하지 못했습니다.\n\n",
            outcome.created.len()
        ));
    } else {
        text.push_str("변환하지 못했습니다.\n\n");
    }
    for failure in outcome.failures.iter().take(MAX_LINES) {
        if failure.file.is_empty() {
            text.push_str(&format!("{}\n", failure.reason));
        } else {
            text.push_str(&format!("• {}\n   {}\n", failure.file, failure.reason));
        }
    }
    if outcome.failures.len() > MAX_LINES {
        text.push_str(&format!("… 외 {}개", outcome.failures.len() - MAX_LINES));
    }
    text
}

fn convert(args: ConvertArgs) -> i32 {
    let mut inputs = job::prepare_inputs(args.mode, args.paths);
    if args.mode == Mode::Merge && !args.no_ui && !inputs.is_empty() {
        let title = format!("{APP_NAME} - 합칠 순서");
        match order_dialog::choose_order(&title, inputs) {
            Some(ordered) => inputs = ordered,
            None => return 0, // 취소
        }
    }

    let progress = Arc::new(Progress::default());
    let worker = {
        let progress = Arc::clone(&progress);
        let mode = args.mode;
        std::thread::spawn(move || job::run(mode, inputs, &progress))
    };

    let started = Instant::now();
    let mut window: Option<ProgressWindow> = None;
    let mut window_tried = false;
    while !worker.is_finished() {
        std::thread::sleep(Duration::from_millis(80));
        if args.no_ui {
            continue;
        }
        if !window_tried && started.elapsed() >= PROGRESS_DELAY {
            window_tried = true;
            window = ProgressWindow::open(args.mode.title());
        }
        if let Some(window) = window.as_mut() {
            window.update(&progress);
        }
    }
    if let Some(window) = window {
        window.close();
    }

    let outcome = match worker.join() {
        Ok(outcome) => outcome,
        Err(_) => {
            let message = "변환 중 예기치 않은 오류가 발생했습니다.";
            eprintln!("{message}");
            if !args.no_ui {
                win::message_box(None, message, MB_OK | MB_ICONERROR | MB_TOPMOST);
            }
            return 1;
        }
    };

    for file in &outcome.created {
        eprintln!("created: {}", file.display());
    }
    if outcome.failures.is_empty() {
        return 0;
    }
    let summary = summarize_failures(&outcome);
    eprintln!("{summary}");
    if !args.no_ui {
        win::message_box(None, &summary, MB_OK | MB_ICONERROR | MB_TOPMOST);
    }
    1
}

/// 명령줄 관리 명령의 결과를 표준 오류와 종료 코드로 알린다.
fn report(result: anyhow::Result<String>) -> i32 {
    match result {
        Ok(message) => {
            eprintln!("{message}");
            0
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            1
        }
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next().map(|a| a.to_string_lossy().into_owned());
    let rest: Vec<std::ffi::OsString> = args.collect();
    let has_flag = |flag: &str| rest.iter().any(|a| a.to_str() == Some(flag));

    // 창(설정, 순서 조정, 진행률)은 이 스레드(STA)에서 다루고, 변환은 작업 스레드에서 한다.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

    let code = match first.as_deref() {
        None => {
            settings_dialog::show();
            0
        }
        Some("--startup-update") => {
            update::startup_check();
            0
        }
        Some("--apply-update") => report(setup::apply_update().map(|()| "updated".to_string())),
        Some("--register") => report(setup::register_menu(!has_flag("--classic")).map(|registration| {
            match registration.fallback_reason {
                Some(reason) => format!("registered: {:?} (fallback: {reason})", registration.state),
                None => format!("registered: {:?}", registration.state),
            }
        })),
        Some("--unregister") => report(setup::unregister_menu().map(|()| "unregistered".to_string())),
        Some("--status") => report(Ok(format!(
            "version {} / menu {:?} / installed {:?} / autostart {}",
            update::current_version(),
            setup::menu_state(),
            setup::installed_version(),
            setup::autostart_enabled()
        ))),
        Some(mode_text) => match parse_convert_args(mode_text, rest) {
            Ok(args) => convert(args),
            Err(message) => {
                eprintln!("{message}");
                win::message_box(None, &message, MB_OK | MB_ICONINFORMATION | MB_TOPMOST);
                2
            }
        },
    };
    std::process::exit(code);
}
