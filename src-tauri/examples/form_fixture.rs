//! Generates a small interactive AcroForm PDF used by the UI regression test.
//!
//! Run with: `cargo run --example form_fixture -- <output.pdf>`

use base64::{Engine as _, engine::general_purpose};
use pdfzen_lib::{DocInput, FormField, FormOption, PageSpec, Payload, blank, build};
use std::env;

fn field(kind: &str, name: &str, page: usize, rect: [f64; 4]) -> FormField {
    FormField {
        page,
        kind: kind.into(),
        name: name.into(),
        value: String::new(),
        values: Vec::new(),
        default_value: String::new(),
        default_values: Vec::new(),
        rect: Some(rect),
        read_only: false,
        required: false,
        multiline: false,
        max_len: None,
        comb: false,
        alignment: 0,
        font_size: 14.0,
        font: "helvetica".into(),
        color: vec![0.1, 0.12, 0.16],
        background: vec![1.0, 1.0, 1.0],
        border_color: vec![0.25, 0.4, 0.8],
        border_width: 1.0,
        tooltip: None,
        options: Vec::new(),
        combo: false,
        multi_select: false,
        checked: false,
        export_value: None,
        radio_group: None,
    }
}

fn main() {
    let output = env::args()
        .nth(1)
        .unwrap_or_else(|| "form-fixture.pdf".into());
    let source = blank(612.0, 792.0).expect("blank pdf");
    let mut forms = Vec::new();

    forms.push(field("text", "full_name", 0, [0.08, 0.10, 0.30, 0.05]));
    let mut notes = field("text", "notes", 0, [0.08, 0.18, 0.35, 0.12]);
    notes.multiline = true;
    forms.push(notes);

    let mut agree = field("checkbox", "agree", 0, [0.55, 0.10, 0.04, 0.04]);
    agree.export_value = Some("Yes".into());
    forms.push(agree);

    let mut plan_a = field("radio", "plan", 0, [0.55, 0.20, 0.04, 0.04]);
    plan_a.radio_group = Some("plan".into());
    plan_a.export_value = Some("Basic".into());
    forms.push(plan_a);
    let mut plan_b = field("radio", "plan", 0, [0.63, 0.20, 0.04, 0.04]);
    plan_b.radio_group = Some("plan".into());
    plan_b.export_value = Some("Pro".into());
    forms.push(plan_b);

    let mut country = field("choice", "country", 0, [0.08, 0.33, 0.25, 0.05]);
    country.combo = true;
    country.value = "ca".into();
    country.values = vec!["ca".into()];
    country.default_value = "ca".into();
    country.default_values = vec!["ca".into()];
    country.options = vec![
        FormOption {
            display: "Canada".into(),
            export: "ca".into(),
        },
        FormOption {
            display: "United States".into(),
            export: "us".into(),
        },
        FormOption {
            display: "Mexico".into(),
            export: "mx".into(),
        },
    ];
    forms.push(country);

    let mut tags = field("choice", "tags", 0, [0.08, 0.42, 0.30, 0.12]);
    tags.multi_select = true;
    tags.values = vec!["alpha".into(), "gamma".into()];
    tags.default_values = tags.values.clone();
    tags.options = vec![
        FormOption {
            display: "Alpha".into(),
            export: "alpha".into(),
        },
        FormOption {
            display: "Beta".into(),
            export: "beta".into(),
        },
        FormOption {
            display: "Gamma".into(),
            export: "gamma".into(),
        },
    ];
    forms.push(tags);

    forms.push(field("signature", "signature", 0, [0.08, 0.58, 0.35, 0.06]));
    forms.push(field("text", "email", 1, [0.08, 0.10, 0.35, 0.05]));

    let payload = Payload {
        docs: vec![DocInput {
            id: "blank".into(),
            data: general_purpose::STANDARD.encode(&source),
        }],
        pages: vec![
            PageSpec {
                doc: "blank".into(),
                index: 0,
                rotation: 0,
                crop: None,
            },
            PageSpec {
                doc: "blank".into(),
                index: 0,
                rotation: 0,
                crop: None,
            },
        ],
        annotations: Vec::new(),
        forms,
        flatten: false,
        remove_metadata: false,
        info: None,
        watermark: None,
        page_numbers: false,
        bookmarks: Vec::new(),
        security: None,
    };
    let pdf = build(payload).expect("build form fixture");
    std::fs::write(&output, pdf).expect("write form fixture");
    println!("{output}");
}
