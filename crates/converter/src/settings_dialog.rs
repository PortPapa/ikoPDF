//! 설정 창 — 실행 파일을 그냥 실행했을 때(또는 메뉴의 "설정…") 뜬다.
//!
//! 우클릭 메뉴 등록/해제, 로그인 때 자동 업데이트 확인, 지금 업데이트 확인.

use ikopdf::APP_NAME;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, GetSysColorBrush, UpdateWindow, COLOR_3DFACE, HGDIOBJ};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    CheckDlgButton, IsDlgButtonChecked, BST_CHECKED, BST_UNCHECKED, WC_BUTTONW, WC_STATICW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::setup::{self, MenuState};
use crate::{update, win};

const ID_STATUS: i32 = 100;
const ID_TOGGLE: i32 = 101;
const ID_AUTO: i32 = 102;
const ID_UPDATE: i32 = 103;
const ID_UPDATE_STATUS: i32 = 104;
const ID_CLOSE: i32 = 2; // IDCANCEL — Esc 키

const CLIENT_WIDTH: i32 = 500;
const CLIENT_HEIGHT: i32 = 398;

pub fn show() {
    unsafe { run() }
}

unsafe fn set_text(window: HWND, id: i32, text: &str) {
    if let Ok(control) = GetDlgItem(Some(window), id) {
        let _ = SetWindowTextW(control, &HSTRING::from(text));
    }
}

unsafe fn enable(window: HWND, id: i32, enabled: bool) {
    if let Ok(control) = GetDlgItem(Some(window), id) {
        let _ = EnableWindow(control, enabled);
    }
}

/// 오래 걸리는 작업 동안 모래시계 커서를 보여 준다.
unsafe fn busy<T>(window: HWND, work: impl FnOnce() -> T) -> T {
    let previous = SetCursor(LoadCursorW(None, IDC_WAIT).ok());
    let _ = UpdateWindow(window);
    let result = work();
    SetCursor(Some(previous));
    result
}

/// 현재 상태(등록 여부, 자동 실행)를 화면에 반영한다.
unsafe fn refresh(window: HWND) {
    let state = setup::menu_state();
    let (status, toggle) = match state {
        MenuState::Modern => (
            "● 등록됨 — 우클릭 메뉴 첫 화면에 표시됩니다.".to_string(),
            "우클릭 메뉴 해제",
        ),
        MenuState::Classic => (
            "● 등록됨 — Windows 11에서는 우클릭 → \"추가 옵션 표시\" 안에 표시됩니다.".to_string(),
            "우클릭 메뉴 해제",
        ),
        MenuState::NotRegistered => (
            "○ 등록되지 않음 — 아래 버튼을 누르면 우클릭 메뉴에 추가됩니다.".to_string(),
            "우클릭 메뉴 등록",
        ),
    };
    set_text(window, ID_STATUS, &status);
    set_text(window, ID_TOGGLE, toggle);

    let registered = state != MenuState::NotRegistered;
    enable(window, ID_AUTO, registered);
    let checked = if registered && setup::autostart_enabled() {
        BST_CHECKED
    } else {
        BST_UNCHECKED
    };
    let _ = CheckDlgButton(window, ID_AUTO, checked);
}

unsafe fn on_toggle(window: HWND) {
    if setup::menu_state() == MenuState::NotRegistered {
        match busy(window, || setup::register_menu(true)) {
            Ok(registration) => {
                let _ = setup::set_autostart(true);
                refresh(window);
                let message = match (registration.state, registration.fallback_reason) {
                    (MenuState::Classic, Some(reason)) => format!(
                        "우클릭 메뉴에 등록했습니다.\n\n이 PC에서는 새 메뉴에 올릴 수 없어서 \
                         \"추가 옵션 표시\" 안의 기존 메뉴로 등록했습니다.\n(사유: {reason})"
                    ),
                    _ => "우클릭 메뉴에 등록했습니다.\n\n이미지나 PDF 파일을 우클릭해 보세요."
                        .to_string(),
                };
                win::message_box(Some(window), &message, MB_OK | MB_ICONINFORMATION);
            }
            Err(error) => {
                refresh(window);
                win::message_box(
                    Some(window),
                    &format!("등록하지 못했습니다.\n\n{error:#}"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        return;
    }

    let from_install_dir = setup::running_from_install_dir();
    let result = busy(window, setup::unregister_menu);
    match result {
        Ok(()) => {
            win::message_box(
                Some(window),
                "우클릭 메뉴를 해제하고 설치된 파일을 지웠습니다.",
                MB_OK | MB_ICONINFORMATION,
            );
        }
        Err(error) => {
            win::message_box(
                Some(window),
                &format!("일부를 해제하지 못했습니다.\n\n{error:#}"),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
    if from_install_dir {
        // 지금 실행 중인 파일이 곧 지워지므로 창을 닫는다.
        let _ = DestroyWindow(window);
    } else {
        refresh(window);
    }
}

unsafe fn on_update(window: HWND) {
    set_text(window, ID_UPDATE_STATUS, "확인하는 중…");
    let latest = match busy(window, update::latest_version) {
        Ok(latest) => latest,
        Err(error) => {
            set_text(window, ID_UPDATE_STATUS, &format!("확인 실패: {error:#}"));
            return;
        }
    };

    let installed = setup::installed_version();
    let baseline = installed.clone().unwrap_or_else(|| update::current_version().to_string());
    if !update::is_newer(&latest, &baseline) {
        set_text(window, ID_UPDATE_STATUS, &format!("최신 버전입니다 (v{baseline})."));
        return;
    }

    if installed.is_none() {
        // 설치(메뉴 등록) 전이면 바꿀 대상이 없으므로 내려받는 곳만 안내한다.
        set_text(window, ID_UPDATE_STATUS, &format!("새 버전 v{latest} 이 있습니다."));
        let answer = win::message_box(
            Some(window),
            &format!("새 버전 v{latest} 이 있습니다.\n내려받는 페이지를 열까요?"),
            MB_YESNO | MB_ICONQUESTION,
        );
        if answer == IDYES {
            ShellExecuteW(
                Some(window),
                w!("open"),
                &HSTRING::from(update::releases_page()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
        return;
    }

    set_text(window, ID_UPDATE_STATUS, &format!("v{latest} 내려받는 중…"));
    match busy(window, update::download_and_apply) {
        Ok(()) => set_text(window, ID_UPDATE_STATUS, &format!("v{latest} 로 업데이트했습니다.")),
        Err(error) => set_text(window, ID_UPDATE_STATUS, &format!("업데이트 실패: {error:#}")),
    }
    refresh(window);
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as i32 {
                ID_TOGGLE => on_toggle(window),
                ID_AUTO => {
                    let wanted = IsDlgButtonChecked(window, ID_AUTO) == BST_CHECKED.0;
                    if let Err(error) = setup::set_autostart(wanted) {
                        win::message_box(Some(window), &format!("{error:#}"), MB_OK | MB_ICONERROR);
                    }
                    refresh(window);
                }
                ID_UPDATE => on_update(window),
                ID_CLOSE => {
                    let _ = DestroyWindow(window);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

unsafe fn run() {
    let Ok(module) = GetModuleHandleW(None) else { return };
    let instance: HINSTANCE = module.into();
    let class_name = w!("IkoPdfSettings");
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        hbrBackground: GetSysColorBrush(COLOR_3DFACE),
        lpszClassName: class_name,
        ..Default::default()
    };
    RegisterClassExW(&class);

    let dpi = win::system_dpi();
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let ex_style = WINDOW_EX_STYLE(0);
    let (x, y, width, height) = win::centered_frame(CLIENT_WIDTH, CLIENT_HEIGHT, dpi, style, ex_style);
    let Ok(window) = CreateWindowExW(
        ex_style,
        class_name,
        &HSTRING::from(format!("{APP_NAME} 설정")),
        style,
        x,
        y,
        width,
        height,
        None,
        None,
        Some(instance),
        None,
    ) else {
        return;
    };

    let font = win::message_font();
    let tab = WS_TABSTOP.0;
    let none = WINDOW_EX_STYLE(0);
    let control = |class: PCWSTR, text: &str, style: u32, rect, id| {
        win::add_control(window, instance, font, dpi, class, text, style, none, rect, id)
    };

    control(
        WC_STATICW,
        &format!("{APP_NAME}   버전 {}", update::current_version()),
        0,
        (16, 14, 468, 22),
        -1,
    );
    control(
        WC_STATICW,
        "탐색기에서 이미지나 PDF 파일을 우클릭하면 변환·나누기·합치기 메뉴가 나옵니다.",
        0,
        (16, 40, 468, 38),
        -1,
    );

    control(WC_STATICW, "우클릭 메뉴", 0, (16, 90, 468, 20), -1);
    control(WC_STATICW, "", 0, (16, 112, 468, 38), ID_STATUS);
    control(
        WC_BUTTONW,
        "우클릭 메뉴 등록",
        tab | BS_DEFPUSHBUTTON as u32,
        (16, 154, 190, 32),
        ID_TOGGLE,
    );
    control(
        WC_STATICW,
        "등록하면 프로그램이 내 계정 폴더로 복사되고, 새 우클릭 메뉴에 표시하기 위한 인증서가 \
         내 Windows 계정에 추가됩니다. 해제하면 모두 지워집니다. 관리자 권한은 쓰지 않습니다.",
        0,
        (16, 196, 468, 54),
        -1,
    );

    control(WC_STATICW, "업데이트", 0, (16, 262, 468, 20), -1);
    control(
        WC_BUTTONW,
        "Windows 시작 시 자동으로 업데이트 확인",
        tab | BS_AUTOCHECKBOX as u32,
        (16, 284, 468, 24),
        ID_AUTO,
    );
    control(WC_BUTTONW, "지금 업데이트 확인", tab, (16, 314, 190, 32), ID_UPDATE);
    control(WC_STATICW, "", 0, (216, 321, 268, 22), ID_UPDATE_STATUS);

    control(WC_BUTTONW, "닫기", tab, (364, 354, 120, 32), ID_CLOSE);

    setup::clean_leftovers();
    refresh(window);
    win::show_in_front(window);
    win::run_modal_loop(window);

    if !font.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
}
