//! 자체 설치와 우클릭 메뉴 등록.
//!
//! 실행 파일 하나에 메뉴 DLL·패키지·인증서가 들어 있다. "메뉴 등록"을 하면
//! `%LOCALAPPDATA%\Programs\ikoPDF` 에 자신을 복사하고(메뉴가 가리킬 고정 위치),
//! 다음 순서로 메뉴를 등록한다.
//!
//! 1. **새 메뉴** (Windows 11 우클릭 첫 화면): 서명된 패키지를 등록한다. Windows가 패키지 서명을
//!    믿도록 공개 인증서를 *이 PC*의 "신뢰할 수 있는 사용자" 저장소에 넣어야 하고, 이 한 가지에만
//!    관리자 승인(UAC)을 받는다.
//! 2. 승인을 받지 못했거나 1이 막히면 **기존 메뉴** ("추가 옵션 표시" 안): 관리자 권한 없이
//!    현재 사용자 레지스트리에만 등록한다.
//!
//! "메뉴 해제"는 위에서 한 일을 모두 되돌린다(패키지, 인증서, 레지스트리, 설정, 폴더).

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use ikopdf::APP_NAME;
use windows::core::{w, HSTRING};
use windows::Foundation::Uri;
use windows::Management::Deployment::{AddPackageOptions, PackageManager};
use windows::Win32::Security::Cryptography::{
    CertAddEncodedCertificateToStore, CertCloseStore, CertCreateCertificateContext,
    CertDeleteCertificateFromStore, CertFindCertificateInStore, CertFreeCertificateContext,
    CertOpenStore, CERT_CONTEXT, CERT_FIND_EXISTING, CERT_OPEN_STORE_FLAGS,
    CERT_QUERY_ENCODING_TYPE, CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_PROV_SYSTEM_W,
    CERT_STORE_READONLY_FLAG, CERT_SYSTEM_STORE_CURRENT_USER, CERT_SYSTEM_STORE_LOCAL_MACHINE,
    HCERTSTORE, X509_ASN_ENCODING,
};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use windows_registry::CURRENT_USER;

// 실행 파일에 함께 들어 있는 것들 (build.rs 참고). 비어 있으면 그 기능은 쓸 수 없다.
static SHELL_DLL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shell.dll"));
static PACKAGE_MSIX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/package.msix"));
static PACKAGE_CERT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/package.cer"));
static MENU_ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/menu.ico"));

/// 들어 있는 패키지의 버전 (AppxManifest.xml 의 Identity Version).
const PACKAGE_VERSION: &str = env!("IKOPDF_PACKAGE_VERSION");
const PACKAGE_NAME: &str = "ikoPDF";
const PACKAGE_PUBLISHER: &str = "CN=ikoPDF";
/// 메뉴 DLL의 COM 클래스 (crates/shell-ext 의 CLSID_IKOPDF_MENU, AppxManifest.xml 과 같아야 한다).
const MENU_CLSID: &str = "{7B3F0C52-9A4E-4D1B-8E6A-2C5D91F4A7E3}";
const VERB_KEY: &str = "ikoPDF";
const EXTENSIONS: [&str; 11] = [
    ".jpg", ".jpeg", ".jpe", ".jfif", ".png", ".bmp", ".gif", ".tif", ".tiff", ".webp", ".pdf",
];
/// 프로그램 설정을 두는 곳 (현재 사용자).
const SETTINGS_KEY: &str = r"Software\ikoPDF";
const AUTO_UPDATE_VALUE: &str = "AutoUpdate";
const LAST_CHECK_VALUE: &str = "LastUpdateCheck";
// 예전 버전이 쓰던 "시작 프로그램" 항목. 지금은 지우기만 한다.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "ikoPDF";

pub const EXE_NAME: &str = "ikoPDF.exe";
const DLL_NAME: &str = "ikopdf_shell.dll";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuState {
    NotRegistered,
    /// Windows 11 새 우클릭 메뉴 (패키지).
    Modern,
    /// 기존 메뉴 — Windows 11에서는 "추가 옵션 표시" 안.
    Classic,
}

// ───────────────────────── 위치 ─────────────────────────

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join("ikoPDF")
}

/// 화면(WebView2) 캐시 등 프로그램이 쓰는 데이터 폴더.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("ikoPDF")
}

pub fn installed_exe() -> PathBuf {
    install_dir().join(EXE_NAME)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

/// 지금 실행 중인 파일이 설치 폴더 안의 것인지.
pub fn running_from_install_dir() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| same_file(dir, &install_dir())))
        .unwrap_or(false)
}

/// 설치된 버전 (설치되어 있지 않으면 `None`).
pub fn installed_version() -> Option<String> {
    let text = std::fs::read_to_string(install_dir().join("version.txt")).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

// ───────────────────────── 파일 설치 ─────────────────────────

/// 사용 중이라 덮어쓸 수 없는 파일은 이름을 바꿔 치워 두고 새로 쓴다.
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path).map(|existing| existing == bytes).unwrap_or(false) {
        return Ok(());
    }
    if std::fs::write(path, bytes).is_ok() {
        return Ok(());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".old{stamp}"));
    std::fs::rename(path, &aside)
        .with_context(|| format!("{} 을(를) 바꿀 수 없습니다", path.display()))?;
    std::fs::write(path, bytes).with_context(|| format!("{} 을(를) 쓸 수 없습니다", path.display()))
}

/// 예전에 치워 둔 파일과 업데이트 임시 파일을 지운다(실패해도 무시).
pub fn clean_leftovers() {
    let dir = install_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().contains(".old") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let update_dir = dir.join("update");
    let running_update = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| same_file(parent, &update_dir)))
        .unwrap_or(false);
    if !running_update {
        let _ = std::fs::remove_dir_all(update_dir);
    }
}

/// 설치 폴더에 실행 파일(자신의 복사본)과 메뉴 DLL, 아이콘을 둔다.
fn install_files() -> Result<()> {
    if SHELL_DLL.is_empty() {
        bail!("이 실행 파일에는 우클릭 메뉴 구성 요소가 들어 있지 않습니다(개발용 빌드)");
    }
    let dir = install_dir();
    std::fs::create_dir_all(dir.join("Assets")).context("설치 폴더를 만들 수 없습니다")?;

    let current = std::env::current_exe().context("실행 파일 위치를 알 수 없습니다")?;
    let target = installed_exe();
    if !same_file(&current, &target) {
        let bytes = std::fs::read(&current).context("실행 파일을 읽을 수 없습니다")?;
        write_replacing(&target, &bytes)?;
    }
    write_replacing(&dir.join(DLL_NAME), SHELL_DLL)?;
    write_replacing(&dir.join("Assets").join("menu.ico"), MENU_ICON)?;
    write_replacing(&dir.join("version.txt"), env!("CARGO_PKG_VERSION").as_bytes())?;
    Ok(())
}

// ───────────────────────── 인증서 ─────────────────────────

//
// Windows는 패키지 서명 인증서가 **이 PC 전체**("로컬 컴퓨터")의 "신뢰할 수 있는 사용자"
// 저장소에 있어야 패키지를 받아 준다. 거기에 쓰려면 관리자 권한이 필요하므로, 그 일만 하는
// 자신(`--trust-certificate`)을 관리자 권한으로 실행한다(Windows가 승인을 한 번 묻는다).
//
// 1.1.1 까지는 현재 사용자의 저장소에 넣었는데, Windows가 그것만으로는 인정하지 않아
// 새 메뉴 등록이 늘 0x800B0109 로 실패하고 기존 메뉴로 넘어갔다.

#[derive(Clone, Copy)]
enum Scope {
    /// 현재 사용자 (예전 버전이 쓰던 곳 — 지금은 지우기만 한다).
    User,
    /// 이 PC 전체. 쓰려면 관리자 권한이 필요하다.
    Machine,
}

/// "신뢰할 수 있는 사용자" 인증서 저장소.
struct CertStore(HCERTSTORE);

impl CertStore {
    fn trusted_people(scope: Scope, writable: bool) -> Result<CertStore> {
        let mut flags = match scope {
            Scope::User => CERT_SYSTEM_STORE_CURRENT_USER,
            Scope::Machine => CERT_SYSTEM_STORE_LOCAL_MACHINE,
        };
        if !writable {
            flags |= CERT_STORE_READONLY_FLAG.0;
        }
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(flags),
                Some(w!("TrustedPeople").as_ptr() as *const c_void),
            )
        }
        .map_err(|e| anyhow!("인증서 저장소를 열 수 없습니다: {}", e.message()))?;
        Ok(CertStore(store))
    }

    /// 저장소에 들어 있는 우리 인증서 (없으면 null). 받은 쪽이 해제하거나 삭제해야 한다.
    unsafe fn find(&self) -> *mut CERT_CONTEXT {
        let ours = CertCreateCertificateContext(X509_ASN_ENCODING, PACKAGE_CERT);
        if ours.is_null() {
            return std::ptr::null_mut();
        }
        let found = CertFindCertificateInStore(
            self.0,
            X509_ASN_ENCODING,
            0,
            CERT_FIND_EXISTING,
            Some(ours as *const c_void),
            None,
        );
        let _ = CertFreeCertificateContext(Some(ours));
        found
    }

    fn contains(&self) -> bool {
        unsafe {
            let found = self.find();
            if found.is_null() {
                return false;
            }
            let _ = CertFreeCertificateContext(Some(found));
            true
        }
    }

    fn add(&self) -> Result<()> {
        unsafe {
            CertAddEncodedCertificateToStore(
                Some(self.0),
                X509_ASN_ENCODING,
                PACKAGE_CERT,
                CERT_STORE_ADD_REPLACE_EXISTING,
                None,
            )
        }
        .map_err(|e| anyhow!("인증서를 추가할 수 없습니다: {}", e.message()))
    }

    fn remove(&self) {
        unsafe {
            let found = self.find();
            if !found.is_null() {
                // 삭제하면 찾은 핸들도 함께 해제된다.
                let _ = CertDeleteCertificateFromStore(found);
            }
        }
    }
}

impl Drop for CertStore {
    fn drop(&mut self) {
        unsafe {
            let _ = CertCloseStore(Some(self.0), 0);
        }
    }
}

fn machine_certificate_trusted() -> bool {
    !PACKAGE_CERT.is_empty()
        && CertStore::trusted_people(Scope::Machine, false)
            .map(|store| store.contains())
            .unwrap_or(false)
}

/// (관리자 권한으로 실행된 `--trust-certificate`) 인증서를 이 PC의 신뢰 저장소에 넣는다.
pub fn trust_machine_certificate() -> Result<()> {
    if PACKAGE_CERT.is_empty() {
        bail!("이 실행 파일에는 인증서가 들어 있지 않습니다");
    }
    CertStore::trusted_people(Scope::Machine, true)?.add()
}

/// (관리자 권한으로 실행된 `--untrust-certificate`) 넣어 둔 인증서를 지운다.
pub fn untrust_machine_certificate() -> Result<()> {
    let store = CertStore::trusted_people(Scope::Machine, true)?;
    store.remove();
    if store.contains() {
        bail!("인증서를 지울 수 없습니다");
    }
    Ok(())
}

/// 예전 버전이 현재 사용자 저장소에 넣어 둔 인증서가 있으면 지운다.
fn remove_user_certificate() {
    if PACKAGE_CERT.is_empty() {
        return;
    }
    let present = CertStore::trusted_people(Scope::User, false)
        .map(|store| store.contains())
        .unwrap_or(false);
    if present {
        if let Ok(store) = CertStore::trusted_people(Scope::User, true) {
            store.remove();
        }
    }
}

/// 자신을 관리자 권한으로 실행하고 끝나기를 기다린다. Windows가 사용자에게 승인을 묻는다.
fn run_elevated(argument: &str) -> Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe = std::env::current_exe().context("실행 파일 위치를 알 수 없습니다")?;
    let exe = HSTRING::from(exe.as_os_str());
    let argument = HSTRING::from(argument);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(argument.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        if let Err(error) = ShellExecuteExW(&mut info) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                bail!("관리자 승인을 받지 못했습니다");
            }
            bail!("관리자 권한으로 실행할 수 없습니다: {}", error.message());
        }
        if info.hProcess.is_invalid() {
            bail!("관리자 권한으로 실행할 수 없습니다");
        }
        WaitForSingleObject(info.hProcess, 60_000);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if code != 0 {
            bail!("관리자 권한 작업이 실패했습니다 (코드 {code})");
        }
    }
    Ok(())
}

/// 패키지 서명 인증서가 이 PC에서 신뢰되도록 한다. 아직 아니면 관리자 승인을 받아 등록하는데,
/// `interactive` 가 거짓이면(창 없이 도는 자동 업데이트) 승인 창을 띄우지 않고 실패한다.
fn ensure_certificate_trusted(interactive: bool) -> Result<()> {
    if machine_certificate_trusted() {
        return Ok(());
    }
    if !interactive {
        bail!("인증서가 이 PC에 등록되어 있지 않습니다");
    }
    run_elevated("--trust-certificate")?;
    if !machine_certificate_trusted() {
        bail!("인증서가 등록되지 않았습니다 (보안 프로그램이 막았을 수 있습니다)");
    }
    Ok(())
}

// ───────────────────────── 새 메뉴 (패키지) ─────────────────────────

/// 패키지 배포 API는 다중 스레드 아파트에서 기다리는 편이 안전해서 별도 스레드에서 부른다.
fn on_worker<T: Send>(body: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
                body()
            })
            .join()
            .expect("작업 스레드가 비정상 종료했습니다")
    })
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file:///");
    for byte in path.to_string_lossy().replace('\\', "/").bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                uri.push(byte as char)
            }
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

/// 등록된 패키지들의 (전체 이름, 버전).
fn registered_packages(manager: &PackageManager) -> Result<Vec<(HSTRING, String)>> {
    let packages = manager.FindPackagesByUserSecurityIdNamePublisher(
        &HSTRING::new(),
        &HSTRING::from(PACKAGE_NAME),
        &HSTRING::from(PACKAGE_PUBLISHER),
    )?;
    let mut found = Vec::new();
    for package in packages.First()? {
        let id = package.Id()?;
        let v = id.Version()?;
        found.push((
            id.FullName()?,
            format!("{}.{}.{}.{}", v.Major, v.Minor, v.Build, v.Revision),
        ));
    }
    Ok(found)
}

fn package_registered() -> Option<String> {
    on_worker(|| {
        let manager = PackageManager::new().ok()?;
        registered_packages(&manager)
            .ok()?
            .into_iter()
            .next()
            .map(|(_, version)| version)
    })
}

fn remove_packages() -> Result<()> {
    on_worker(|| -> Result<()> {
        let manager = PackageManager::new()?;
        for (full_name, _) in registered_packages(&manager)? {
            manager
                .RemovePackageAsync(&full_name)?
                .join()
                .map_err(|e| anyhow!("{} (0x{:08X})", e.message(), e.code().0 as u32))?;
        }
        Ok(())
    })
}

fn register_package(interactive: bool) -> Result<()> {
    if PACKAGE_MSIX.is_empty() || PACKAGE_CERT.is_empty() {
        bail!("이 실행 파일에는 메뉴 패키지가 들어 있지 않습니다");
    }
    ensure_certificate_trusted(interactive)?;

    let dir = install_dir();
    let msix = dir.join("ikoPDF.msix");
    std::fs::write(&msix, PACKAGE_MSIX).context("패키지 파일을 쓸 수 없습니다")?;

    let result = on_worker(|| -> Result<()> {
        let manager = PackageManager::new()?;
        // 같은 버전이 이미 있으면 다시 등록할 수 없으므로 먼저 지운다.
        for (full_name, _) in registered_packages(&manager)? {
            let _ = manager.RemovePackageAsync(&full_name)?.join();
        }
        let options = AddPackageOptions::new()?;
        options.SetExternalLocationUri(&Uri::CreateUri(&HSTRING::from(file_uri(&dir)))?)?;
        let package_uri = Uri::CreateUri(&HSTRING::from(file_uri(&msix)))?;
        let deployment = manager
            .AddPackageByUriAsync(&package_uri, &options)?
            .join()
            .map_err(|e| anyhow!("{} (0x{:08X})", e.message(), e.code().0 as u32))?;
        let code = deployment.ExtendedErrorCode()?;
        if code.is_err() {
            bail!("{} (0x{:08X})", deployment.ErrorText()?, code.0 as u32);
        }
        Ok(())
    });

    let _ = std::fs::remove_file(&msix);
    result
}

// ───────────────────────── 기존 메뉴 (레지스트리) ─────────────────────────

fn registry_error(what: &str, error: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{what}: {error}")
}

fn classic_registered() -> bool {
    CURRENT_USER
        .open(format!(r"Software\Classes\CLSID\{MENU_CLSID}\InprocServer32"))
        .is_ok()
}

fn register_classic() -> Result<()> {
    let dir = install_dir();
    let registry = |e| registry_error("레지스트리에 쓸 수 없습니다", e);

    let classes = CURRENT_USER.create(r"Software\Classes").map_err(registry)?;
    let class = classes.create(format!(r"CLSID\{MENU_CLSID}")).map_err(registry)?;
    class.set_string("", APP_NAME).map_err(registry)?;
    let server = class.create("InprocServer32").map_err(registry)?;
    server
        .set_string("", dir.join(DLL_NAME).to_string_lossy())
        .map_err(registry)?;
    server.set_string("ThreadingModel", "Apartment").map_err(registry)?;

    let icon = dir.join("Assets").join("menu.ico");
    for extension in EXTENSIONS {
        let verb = classes
            .create(format!(r"SystemFileAssociations\{extension}\shell\{VERB_KEY}"))
            .map_err(registry)?;
        verb.set_string("MUIVerb", APP_NAME).map_err(registry)?;
        verb.set_string("Icon", icon.to_string_lossy()).map_err(registry)?;
        verb.set_string("ExplorerCommandHandler", MENU_CLSID).map_err(registry)?;
    }
    Ok(())
}

fn remove_classic() {
    let Ok(classes) = CURRENT_USER.create(r"Software\Classes") else { return };
    let _ = classes.remove_tree(format!(r"CLSID\{MENU_CLSID}"));
    for extension in EXTENSIONS {
        let _ = classes.remove_tree(format!(r"SystemFileAssociations\{extension}\shell\{VERB_KEY}"));
    }
}

fn notify_shell() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

// ───────────────────────── 등록 / 해제 ─────────────────────────

/// 메뉴가 실제로 쓰는 파일(실행 파일, 메뉴 DLL)이 설치 폴더에 있는지.
fn files_installed() -> bool {
    installed_exe().is_file() && install_dir().join(DLL_NAME).is_file()
}

pub fn menu_state() -> MenuState {
    // 등록 정보만 남고 파일이 없으면 메뉴가 뜨지 않는다. 다시 등록할 수 있게 "등록 안 됨"으로 본다.
    if !files_installed() {
        return MenuState::NotRegistered;
    }
    if package_registered().is_some() {
        MenuState::Modern
    } else if classic_registered() {
        MenuState::Classic
    } else {
        MenuState::NotRegistered
    }
}

/// 등록 결과와, 새 메뉴 대신 기존 메뉴로 등록됐다면 그 이유.
pub struct Registration {
    pub state: MenuState,
    pub fallback_reason: Option<String>,
}

/// 메뉴를 등록한다. `prefer_modern` 이 거짓이면 기존 메뉴로만 등록한다.
/// `interactive` 가 참이면 새 메뉴에 필요한 관리자 승인 창을 띄울 수 있다.
pub fn register_menu(prefer_modern: bool, interactive: bool) -> Result<Registration> {
    // 방금 해제하면서 예약해 둔 "폴더 지우기"가 있으면 취소한다.
    let _ = std::fs::remove_file(removal_marker());
    install_files()?;
    remove_legacy_autostart();
    remove_user_certificate();

    let modern = if prefer_modern {
        register_package(interactive)
    } else {
        Err(anyhow!("기존 메뉴로 등록하도록 지정했습니다"))
    };
    let registration = match modern {
        Ok(()) => {
            remove_classic();
            Registration {
                state: MenuState::Modern,
                fallback_reason: None,
            }
        }
        Err(reason) => {
            let _ = remove_packages();
            register_classic()?;
            Registration {
                state: MenuState::Classic,
                fallback_reason: Some(format!("{reason:#}")),
            }
        }
    };
    // 관리자 승인을 기다리는 사이에 파일이 지워졌더라도(예약된 삭제가 취소 직전에 돈 경우)
    // 메뉴가 빈 폴더를 가리키지 않도록 다시 확인해서 채운다.
    install_files()?;
    notify_shell();
    Ok(registration)
}

pub fn unregister_menu() -> Result<()> {
    let removed = remove_packages();
    remove_user_certificate();
    // 이 PC에 등록해 둔 인증서는 관리자 권한으로만 지울 수 있다(승인 창이 한 번 뜬다).
    let certificate = if machine_certificate_trusted() {
        run_elevated("--untrust-certificate")
            .context("메뉴는 해제했지만, 이 PC에 등록한 인증서는 지우지 못했습니다")
    } else {
        Ok(())
    };
    remove_classic();
    remove_legacy_autostart();
    let _ = CURRENT_USER.remove_tree(SETTINGS_KEY);
    notify_shell();
    remove_install_dir();
    removed.context("패키지를 해제하지 못했습니다")?;
    certificate
}

/// 예약해 둔 "폴더 지우기"가 아직 유효하다는 표시. 다시 등록하면 지워서 예약을 취소한다.
fn removal_marker() -> PathBuf {
    std::env::temp_dir().join("ikoPDF-remove.pending")
}

/// 설치 폴더와 데이터 폴더를 지운다. 사용 중인 파일(지금 실행 중인 exe, 탐색기가 잡고 있는
/// DLL, 열려 있는 설정 창의 화면 캐시)이 있으면 프로그램이 끝난 뒤 다시 한번 지우도록 예약한다.
///
/// 예약된 삭제는 몇 초 뒤에 돈다. 그 사이에 사용자가 다시 등록하면 새로 설치한 파일을
/// 지워 버리므로(1.1.2 시험판에서 실제로 일어남), 표시 파일이 남아 있을 때만 지우게 한다.
fn remove_install_dir() {
    let targets = [install_dir(), data_dir()];
    if !running_from_install_dir() {
        for dir in &targets {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    let remaining: Vec<String> = targets
        .iter()
        .filter(|dir| dir.exists())
        .map(|dir| format!("rmdir /s /q \"{}\"", dir.to_string_lossy()))
        .collect();
    if remaining.is_empty() {
        return;
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let marker = removal_marker();
    if std::fs::write(&marker, b"").is_err() {
        // 취소할 방법이 없으면 예약하지 않는다(남은 파일은 다음 등록·해제 때 정리된다).
        return;
    }
    let marker = marker.to_string_lossy().into_owned();
    // cmd 에서는 `if 조건 A & B` 의 B 도 조건 안에 들어간다.
    let script = format!(
        "ping -n 8 127.0.0.1 >nul & if exist \"{marker}\" {} & del /q \"{marker}\"",
        remaining.join(" & ")
    );
    let _ = std::process::Command::new("cmd.exe")
        .raw_arg(format!("/c {script}"))
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}
/// 새 버전의 실행 파일(자신)을 설치 폴더에 반영하고, 필요하면 메뉴 등록을 갱신한다.
pub fn apply_update() -> Result<()> {
    let before = menu_state();
    install_files()?;
    remove_legacy_autostart();
    match before {
        MenuState::Modern => {
            // 패키지 내용(메뉴가 뜨는 파일 종류 등)이 바뀐 버전이면 다시 등록한다.
            if package_registered().as_deref() != Some(PACKAGE_VERSION) {
                register_menu(true, false)?;
            }
        }
        MenuState::Classic => {
            register_classic()?;
            notify_shell();
        }
        MenuState::NotRegistered => {}
    }
    Ok(())
}

// ───────────────────────── 자동 업데이트 설정 ─────────────────────────
//
// 프로그램은 Windows 시작 때 자동으로 실행되지 않는다. 1.1.0 까지는 로그인 때 업데이트를
// 확인하려고 "시작 프로그램"에 자신을 등록했는데, 백신이 그 동작을 악성 행위로 보고 막았다.
// 지금은 우클릭 메뉴를 쓸 때 하루에 한 번 확인한다(`update::check_in_background_if_due`).

/// 자동 업데이트를 쓰는지. 따로 끄지 않았으면 켜져 있다.
pub fn auto_update_enabled() -> bool {
    CURRENT_USER
        .open(SETTINGS_KEY)
        .and_then(|key| key.get_u32(AUTO_UPDATE_VALUE))
        .map(|value| value != 0)
        .unwrap_or(true)
}

pub fn set_auto_update(enabled: bool) -> Result<()> {
    let registry = |e| registry_error("설정을 저장할 수 없습니다", e);
    CURRENT_USER
        .create(SETTINGS_KEY)
        .map_err(registry)?
        .set_u32(AUTO_UPDATE_VALUE, enabled as u32)
        .map_err(registry)
}

/// 마지막으로 새 버전을 확인한 지 `interval` 이 지났으면 지금 시각을 적어 두고 참을 돌려준다.
pub fn claim_update_check(interval: std::time::Duration) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let Ok(key) = CURRENT_USER.create(SETTINGS_KEY) else {
        return false;
    };
    let last = key.get_u64(LAST_CHECK_VALUE).unwrap_or(0);
    // 시계가 뒤로 돌아간 경우(now < last)에는 다시 확인한다.
    if now >= last && now - last < interval.as_secs() {
        return false;
    }
    key.set_u64(LAST_CHECK_VALUE, now).is_ok()
}

/// 1.1.0 까지의 버전이 "시작 프로그램"에 넣던 항목이 남아 있으면 지운다.
fn remove_legacy_autostart() {
    let exists = CURRENT_USER
        .open(RUN_KEY)
        .and_then(|key| key.get_string(RUN_VALUE))
        .is_ok();
    if exists {
        if let Ok(key) = CURRENT_USER.create(RUN_KEY) {
            let _ = key.remove_value(RUN_VALUE);
        }
    }
}
