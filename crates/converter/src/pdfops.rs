//! PDF 나누기 / 합치기 (페이지 내용은 다시 그리지 않고 그대로 옮긴다 → 화질·글자 유지).
//!
//! - [`split_pdf`]: 한 PDF를 페이지마다 하나의 PDF로 나눈다. `문서\문서_001.pdf …`
//! - [`merge_to_pdf`]: 여러 PDF(와 이미지)를 주어진 순서대로 한 PDF로 합친다.
//!
//! 책갈피(목차)와 양식 필드 같은 문서 전체 정보는 옮기지 않는다.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::img2pdf::images_to_pdf_bytes;
use crate::naming::{is_image, output_folder, stem, unique_path};
use crate::Cancelled;

/// 부모 노드에서 물려받을 수 있는 페이지 속성.
const INHERITABLE: [&[u8]; 4] = [b"MediaBox", b"CropBox", b"Resources", b"Rotate"];

fn load(path: &Path) -> Result<Document> {
    let document = Document::load(path).map_err(|e| {
        anyhow!("PDF를 열 수 없습니다(손상되었거나 지원하지 않는 형식): {e}")
    })?;
    ensure_not_encrypted(&document)?;
    Ok(document)
}

fn ensure_not_encrypted(document: &Document) -> Result<()> {
    if document.is_encrypted() {
        bail!("암호가 걸린 PDF는 처리할 수 없습니다");
    }
    Ok(())
}

fn is_page_tree_node(document: &Document, id: ObjectId) -> bool {
    document
        .get_object(id)
        .ok()
        .and_then(|object| object.type_name().ok())
        .map(|name| name == b"Page" || name == b"Pages")
        .unwrap_or(false)
}

/// 페이지나 그 부모들에서 속성 값을 찾는다.
fn inherited_attribute(document: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    for _ in 0..64 {
        let node = document.get_dictionary(current).ok()?;
        if let Ok(value) = node.get(key) {
            if !matches!(value, Object::Null) {
                return Some(value.clone());
            }
        }
        current = node.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

/// 모든 페이지가 부모 없이도 온전하도록 물려받은 속성을 페이지에 직접 써 넣고,
/// 페이지 ID를 순서대로 돌려준다.
fn materialize_pages(document: &mut Document) -> Vec<ObjectId> {
    let pages: Vec<ObjectId> = document.get_pages().into_values().collect();
    for &page_id in &pages {
        let mut additions: Vec<(&[u8], Object)> = Vec::new();
        for key in INHERITABLE {
            let has_own = document
                .get_dictionary(page_id)
                .map(|page| page.has(key))
                .unwrap_or(true);
            if has_own {
                continue;
            }
            if let Some(value) = inherited_attribute(document, page_id, key) {
                additions.push((key, value));
            }
        }
        if let Ok(page) = document.get_dictionary_mut(page_id) {
            for (key, value) in additions {
                page.set(key, value);
            }
        }
    }
    pages
}

fn pages_node(kids: &[ObjectId]) -> Object {
    let mut node = Dictionary::new();
    node.set("Type", Object::Name(b"Pages".to_vec()));
    node.set(
        "Kids",
        Object::Array(kids.iter().map(|id| Object::Reference(*id)).collect()),
    );
    node.set("Count", Object::Integer(kids.len() as i64));
    Object::Dictionary(node)
}

fn catalog_node(pages_id: ObjectId) -> Object {
    let mut node = Dictionary::new();
    node.set("Type", Object::Name(b"Catalog".to_vec()));
    node.set("Pages", Object::Reference(pages_id));
    Object::Dictionary(node)
}

fn set_parent(document: &mut Document, page_id: ObjectId, pages_id: ObjectId) {
    if let Ok(page) = document.get_dictionary_mut(page_id) {
        page.set("Parent", Object::Reference(pages_id));
    }
}

fn save(document: &mut Document, output: &Path) -> Result<()> {
    document
        .save(output)
        .map(|_| ())
        .map_err(|e| anyhow!("PDF 파일을 저장할 수 없습니다: {e}"))
}

// ───────────────────────── 나누기 ─────────────────────────

/// `object` 에서 참조로 이어지는 모든 객체 ID를 모은다.
/// 다른 페이지나 페이지 트리 노드(부모, 링크 대상 등)로는 넘어가지 않는다.
fn collect_reachable(document: &Document, root_page: ObjectId, reachable: &mut BTreeSet<ObjectId>) {
    let mut pending = vec![root_page];
    reachable.insert(root_page);

    fn references(object: &Object, found: &mut Vec<ObjectId>) {
        match object {
            Object::Reference(id) => found.push(*id),
            Object::Array(items) => items.iter().for_each(|item| references(item, found)),
            Object::Dictionary(dict) => dict.iter().for_each(|(_, value)| references(value, found)),
            Object::Stream(stream) => stream
                .dict
                .iter()
                .for_each(|(_, value)| references(value, found)),
            _ => {}
        }
    }

    while let Some(id) = pending.pop() {
        let Ok(object) = document.get_object(id) else { continue };
        let mut found = Vec::new();
        references(object, &mut found);
        for next in found {
            if reachable.contains(&next) || is_page_tree_node(document, next) {
                continue;
            }
            reachable.insert(next);
            pending.push(next);
        }
    }
}

/// PDF의 페이지 수 (암호가 걸렸거나 열 수 없으면 오류).
pub fn page_count(pdf: &Path) -> Result<usize> {
    Ok(load(pdf)?.get_pages().len())
}

/// PDF를 페이지마다 하나의 파일로 나눈다. PDF 이름으로 폴더를 만들어 그 안에 저장한다.
///
/// `on_page(i, total)`은 i번째(0부터) 페이지를 처리하기 직전에 호출되며 `false`면 취소한다.
pub fn split_pdf(pdf: &Path, mut on_page: impl FnMut(u32, u32) -> bool) -> Result<Vec<PathBuf>> {
    let mut source = load(pdf)?;
    let pages = materialize_pages(&mut source);
    if pages.is_empty() {
        bail!("페이지가 없는 PDF입니다");
    }
    if pages.len() == 1 {
        bail!("1쪽짜리 PDF라서 나눌 것이 없습니다");
    }

    let parent = pdf.parent().unwrap_or_else(|| Path::new("."));
    let name = stem(pdf);
    let output_dir = output_folder(parent, &name).context("결과 폴더를 만들 수 없습니다")?;

    let total = pages.len() as u32;
    let digits = total.to_string().len().max(3);
    let pages_id: ObjectId = (source.max_id + 1, 0);
    let catalog_id: ObjectId = (source.max_id + 2, 0);

    let mut outputs = Vec::with_capacity(pages.len());
    for (index, &page_id) in pages.iter().enumerate() {
        if !on_page(index as u32, total) {
            return Err(Cancelled.into());
        }

        let mut reachable = BTreeSet::new();
        collect_reachable(&source, page_id, &mut reachable);

        let mut single = Document::with_version(source.version.clone());
        for id in reachable {
            if let Some(object) = source.objects.get(&id) {
                single.objects.insert(id, object.clone());
            }
        }
        set_parent(&mut single, page_id, pages_id);
        single.objects.insert(pages_id, pages_node(&[page_id]));
        single.objects.insert(catalog_id, catalog_node(pages_id));
        single.trailer.set("Root", Object::Reference(catalog_id));
        single.max_id = catalog_id.0;
        single.renumber_objects();

        let target = unique_path(&output_dir.join(format!("{name}_{:0digits$}.pdf", index + 1)));
        save(&mut single, &target).with_context(|| format!("{}쪽", index + 1))?;
        outputs.push(target);
    }
    Ok(outputs)
}

// ───────────────────────── 합치기 ─────────────────────────

/// PDF와 이미지를 `inputs` 순서대로 한 PDF로 합친다. 이미지는 한 장이 한 페이지가 된다.
///
/// `on_item(i)`는 i번째 입력을 처리하기 직전에 호출되며 `false`면 취소한다.
pub fn merge_to_pdf(
    inputs: &[PathBuf],
    output: &Path,
    mut on_item: impl FnMut(usize) -> bool,
) -> Result<()> {
    if inputs.is_empty() {
        bail!("합칠 파일이 없습니다");
    }

    let mut merged = Document::with_version("1.7");
    let mut page_ids: Vec<ObjectId> = Vec::new();
    let mut next_id = 1u32;

    for (index, input) in inputs.iter().enumerate() {
        if !on_item(index) {
            return Err(Cancelled.into());
        }
        let file_name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut document = if is_image(input) {
            let bytes = images_to_pdf_bytes(std::slice::from_ref(input), |_| true)?;
            Document::load_mem(&bytes)
                .map_err(|e| anyhow!("이미지를 PDF로 바꾸지 못했습니다: {e}"))
                .with_context(|| file_name.clone())?
        } else {
            load(input).with_context(|| file_name.clone())?
        };

        // 번호를 다시 매기기 전에 물려받은 속성을 풀어 둔다.
        materialize_pages(&mut document);
        document.renumber_objects_with(next_id);
        next_id = document.max_id + 1;

        let pages: Vec<ObjectId> = document.get_pages().into_values().collect();
        if pages.is_empty() {
            return Err(anyhow!("페이지가 없는 PDF입니다")).with_context(|| file_name);
        }
        let page_set: BTreeSet<ObjectId> = pages.iter().copied().collect();
        page_ids.extend(pages);

        for (id, object) in std::mem::take(&mut document.objects) {
            // 원본의 문서 뼈대(카탈로그, 페이지 트리)는 버리고 새로 만든다.
            let type_name = object.type_name().ok().map(|name| name.to_vec());
            let skip = match type_name.as_deref() {
                Some(b"Catalog") | Some(b"Pages") => true,
                Some(b"Page") => !page_set.contains(&id),
                _ => false,
            };
            if !skip {
                merged.objects.insert(id, object);
            }
        }
    }

    let pages_id: ObjectId = (next_id, 0);
    let catalog_id: ObjectId = (next_id + 1, 0);
    for &page_id in &page_ids {
        set_parent(&mut merged, page_id, pages_id);
    }
    merged.objects.insert(pages_id, pages_node(&page_ids));
    merged.objects.insert(catalog_id, catalog_node(pages_id));
    merged.trailer.set("Root", Object::Reference(catalog_id));
    merged.max_id = catalog_id.0;

    // 더 이상 어디서도 쓰이지 않는 객체(원본의 목차 등)를 정리한다.
    merged.prune_objects();
    merged.renumber_objects();

    save(&mut merged, output)
}
