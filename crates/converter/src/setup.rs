//! 자체 설치와 우클릭 메뉴 등록.
//!
//! 실행 파일 하나에 메뉴 DLL·패키지·인증서가 들어 있다. "메뉴 등록"을 하면
//! `%LOCALAPPDATA%\Programs\ikoPDF` 에 자신을 복사하고(메뉴가 가리킬 고정 위치),
//! 다음 순서로 메뉴를 등록한다. 관리자 권한은 쓰지 않는다.
//!
//! 1. **새 메뉴** (Windows 11 우클릭 첫 화면): 서명된 패키지를 등록한다. 패키지 서명을
//!    믿도록 공개 인증서를 *현재 사용자*의 "신뢰할 수 있는 사용자" 저장소에 넣는다.
//! 2. 1이 막히면 **기존 메뉴** ("추가 옵션 표시" 안): 현재 사용자 레지스트리에만 등록한다.
//!
//! "메뉴 해제"는 위에서 한 일을 모두 되돌린다(패키지, 인증서, 레지스트리, 자동 실행, 폴더).

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
    CertOpenStore, CERT_FIND_EXISTING, CERT_OPEN_STORE_FLAGS, CERT_QUERY_ENCODING_TYPE,
    CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_CURRENT_USER,
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

struct CertStore(HCERTSTORE);

impl CertStore {
    /// 현재 사용자의 "신뢰할 수 있는 사용자" 저장소.
    fn trusted_people() -> Result<CertStore> {
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_CURRENT_USER),
                Some(w!("TrustedPeople").as_ptr() as *const c_void),
            )
        }
        .map_err(|e| anyhow!("인증서 저장소를 열 수 없습니다: {}", e.message()))?;
        Ok(CertStore(store))
    }
}

impl Drop for CertStore {
    fn drop(&mut self) {
        unsafe {
            let _ = CertCloseStore(Some(self.0), 0);
        }
    }
}

fn trust_certificate() -> Result<()> {
    let store = CertStore::trusted_people()?;
    unsafe {
        CertAddEncodedCertificateToStore(
            Some(store.0),
            X509_ASN_ENCODING,
            PACKAGE_CERT,
            CERT_STORE_ADD_REPLACE_EXISTING,
            None,
        )
    }
    .map_err(|e| anyhow!("인증서를 추가할 수 없습니다: {}", e.message()))
}

fn untrust_certificate() {
    if PACKAGE_CERT.is_empty() {
        return;
    }
    let Ok(store) = CertStore::trusted_people() else { return };
    unsafe {
        let ours = CertCreateCertificateContext(X509_ASN_ENCODING, PACKAGE_CERT);
        if ours.is_null() {
            return;
        }
        let found = CertFindCertificateInStore(
            store.0,
            X509_ASN_ENCODING,
            0,
            CERT_FIND_EXISTING,
            Some(ours as *const c_void),
            None,
        );
        if !found.is_null() {
            // 삭제하면 찾은 핸들도 함께 해제된다.
            let _ = CertDeleteCertificateFromStore(found);
        }
        let _ = CertFreeCertificateContext(Some(ours));
    }
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

fn register_package() -> Result<()> {
    if PACKAGE_MSIX.is_empty() || PACKAGE_CERT.is_empty() {
        bail!("이 실행 파일에는 메뉴 패키지가 들어 있지 않습니다");
    }
    trust_certificate()?;

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
    if result.is_err() {
        untrust_certificate();
    }
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

pub fn menu_state() -> MenuState {
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
pub fn register_menu(prefer_modern: bool) -> Result<Registration> {
    install_files()?;

    let modern = if prefer_modern {
        register_package()
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
    notify_shell();
    Ok(registration)
}

pub fn unregister_menu() -> Result<()> {
    let removed = remove_packages();
    untrust_certificate();
    remove_classic();
    let _ = set_autostart(false);
    notify_shell();
    remove_install_dir();
    removed.context("패키지를 해제하지 못했습니다")
}

/// 설치 폴더를 지운다. 사용 중인 파일(지금 실행 중인 exe, 탐색기가 잡고 있는 DLL)이 있으면
/// 프로그램이 끝난 뒤 다시 한번 지우도록 예약한다.
fn remove_install_dir() {
    let dir = install_dir();
    if !running_from_install_dir() && std::fs::remove_dir_all(&dir).is_ok() {
        return;
    }
    if !dir.exists() {
        return;
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!(
        "ping -n 8 127.0.0.1 >nul & rmdir /s /q \"{}\"",
        dir.to_string_lossy()
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
    match before {
        MenuState::Modern => {
            // 패키지 내용(메뉴가 뜨는 파일 종류 등)이 바뀐 버전이면 다시 등록한다.
            if package_registered().as_deref() != Some(PACKAGE_VERSION) {
                register_menu(true)?;
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

// ───────────────────────── 자동 실행 ─────────────────────────

/// Windows 로그인 때 업데이트를 확인하도록 등록되어 있는지.
pub fn autostart_enabled() -> bool {
    CURRENT_USER
        .open(RUN_KEY)
        .and_then(|key| key.get_string(RUN_VALUE))
        .is_ok()
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let registry = |e| registry_error("자동 실행 설정을 바꿀 수 없습니다", e);
    let key = CURRENT_USER.create(RUN_KEY).map_err(registry)?;
    if enabled {
        let command = format!("\"{}\" --startup-update", installed_exe().to_string_lossy());
        key.set_string(RUN_VALUE, command).map_err(registry)
    } else {
        let _ = key.remove_value(RUN_VALUE);
        Ok(())
    }
}
