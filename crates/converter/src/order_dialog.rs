//! 합치기 전에 파일 순서를 정하는 창.
//!
//! 목록에서 항목을 끌어서 옮기거나 오른쪽 버튼(맨 위로/위로/아래로/맨 아래로)으로
//! 순서를 바꾼 뒤 "합치기"를 누른다. Delete 키나 "목록에서 빼기"로 항목을 뺄 수 있다.

use std::path::{Path, PathBuf};

use ikopdf::naming::sort_natural;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, GetSysColorBrush, InvalidateRect, COLOR_3DFACE, HGDIOBJ};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    DrawInsert, LBItemFromPt, MakeDragList, DL_BEGINDRAG, DL_CANCELDRAG, DL_DRAGGING, DL_DROPPED,
    DL_MOVECURSOR, DL_STOPCURSOR, DRAGLISTINFO, DRAGLISTMSGSTRING, WC_BUTTONW, WC_LISTBOXW,
    WC_STATICW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_DELETE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::win;

const ID_LIST: i32 = 100;
const ID_TOP: i32 = 101;
const ID_UP: i32 = 102;
const ID_DOWN: i32 = 103;
const ID_BOTTOM: i32 = 104;
const ID_SORT: i32 = 105;
const ID_REMOVE: i32 = 106;
const ID_COUNT: i32 = 107;
const ID_OK: i32 = 1; // IDOK — Enter 키
const ID_CANCEL: i32 = 2; // IDCANCEL — Esc 키

const CLIENT_WIDTH: i32 = 580;
const CLIENT_HEIGHT: i32 = 440;

struct State {
    items: Vec<PathBuf>,
    list: HWND,
    accepted: bool,
    drag_message: u32,
    drag_from: i32,
}

/// 순서 조정 창을 띄운다. "합치기"를 누르면 정한 순서를, 취소하면 `None`을 돌려준다.
pub fn choose_order(title: &str, items: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    unsafe { run_dialog(title, items) }
}

fn display_name(index: usize, path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    format!("{:>3}.   {}", index + 1, name)
}

unsafe fn state_of<'a>(window: HWND) -> Option<&'a mut State> {
    let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut State;
    pointer.as_mut()
}

/// 목록 상자를 `state.items` 내용으로 다시 채우고 `select` 번째를 고른다.
unsafe fn refresh(window: HWND, state: &State, select: i32) {
    let list = state.list;
    SendMessageW(list, WM_SETREDRAW, Some(WPARAM(0)), None);
    SendMessageW(list, LB_RESETCONTENT, None, None);
    for (index, path) in state.items.iter().enumerate() {
        let text = HSTRING::from(display_name(index, path));
        SendMessageW(list, LB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
    }
    if !state.items.is_empty() {
        let select = select.clamp(0, state.items.len() as i32 - 1);
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(select as usize)), None);
    }
    SendMessageW(list, WM_SETREDRAW, Some(WPARAM(1)), None);
    let _ = InvalidateRect(Some(list), None, true);

    if let Ok(counter) = GetDlgItem(Some(window), ID_COUNT) {
        let _ = SetWindowTextW(counter, &HSTRING::from(format!("총 {}개", state.items.len())));
    }
    if let Ok(ok_button) = GetDlgItem(Some(window), ID_OK) {
        let _ = EnableWindow(ok_button, !state.items.is_empty());
    }
}

unsafe fn selected_index(state: &State) -> Option<usize> {
    let index = SendMessageW(state.list, LB_GETCURSEL, None, None).0;
    if index >= 0 && (index as usize) < state.items.len() {
        Some(index as usize)
    } else {
        None
    }
}

unsafe fn move_selected(window: HWND, state: &mut State, target: impl Fn(usize, usize) -> usize) {
    let Some(from) = selected_index(state) else { return };
    let to = target(from, state.items.len());
    if to != from {
        let item = state.items.remove(from);
        state.items.insert(to, item);
    }
    refresh(window, state, to as i32);
}

unsafe fn remove_selected(window: HWND, state: &mut State) {
    if let Some(index) = selected_index(state) {
        state.items.remove(index);
        refresh(window, state, index as i32);
    }
}

unsafe fn on_command(window: HWND, state: &mut State, id: i32) {
    match id {
        ID_TOP => move_selected(window, state, |_, _| 0),
        ID_UP => move_selected(window, state, |from, _| from.saturating_sub(1)),
        ID_DOWN => move_selected(window, state, |from, count| (from + 1).min(count - 1)),
        ID_BOTTOM => move_selected(window, state, |_, count| count - 1),
        ID_SORT => {
            sort_natural(&mut state.items);
            refresh(window, state, 0);
        }
        ID_REMOVE => remove_selected(window, state),
        ID_OK => {
            // 목록이 비면 "합치기" 버튼이 꺼져 있지만 Enter 키로도 들어올 수 있다.
            if !state.items.is_empty() {
                state.accepted = true;
                let _ = DestroyWindow(window);
            }
        }
        ID_CANCEL => {
            let _ = DestroyWindow(window);
        }
        _ => {}
    }
}

/// 끌어서 옮기기 알림 처리. 반환값은 목록 상자가 그대로 사용한다.
unsafe fn on_drag(window: HWND, state: &mut State, info: &DRAGLISTINFO) -> LRESULT {
    let list = state.list;
    match info.uNotification {
        DL_BEGINDRAG => {
            state.drag_from = LBItemFromPt(list, info.ptCursor, false);
            LRESULT((state.drag_from >= 0) as isize)
        }
        DL_DRAGGING => {
            let over = LBItemFromPt(list, info.ptCursor, true);
            DrawInsert(window, list, over);
            LRESULT(if over >= 0 { DL_MOVECURSOR } else { DL_STOPCURSOR } as isize)
        }
        DL_CANCELDRAG => {
            DrawInsert(window, list, -1);
            state.drag_from = -1;
            LRESULT(0)
        }
        DL_DROPPED => {
            DrawInsert(window, list, -1);
            let to = LBItemFromPt(list, info.ptCursor, false);
            let from = state.drag_from;
            state.drag_from = -1;
            let count = state.items.len() as i32;
            if from >= 0 && from < count && to >= 0 && to < count && from != to {
                let item = state.items.remove(from as usize);
                state.items.insert(to as usize, item);
                refresh(window, state, to);
            }
            LRESULT(0)
        }
        _ => LRESULT(0),
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(state) = state_of(window) {
        if message == state.drag_message && state.drag_message != 0 {
            let info = &*(lparam.0 as *const DRAGLISTINFO);
            return on_drag(window, state, info);
        }
        match message {
            WM_COMMAND => {
                on_command(window, state, (wparam.0 & 0xFFFF) as i32);
                return LRESULT(0);
            }
            WM_VKEYTOITEM => {
                // 목록에서 Delete 키 → 항목 빼기
                if (wparam.0 & 0xFFFF) as u16 == VK_DELETE.0 {
                    remove_selected(window, state);
                    return LRESULT(-2);
                }
                return LRESULT(-1);
            }
            _ => {}
        }
    }
    match message {
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

unsafe fn run_dialog(title: &str, items: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
    let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
    let class_name = w!("IkoPdfOrderDialog");
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
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
    let ex_style = WS_EX_DLGMODALFRAME;
    let (x, y, width, height) = win::centered_frame(CLIENT_WIDTH, CLIENT_HEIGHT, dpi, style, ex_style);
    let window = CreateWindowExW(
        ex_style,
        class_name,
        &HSTRING::from(title),
        style,
        x,
        y,
        width,
        height,
        None,
        None,
        Some(instance),
        None,
    )
    .ok()?;

    let font = win::message_font();
    let tab = WS_TABSTOP.0;
    let control = |class: PCWSTR, text: &str, style: u32, ex: WINDOW_EX_STYLE, rect, id| {
        win::add_control(window, instance, font, dpi, class, text, style, ex, rect, id)
    };

    control(
        WC_STATICW,
        "위에서부터 순서대로 합쳐집니다.\n항목을 끌어서 옮기거나 오른쪽 버튼으로 순서를 바꾸세요.",
        0,
        WINDOW_EX_STYLE(0),
        (14, 12, 552, 40),
        -1,
    );
    let list = control(
        WC_LISTBOXW,
        "",
        tab | WS_VSCROLL.0 | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | LBS_WANTKEYBOARDINPUT) as u32,
        WS_EX_CLIENTEDGE,
        (14, 58, 410, 322),
        ID_LIST,
    );
    let buttons = [
        ("맨 위로", ID_TOP, 58),
        ("위로", ID_UP, 94),
        ("아래로", ID_DOWN, 130),
        ("맨 아래로", ID_BOTTOM, 166),
        ("이름순 정렬", ID_SORT, 218),
        ("목록에서 빼기", ID_REMOVE, 254),
    ];
    for (text, id, top) in buttons {
        control(WC_BUTTONW, text, tab, WINDOW_EX_STYLE(0), (436, top, 130, 30), id);
    }
    control(WC_STATICW, "", 0, WINDOW_EX_STYLE(0), (14, 398, 200, 24), ID_COUNT);
    control(
        WC_BUTTONW,
        "합치기",
        tab | BS_DEFPUSHBUTTON as u32,
        WINDOW_EX_STYLE(0),
        (300, 392, 126, 34),
        ID_OK,
    );
    control(WC_BUTTONW, "취소", tab, WINDOW_EX_STYLE(0), (436, 392, 130, 34), ID_CANCEL);

    let mut state = Box::new(State {
        items,
        list,
        accepted: false,
        drag_message: 0,
        drag_from: -1,
    });
    if MakeDragList(list).as_bool() {
        state.drag_message = RegisterWindowMessageW(DRAGLISTMSGSTRING);
    }
    SetWindowLongPtrW(window, GWLP_USERDATA, &mut *state as *mut State as isize);
    refresh(window, &state, 0);

    win::show_in_front(window);
    let _ = SetFocus(Some(list));
    win::run_modal_loop(window);

    // 창은 이미 없어졌다. 상태를 회수한다.
    if !font.is_invalid() {
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
    let state = *state;
    if state.accepted {
        Some(state.items)
    } else {
        None
    }
}
