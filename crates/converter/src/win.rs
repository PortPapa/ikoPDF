//! 창을 만들 때 공통으로 쓰는 Win32 도우미.

use std::ffi::c_void;

use ikopdf::APP_NAME;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, GetMonitorInfoW, MonitorFromPoint, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 96 DPI 기준 길이를 현재 화면 배율에 맞춘다.
pub fn scale(value: i32, dpi: u32) -> i32 {
    (value as i64 * dpi as i64 / 96) as i32
}

/// 실행 파일에 들어 있는 프로그램 아이콘 (build.rs 가 넣는다). 없으면 기본 아이콘.
pub unsafe fn app_icon(instance: HINSTANCE) -> HICON {
    LoadIconW(Some(instance), PCWSTR(1 as *const u16)).unwrap_or_default()
}

pub fn system_dpi() -> u32 {
    unsafe { GetDpiForSystem() }.max(96)
}

/// 시스템 메시지 상자와 같은 글꼴.
pub unsafe fn message_font() -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let ok = SystemParametersInfoW(
        SPI_GETNONCLIENTMETRICS,
        metrics.cbSize,
        Some(&mut metrics as *mut _ as *mut c_void),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    );
    if ok.is_ok() {
        CreateFontIndirectW(&metrics.lfMessageFont)
    } else {
        HFONT::default()
    }
}

/// 자식 컨트롤을 하나 만든다. `rect` 는 96 DPI 기준 (x, y, 너비, 높이).
#[allow(clippy::too_many_arguments)]
pub unsafe fn add_control(
    parent: HWND,
    instance: HINSTANCE,
    font: HFONT,
    dpi: u32,
    class: PCWSTR,
    text: &str,
    style: u32,
    ex_style: WINDOW_EX_STYLE,
    rect: (i32, i32, i32, i32),
    id: i32,
) -> HWND {
    let (x, y, width, height) = rect;
    let window = CreateWindowExW(
        ex_style,
        class,
        &HSTRING::from(text),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
        scale(x, dpi),
        scale(y, dpi),
        scale(width, dpi),
        scale(height, dpi),
        Some(parent),
        Some(HMENU(id as isize as *mut c_void)),
        Some(instance),
        None,
    )
    .unwrap_or_default();
    SendMessageW(
        window,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    window
}

/// 안쪽 크기(96 DPI 기준)에 맞는 창 전체 크기와, 마우스가 있는 모니터 가운데 위치.
pub unsafe fn centered_frame(
    client_width: i32,
    client_height: i32,
    dpi: u32,
    style: WINDOW_STYLE,
    ex_style: WINDOW_EX_STYLE,
) -> (i32, i32, i32, i32) {
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: scale(client_width, dpi),
        bottom: scale(client_height, dpi),
    };
    let _ = AdjustWindowRectEx(&mut frame, style, false, ex_style);
    let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);

    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    let mut monitor = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let found = GetMonitorInfoW(
        MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST),
        &mut monitor,
    )
    .as_bool();
    if found {
        let work = monitor.rcWork;
        (
            work.left + ((work.right - work.left) - width) / 2,
            work.top + ((work.bottom - work.top) - height) / 2,
            width,
            height,
        )
    } else {
        (CW_USEDEFAULT, CW_USEDEFAULT, width, height)
    }
}

/// 창을 보이고, 탐색기 뒤에 숨지 않도록 앞으로 끌어온다.
pub unsafe fn show_in_front(window: HWND) {
    let _ = ShowWindow(window, SW_SHOW);
    let _ = SetWindowPos(window, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    let _ = SetWindowPos(window, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    let _ = SetForegroundWindow(window);
}

/// 창이 닫힐 때까지 메시지를 처리한다(Tab·Enter·Esc 키 포함).
pub unsafe fn run_modal_loop(window: HWND) {
    let mut message = MSG::default();
    while GetMessageW(&mut message, None, 0, 0).0 > 0 {
        if !IsDialogMessageW(window, &message).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

pub fn message_box(owner: Option<HWND>, text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    unsafe {
        MessageBoxW(
            owner,
            &HSTRING::from(text),
            &HSTRING::from(APP_NAME),
            MB_SETFOREGROUND | style,
        )
    }
}
