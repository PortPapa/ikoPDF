//! 출력 파일 이름 규칙과 파일명 정렬.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// 변환 대상으로 받는 이미지 확장자 (소문자).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "bmp", "gif", "tif", "tiff", "webp",
];

fn extension_lower(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_image(path: &Path) -> bool {
    IMAGE_EXTENSIONS.contains(&extension_lower(path).as_str())
}

pub fn is_pdf(path: &Path) -> bool {
    extension_lower(path) == "pdf"
}

/// 파일 이름에서 확장자를 뺀 부분.
pub fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string())
}

/// 이미 있는 파일/폴더를 덮어쓰지 않도록 `이름 (2).확장자`, `이름 (3).확장자` … 를 찾는다.
pub fn unique_path(desired: &Path) -> PathBuf {
    if !desired.exists() {
        return desired.to_path_buf();
    }
    let parent = desired.parent().unwrap_or_else(|| Path::new("."));
    let name = stem(desired);
    let ext = desired.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 2u32.. {
        let file_name = match &ext {
            Some(ext) => format!("{name} ({n}).{ext}"),
            None => format!("{name} ({n})"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// 여러 페이지 결과물을 모아 둘 폴더 `parent\name` 을 준비한다.
/// 이미 그런 폴더가 있으면 그대로 쓰고(안의 파일은 [`unique_path`] 로 겹치지 않게 저장),
/// 같은 이름의 *파일*이 있으면 `name (2)` 처럼 다른 이름으로 만든다.
pub fn output_folder(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let desired = parent.join(name);
    let folder = if desired.is_dir() {
        desired
    } else {
        unique_path(&desired)
    };
    std::fs::create_dir_all(&folder)?;
    Ok(folder)
}

/// 탐색기처럼 숫자를 숫자로 비교하는 정렬 (`2.jpg` < `10.jpg`). 대소문자는 구분하지 않는다.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a: Vec<char> = a.chars().flat_map(char::to_lowercase).collect();
    let b: Vec<char> = b.chars().flat_map(char::to_lowercase).collect();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let start_i = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let start_j = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let num_a = trim_leading_zeros(&a[start_i..i]);
            let num_b = trim_leading_zeros(&b[start_j..j]);
            let ord = num_a
                .len()
                .cmp(&num_b.len())
                .then_with(|| num_a.cmp(num_b));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let ord = a[i].cmp(&b[j]);
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn trim_leading_zeros(digits: &[char]) -> &[char] {
    let first_non_zero = digits.iter().position(|c| *c != '0').unwrap_or(digits.len());
    &digits[first_non_zero..]
}

/// 경로 목록을 파일 이름 기준 자연 정렬한다 (이름이 같으면 전체 경로로).
pub fn sort_natural(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        let name_a = a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name_b = b.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        natural_cmp(&name_a, &name_b)
            .then_with(|| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_compares_numbers_as_numbers() {
        assert_eq!(natural_cmp("2.jpg", "10.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("스캔_10.jpg", "스캔_9.jpg"), Ordering::Greater);
        assert_eq!(natural_cmp("a01.png", "a1.png"), Ordering::Equal);
        assert_eq!(natural_cmp("IMG_3.JPG", "img_3.jpg"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "ab"), Ordering::Less);
    }

    #[test]
    fn sort_natural_orders_like_explorer() {
        let mut paths: Vec<PathBuf> = ["p10.jpg", "p2.jpg", "p1.jpg", "p20.jpg"]
            .iter()
            .map(PathBuf::from)
            .collect();
        sort_natural(&mut paths);
        let names: Vec<_> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["p1.jpg", "p2.jpg", "p10.jpg", "p20.jpg"]);
    }

    #[test]
    fn unique_path_does_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("imgpdf-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("문서.pdf");
        assert_eq!(unique_path(&first), first);
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&first);
        assert_eq!(second, dir.join("문서 (2).pdf"));
        std::fs::write(&second, b"x").unwrap();
        assert_eq!(unique_path(&first), dir.join("문서 (3).pdf"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_kinds_by_extension() {
        assert!(is_image(Path::new("a.JPG")));
        assert!(is_image(Path::new("a.webp")));
        assert!(!is_image(Path::new("a.pdf")));
        assert!(is_pdf(Path::new("문서.PDF")));
    }
}
