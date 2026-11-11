//! 탐색기 우클릭 메뉴 확장 (`IExplorerCommand`).
//!
//! "iko PDF 변환" 이라는 상위 항목 아래에 하위 메뉴를 보여 주고, 고르면 같은 폴더의
//! `ikoPDF.exe` 를 실행한다. 이 DLL은 탐색기 쪽 프로세스에서 돌기 때문에 변환 작업은
//! 전혀 하지 않고, 선택한 파일 목록만 넘긴다.
//!
//! 두 가지 방식으로 등록될 수 있다(등록은 ikoPDF.exe 가 한다).
//! - Windows 11 새 메뉴: 패키지(AppxManifest)의 COM 대리 서버로 로드된다.
//! - 기존 메뉴("추가 옵션 표시"): 레지스트리의 `ExplorerCommandHandler` 로 탐색기 안에 로드된다.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use windows::core::*;
use windows::Data::Pdf::PdfDocument;
use windows::Storage::StorageFile;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::Shell::*;

/// AppxManifest.xml 의 `com:Class Id` / `desktop5:Verb Clsid`, 그리고 ikoPDF.exe 의
/// 레지스트리 등록 코드와 같아야 한다.
const CLSID_IKOPDF_MENU: GUID = GUID::from_u128(0x7B3F0C52_9A4E_4D1B_8E6A_2C5D91F4A7E3);

const EXE_NAME: &str = "ikoPDF.exe";
const DLL_PROCESS_ATTACH: u32 = 1;
const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "bmp", "gif", "tif", "tiff", "webp",
];
/// 쪽수를 미리 세어 보는 PDF 개수 한도와 기다리는 시간(메뉴가 굼떠지지 않도록).
const PAGE_CHECK_MAX_FILES: usize = 3;
const PAGE_CHECK_TIMEOUT: Duration = Duration::from_millis(400);

static DLL_MODULE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

/// 이 DLL 안의 오류(panic)가 탐색기까지 번지지 않게 막는다.
fn guarded<T>(body: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| Err(E_FAIL.into()))
}

// ───────────────────────── 선택한 파일 ─────────────────────────

#[derive(Clone, Default)]
struct Selection {
    paths: Vec<String>,
    images: usize,
    pdfs: usize,
}

impl Selection {
    fn pdf_paths(&self) -> Vec<String> {
        self.paths
            .iter()
            .filter(|path| extension_lower(path) == "pdf")
            .cloned()
            .collect()
    }
}

/// 상위 항목과 하위 항목들이 함께 쓰는 상태.
#[derive(Default)]
struct Shared {
    /// 마지막으로 본 선택 항목. 하위 항목이 선택 정보를 못 받는 경우의 대비용.
    selection: Selection,
    /// (PDF 경로들, 전부 1쪽짜리인지) — 같은 선택에 대해 다시 세지 않기 위한 기억.
    single_page: Option<(Vec<String>, bool)>,
}

fn extension_lower(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// 탐색기가 넘겨준 선택 항목에서 파일 경로를 뽑는다.
fn read_selection(items: Option<&IShellItemArray>) -> Option<Selection> {
    let items = items?;
    let mut selection = Selection::default();
    unsafe {
        let count = items.GetCount().ok()?;
        for index in 0..count {
            let Ok(item) = items.GetItemAt(index) else { continue };
            let Ok(raw) = item.GetDisplayName(SIGDN_FILESYSPATH) else { continue };
            let path = raw.to_string().unwrap_or_default();
            CoTaskMemFree(Some(raw.0 as *const c_void));
            if path.is_empty() {
                continue;
            }
            let ext = extension_lower(&path);
            if ext == "pdf" {
                selection.pdfs += 1;
            } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
                selection.images += 1;
            }
            selection.paths.push(path);
        }
    }
    Some(selection)
}

// ───────────────────────── PDF 쪽수 확인 ─────────────────────────

fn pdf_page_count(path: &str) -> Option<u32> {
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))
        .ok()?
        .join()
        .ok()?;
    let document = PdfDocument::LoadFromFileAsync(&file).ok()?.join().ok()?;
    document.PageCount().ok()
}

/// 주어진 PDF가 전부 1쪽짜리인지. 확인이 오래 걸리거나 열 수 없으면 `false`(=나눌 수 있다고 본다).
fn all_single_page(pdfs: Vec<String>) -> bool {
    if pdfs.is_empty() || pdfs.len() > PAGE_CHECK_MAX_FILES {
        return false;
    }
    let (sender, receiver) = mpsc::channel();
    let spawned = std::thread::Builder::new().spawn(move || {
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let result = pdfs.iter().all(|path| pdf_page_count(path) == Some(1));
        let _ = sender.send(result);
    });
    if spawned.is_err() {
        return false;
    }
    receiver.recv_timeout(PAGE_CHECK_TIMEOUT).unwrap_or(false)
}

// ───────────────────────── 메뉴 항목 정의 ─────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    ToPdf,
    ToJpg,
    ToPng,
    Split,
    Merge,
    Settings,
}

const ACTIONS: [Action; 6] = [
    Action::ToPdf,
    Action::ToJpg,
    Action::ToPng,
    Action::Split,
    Action::Merge,
    Action::Settings,
];

impl Action {
    fn title(self) -> PCWSTR {
        match self {
            Action::ToPdf => w!("PDF로 변환"),
            Action::ToJpg => w!("JPG로 변환"),
            Action::ToPng => w!("PNG로 변환"),
            Action::Split => w!("페이지별로 나누기"),
            Action::Merge => w!("하나의 PDF로 합치기…"),
            Action::Settings => w!("설정…"),
        }
    }

    /// ikoPDF.exe 에 넘기는 작업 이름. 설정 창은 인자 없이 실행한다.
    fn mode(self) -> Option<&'static str> {
        match self {
            Action::ToPdf => Some("to-pdf"),
            Action::ToJpg => Some("to-jpg"),
            Action::ToPng => Some("to-png"),
            Action::Split => Some("split"),
            Action::Merge => Some("merge"),
            Action::Settings => None,
        }
    }

    fn applies_to(self, selection: &Selection) -> bool {
        match self {
            Action::ToPdf => selection.images >= 1,
            Action::ToJpg | Action::ToPng | Action::Split => selection.pdfs >= 1,
            // 이미지와 PDF를 섞어 골라도 한 PDF로 합칠 수 있다.
            Action::Merge => selection.images + selection.pdfs >= 2,
            Action::Settings => true,
        }
    }
}

// ───────────────────────── 설치 폴더 ─────────────────────────

/// 이 DLL이 들어 있는 폴더 (= 설치 폴더).
fn install_dir() -> Option<PathBuf> {
    let module = HMODULE(DLL_MODULE.load(Ordering::Relaxed));
    let mut buffer = vec![0u16; 1024];
    let length = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
    path.parent().map(|dir| dir.to_path_buf())
}

fn alloc_string(text: &str) -> Result<PWSTR> {
    unsafe { SHStrDupW(&HSTRING::from(text)) }
}

fn menu_icon() -> Result<PWSTR> {
    let icon = install_dir()
        .map(|dir| dir.join("Assets").join("menu.ico"))
        .ok_or_else(|| Error::from(E_FAIL))?;
    alloc_string(&icon.to_string_lossy())
}

fn launch(action: Action, selection: &Selection) -> Result<()> {
    let exe = install_dir()
        .map(|dir| dir.join(EXE_NAME))
        .ok_or_else(|| Error::from(E_FAIL))?;
    let mut command = Command::new(exe);
    command.stdout(Stdio::null()).stderr(Stdio::null());

    let Some(mode) = action.mode() else {
        // 설정 창
        command.stdin(Stdio::null());
        command.spawn().map_err(|_| Error::from(E_FAIL))?;
        return Ok(());
    };

    let mut child = command
        .arg(mode)
        .arg("--stdin")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|_| Error::from(E_FAIL))?;

    // 파일 목록은 표준 입력으로 넘긴다(명령줄 길이 제한·한글 경로 문제 회피).
    // 탐색기를 붙잡지 않도록 별도 스레드에서 쓴다.
    let list = selection.paths.join("\n");
    let writer = std::thread::Builder::new().spawn(move || {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(list.as_bytes());
        }
        let _ = child.wait();
    });
    writer.map(|_| ()).map_err(|_| Error::from(E_FAIL))
}

// ───────────────────────── 상위 항목: "iko PDF 변환" ─────────────────────────

#[implement(IExplorerCommand)]
struct RootCommand {
    shared: Arc<Mutex<Shared>>,
}

impl IExplorerCommand_Impl for RootCommand_Impl {
    fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        unsafe { SHStrDupW(w!("iko PDF 변환")) }
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        guarded(menu_icon)
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(CLSID_IKOPDF_MENU)
    }

    fn GetState(&self, items: Ref<IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guarded(|| {
            let Some(selection) = read_selection(items.as_ref()) else {
                return Ok(ECS_ENABLED.0 as u32);
            };
            let usable = selection.images > 0 || selection.pdfs > 0;
            if let Ok(mut shared) = self.shared.lock() {
                shared.selection = selection;
            }
            Ok(if usable { ECS_ENABLED.0 } else { ECS_HIDDEN.0 } as u32)
        })
    }

    fn Invoke(&self, _items: Ref<IShellItemArray>, _context: Ref<IBindCtx>) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_HASSUBCOMMANDS.0 as u32)
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        guarded(|| {
            let commands = ACTIONS
                .iter()
                .map(|&action| {
                    SubCommand {
                        action,
                        shared: Arc::clone(&self.shared),
                    }
                    .into()
                })
                .collect();
            Ok(SubCommandEnum {
                commands,
                position: AtomicUsize::new(0),
            }
            .into())
        })
    }
}

// ───────────────────────── 하위 항목 ─────────────────────────

#[implement(IExplorerCommand)]
struct SubCommand {
    action: Action,
    shared: Arc<Mutex<Shared>>,
}

impl SubCommand {
    fn selection_for(&self, items: Option<&IShellItemArray>) -> Selection {
        if let Some(selection) = read_selection(items) {
            if !selection.paths.is_empty() {
                return selection;
            }
        }
        self.shared
            .lock()
            .map(|shared| shared.selection.clone())
            .unwrap_or_default()
    }

    /// 선택한 PDF가 모두 1쪽짜리라서 나눌 것이 없는지.
    fn nothing_to_split(&self, selection: &Selection) -> bool {
        let pdfs = selection.pdf_paths();
        if let Ok(shared) = self.shared.lock() {
            if let Some((known, result)) = &shared.single_page {
                if *known == pdfs {
                    return *result;
                }
            }
        }
        let result = all_single_page(pdfs.clone());
        if let Ok(mut shared) = self.shared.lock() {
            shared.single_page = Some((pdfs, result));
        }
        result
    }
}

impl IExplorerCommand_Impl for SubCommand_Impl {
    fn GetTitle(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        unsafe { SHStrDupW(self.action.title()) }
    }

    fn GetIcon(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetToolTip(&self, _items: Ref<IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(GUID::zeroed())
    }

    fn GetState(&self, items: Ref<IShellItemArray>, _ok_to_be_slow: BOOL) -> Result<u32> {
        guarded(|| {
            let selection = self.selection_for(items.as_ref());
            let state = if !self.action.applies_to(&selection) {
                ECS_HIDDEN
            } else if self.action == Action::Split && self.nothing_to_split(&selection) {
                ECS_DISABLED
            } else {
                ECS_ENABLED
            };
            Ok(state.0 as u32)
        })
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _context: Ref<IBindCtx>) -> Result<()> {
        guarded(|| {
            let selection = self.selection_for(items.as_ref());
            if selection.paths.is_empty() && self.action != Action::Settings {
                return Err(E_INVALIDARG.into());
            }
            launch(self.action, &selection)
        })
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_DEFAULT.0 as u32)
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

// ───────────────────────── 하위 항목 열거자 ─────────────────────────

#[implement(IEnumExplorerCommand)]
struct SubCommandEnum {
    commands: Vec<IExplorerCommand>,
    position: AtomicUsize,
}

impl IEnumExplorerCommand_Impl for SubCommandEnum_Impl {
    fn Next(
        &self,
        count: u32,
        commands: *mut Option<IExplorerCommand>,
        fetched: *mut u32,
    ) -> HRESULT {
        if commands.is_null() {
            return E_POINTER;
        }
        let mut written = 0u32;
        let mut position = self.position.load(Ordering::Relaxed);
        while written < count && position < self.commands.len() {
            unsafe {
                commands
                    .add(written as usize)
                    .write(Some(self.commands[position].clone()));
            }
            written += 1;
            position += 1;
        }
        self.position.store(position, Ordering::Relaxed);
        if !fetched.is_null() {
            unsafe { *fetched = written };
        }
        if written == count {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, count: u32) -> Result<()> {
        let position = self.position.load(Ordering::Relaxed) + count as usize;
        self.position
            .store(position.min(self.commands.len()), Ordering::Relaxed);
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        self.position.store(0, Ordering::Relaxed);
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        Ok(SubCommandEnum {
            commands: self.commands.clone(),
            position: AtomicUsize::new(self.position.load(Ordering::Relaxed)),
        }
        .into())
    }
}

// ───────────────────────── COM 클래스 팩토리 / DLL 진입점 ─────────────────────────

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut c_void,
    ) -> Result<()> {
        if object.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *object = null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let command: IExplorerCommand = RootCommand {
            shared: Arc::new(Mutex::new(Shared::default())),
        }
        .into();
        unsafe { command.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    class_id: *const GUID,
    iid: *const GUID,
    object: *mut *mut c_void,
) -> HRESULT {
    if class_id.is_null() || iid.is_null() || object.is_null() {
        return E_POINTER;
    }
    *object = null_mut();
    if *class_id != CLSID_IKOPDF_MENU {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    factory.query(iid, object)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // 수명 추적을 하지 않으므로 언로드를 허용하지 않는다.
    S_FALSE
}

#[no_mangle]
pub extern "system" fn DllMain(module: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        DLL_MODULE.store(module.0, Ordering::Relaxed);
    }
    BOOL(1)
}
