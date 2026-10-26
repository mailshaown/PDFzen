//! Generates `sample.pdf` in the project root: a two page document with all
//! four annotation kinds, one rotated page and a merged page, produced by the
//! same engine the app uses.
//!
//! Run with: `cargo run --example make_sample`

use base64::{Engine as _, engine::general_purpose};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};
use pdfzen_lib::{Anno, DocInput, PageSpec, Payload, build};

fn text_page(label: &str, size: f32) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let content = format!(
        "BT /F1 {size} Tf 72 690 Td ({label}) Tj ET\n\
         BT /F1 14 Tf 72 660 Td (The quick brown fox jumps over the lazy dog.) Tj ET\n\
         BT /F1 14 Tf 72 640 Td (Use the highlight tool on this line to test annotations.) Tj ET"
    );
    let contents_id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Parent", Object::Reference(pages_id));
    page.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    page.set(
        "Resources",
        dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } },
    );
    page.set("Contents", Object::Reference(contents_id));
    let page_id = doc.add_object(page);
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.new_object_id();
    doc.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        }),
    );
    doc.trailer.set("Root", Object::Reference(catalog_id));

    // Round-trip so the example exercises the same bytes the app loads.
    let mut buffer = Vec::new();
    doc.save_to(&mut buffer).unwrap();
    Document::load_mem(&buffer).unwrap()
}

fn main() {
    let mut page_a = text_page("Sample page one", 28.0);
    let mut page_b = text_page("Sample page two (rotated 90 degrees)", 24.0);

    let a = general_purpose::STANDARD.encode({
        let mut b = Vec::new();
        page_a.save_to(&mut b).unwrap();
        b
    });
    let b = general_purpose::STANDARD.encode({
        let mut b = Vec::new();
        page_b.save_to(&mut b).unwrap();
        b
    });

    let payload = Payload {
        docs: vec![
            DocInput {
                id: "a".into(),
                data: a,
            },
            DocInput {
                id: "b".into(),
                data: b,
            },
        ],
        pages: vec![
            PageSpec {
                doc: "a".into(),
                index: 0,
                rotation: 0,
                crop: None,
            },
            PageSpec {
                doc: "b".into(),
                index: 0,
                rotation: 90,
                crop: None,
            },
        ],
        annotations: vec![
            Anno {
                page: 0,
                kind: "highlight".into(),
                color: vec![1.0, 0.84, 0.0],
                opacity: 0.35,
                width: 2.0,
                rect: Some([0.1, 0.145, 0.55, 0.025]),
                rects: None,
                points: None,
                text: Some("Remember to cite this passage.".into()),
                size: 12.0,
                image: None,
                label: None,
            },
            Anno {
                page: 0,
                kind: "square".into(),
                color: vec![0.85, 0.1, 0.1],
                opacity: 1.0,
                width: 2.5,
                rect: Some([0.1, 0.25, 0.45, 0.12]),
                rects: None,
                points: None,
                text: None,
                size: 12.0,
                image: None,
                label: None,
            },
            Anno {
                page: 0,
                kind: "ink".into(),
                color: vec![0.1, 0.35, 0.9],
                opacity: 1.0,
                width: 3.0,
                rect: None,
                rects: None,
                points: Some(vec![
                    [0.12, 0.42],
                    [0.2, 0.46],
                    [0.28, 0.41],
                    [0.36, 0.47],
                    [0.44, 0.42],
                ]),
                text: None,
                size: 12.0,
                image: None,
                label: None,
            },
            Anno {
                page: 0,
                kind: "text".into(),
                color: vec![0.1, 0.1, 0.1],
                opacity: 1.0,
                width: 2.0,
                rect: Some([0.1, 0.55, 0.4, 0.08]),
                rects: None,
                points: None,
                text: Some("Reviewed by PDFzen".into()),
                size: 14.0,
                image: None,
                label: None,
            },
            Anno {
                page: 1,
                kind: "highlight".into(),
                color: vec![0.2, 0.9, 0.4],
                opacity: 0.35,
                width: 2.0,
                rect: Some([0.08, 0.09, 0.62, 0.03]),
                rects: None,
                points: None,
                text: None,
                size: 12.0,
                image: None,
                label: None,
            },
        ],
        flatten: false,
        remove_metadata: false,
        info: None,
        watermark: None,
        page_numbers: false,
        bookmarks: vec![],
        security: None,
    };

    let bytes = build(payload).expect("build sample");
    let path = std::path::Path::new("../sample.pdf");
    std::fs::write(path, &bytes).expect("write sample.pdf");
    println!(
        "wrote {} ({} bytes)",
        path.canonicalize().unwrap_or(path.to_path_buf()).display(),
        bytes.len()
    );
}
