//! PDF composition engine: takes a page list, page rotations, annotations and
//! document-level extras and produces a fresh PDF document.

use crate::security::SecurityInput;
use base64::{Engine as _, engine::general_purpose};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Cursor, Write};

#[derive(Deserialize)]
pub struct Payload {
    pub docs: Vec<DocInput>,
    pub pages: Vec<PageSpec>,
    #[serde(default)]
    pub annotations: Vec<Anno>,
    /// Interactive AcroForm widgets to recreate on the composed pages.
    #[serde(default)]
    pub forms: Vec<FormField>,
    /// Bake annotations into the page content and drop the /Annots array.
    #[serde(default)]
    pub flatten: bool,
    #[serde(default)]
    pub remove_metadata: bool,
    #[serde(default)]
    pub info: Option<InfoInput>,
    #[serde(default)]
    pub watermark: Option<Watermark>,
    #[serde(default)]
    pub page_numbers: bool,
    /// Bookmarks to write into the composed document's outline.
    #[serde(default)]
    pub bookmarks: Vec<BookmarkInput>,
    /// Optional AES-256 password protection and PDFzen integrity seal.
    #[serde(default)]
    pub security: Option<SecurityInput>,
}

#[derive(Deserialize)]
pub struct BookmarkInput {
    pub title: String,
    /// Zero based page index in the composed document.
    #[serde(default)]
    pub page: usize,
}

#[derive(Deserialize)]
pub struct DocInput {
    pub id: String,
    /// Base64 encoded PDF bytes.
    pub data: String,
}

#[derive(Deserialize)]
pub struct PageSpec {
    pub doc: String,
    /// Zero based page index inside the source document.
    pub index: u32,
    /// Total display rotation in degrees (0, 90, 180, 270).
    #[serde(default)]
    pub rotation: i64,
    /// Optional normalized crop box [x0, y0, x1, y1], origin top-left.
    #[serde(default)]
    pub crop: Option<[f64; 4]>,
}

#[derive(Deserialize)]
pub struct Anno {
    pub page: usize,
    pub kind: String,
    #[serde(default)]
    pub color: Vec<f64>,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default = "default_width")]
    pub width: f64,
    /// Normalized rect, origin top-left: [x, y, w, h].
    #[serde(default)]
    pub rect: Option<[f64; 4]>,
    /// Optional per-line rects for a text selection. The first rect is used
    /// by viewers that do not understand QuadPoints; all rects are emitted.
    #[serde(default)]
    pub rects: Option<Vec<[f64; 4]>>,
    /// Normalized points, origin top-left.
    #[serde(default)]
    pub points: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default = "default_font_size")]
    pub size: f64,
    /// Text box family: helvetica, times, or courier.
    #[serde(default = "default_font")]
    pub font: String,
    /// Base64 JPEG data for image stamps / signatures.
    #[serde(default)]
    pub image: Option<String>,
    /// Text shown on text stamps.
    #[serde(default)]
    pub label: Option<String>,
}

/// One widget from a fillable PDF. Widgets are rebuilt as standalone
/// AcroForm fields so page reordering/rotation/cropping keeps working.
#[derive(Deserialize, Clone)]
pub struct FormField {
    pub page: usize,
    /// text, checkbox, radio, or choice. Signature widgets are stored as text.
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub default_value: String,
    #[serde(default)]
    pub default_values: Vec<String>,
    /// Normalized rect, origin top-left: [x, y, w, h].
    #[serde(default)]
    pub rect: Option<[f64; 4]>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub multiline: bool,
    #[serde(default)]
    pub max_len: Option<u32>,
    #[serde(default)]
    pub comb: bool,
    /// 0 = left, 1 = center, 2 = right.
    #[serde(default)]
    pub alignment: u8,
    #[serde(default = "default_font_size")]
    pub font_size: f64,
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default)]
    pub color: Vec<f64>,
    #[serde(default)]
    pub background: Vec<f64>,
    #[serde(default)]
    pub border_color: Vec<f64>,
    #[serde(default)]
    pub border_width: f64,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub options: Vec<FormOption>,
    #[serde(default)]
    pub combo: bool,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub export_value: Option<String>,
    /// Stable grouping key for radio buttons; not written to the PDF.
    #[serde(default)]
    pub radio_group: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct FormOption {
    pub display: String,
    pub export: String,
}

fn default_opacity() -> f64 {
    0.35
}
fn default_width() -> f64 {
    2.0
}
fn default_font_size() -> f64 {
    12.0
}
fn default_font() -> String {
    "helvetica".into()
}

#[derive(Deserialize, Default)]
pub struct InfoInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub keywords: Option<String>,
    #[serde(default)]
    pub creator: Option<String>,
}

#[derive(Deserialize)]
pub struct Watermark {
    pub text: String,
    #[serde(default = "default_wm_size")]
    pub size: f64,
    #[serde(default = "default_wm_opacity")]
    pub opacity: f64,
    #[serde(default = "default_wm_angle")]
    pub angle: f64,
    #[serde(default)]
    pub color: Vec<f64>,
}

fn default_wm_size() -> f64 {
    54.0
}
fn default_wm_opacity() -> f64 {
    0.12
}
fn default_wm_angle() -> f64 {
    45.0
}

type IdMap = HashMap<ObjectId, ObjectId>;

/// Deep copy an object graph from `src` into `dst`, remapping references.
fn deep_copy(src: &Document, dst: &mut Document, obj: &Object, map: &mut IdMap) -> Object {
    match obj {
        Object::Reference(id) => {
            if let Some(new_id) = map.get(id) {
                return Object::Reference(*new_id);
            }
            let src_obj = match src.get_object(*id) {
                Ok(o) => o.clone(),
                Err(_) => return Object::Null,
            };
            // Reserve the id first so cycles terminate.
            let new_id: ObjectId = (dst.max_id + 1, 0);
            dst.max_id = new_id.0;
            map.insert(*id, new_id);
            let copied = deep_copy(src, dst, &src_obj, map);
            dst.objects.insert(new_id, copied);
            Object::Reference(new_id)
        }
        Object::Array(items) => Object::Array(
            items
                .iter()
                .map(|item| deep_copy(src, dst, item, map))
                .collect(),
        ),
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in dict.iter() {
                new_dict.set(key.clone(), deep_copy(src, dst, value, map));
            }
            Object::Dictionary(new_dict)
        }
        Object::Stream(stream) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in stream.dict.iter() {
                if key == b"Length" {
                    continue;
                }
                new_dict.set(key.clone(), deep_copy(src, dst, value, map));
            }
            Object::Stream(Stream::new(new_dict, stream.content.clone()))
        }
        other => other.clone(),
    }
}

/// Look up an inheritable page attribute (`Resources`, `MediaBox`, ...).
fn inherited(doc: &Document, page: &Dictionary, key: &[u8]) -> Option<Object> {
    let mut current = page.clone();
    for _ in 0..32 {
        if let Ok(value) = current.get(key) {
            return Some(value.clone());
        }
        match current.get(b"Parent") {
            Ok(Object::Reference(parent_id)) => match doc.get_dictionary(*parent_id) {
                Ok(parent) => current = parent.clone(),
                Err(_) => return None,
            },
            _ => return None,
        }
    }
    None
}

fn numbers(obj: &Object) -> Option<Vec<f64>> {
    match obj {
        Object::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Object::Integer(i) => out.push(*i as f64),
                    Object::Real(r) => out.push(*r as f64),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

fn media_box(raw: Option<Object>) -> [f64; 4] {
    if let Some(Object::Array(items)) = raw {
        if let Some(v) = numbers(&Object::Array(items)) {
            if v.len() == 4 {
                return [v[0], v[1], v[2], v[3]];
            }
        }
    }
    [0.0, 0.0, 612.0, 792.0]
}

fn color_of(anno: &Anno) -> (f64, f64, f64) {
    let get = |i: usize| anno.color.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
    (get(0), get(1), get(2))
}

/// PDF text string, using UTF-16BE when the text is not plain ASCII.
fn pdf_string(text: &str) -> Object {
    if text.is_ascii() {
        Object::string_literal(text.as_bytes().to_vec())
    } else {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        Object::String(bytes, lopdf::StringFormat::Literal)
    }
}

fn appearance(
    out: &mut Document,
    bbox: [f64; 4],
    content: String,
    extra_resources: Option<Dictionary>,
) -> ObjectId {
    let mut resources = Dictionary::new();
    if let Some(extra) = extra_resources {
        for (key, value) in extra.iter() {
            resources.set(key.clone(), value.clone());
        }
    }
    let dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "FormType" => 1,
        "BBox" => vec![
            Object::Real(bbox[0] as f32),
            Object::Real(bbox[1] as f32),
            Object::Real(bbox[2] as f32),
            Object::Real(bbox[3] as f32),
        ],
        "Resources" => Object::Dictionary(resources),
        "Group" => dictionary! {
            "S" => "Transparency",
            "CS" => "DeviceRGB",
        },
    };
    out.add_object(Stream::new(dict, content.into_bytes()))
}

fn font_resources(name: &str, font_id: ObjectId) -> Dictionary {
    let mut fonts = Dictionary::new();
    fonts.set(name, Object::Reference(font_id));
    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(fonts));
    resources
}

#[derive(Clone, Copy)]
struct AnnotationFonts {
    helvetica: ObjectId,
    times: ObjectId,
    courier: ObjectId,
}

impl AnnotationFonts {
    fn for_font(self, name: &str) -> (&'static str, ObjectId) {
        let font = name.trim().to_ascii_lowercase();
        if font.contains("sans") {
            ("Helv", self.helvetica)
        } else if font.contains("times") || font.contains("serif") || font == "tiro" {
            ("TiRo", self.times)
        } else if font.contains("cour") || font.contains("mono") {
            ("Cour", self.courier)
        } else {
            ("Helv", self.helvetica)
        }
    }

    fn for_annotation(self, anno: &Anno) -> (&'static str, ObjectId) {
        self.for_font(&anno.font)
    }
}

fn escape_pdf_text(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\\' => out.push_str("\\\\"),
            '\n' => {}
            '\r' => {}
            c if c.is_ascii() => out.push(c),
            // WinAnsi can represent the first 256 code points. Octal escapes
            // keep the content stream ASCII while preserving accented text.
            c if (c as u32) <= 0xFF => out.push_str(&format!("\\{:03o}", c as u32)),
            _ => out.push('?'),
        }
    }
    out
}

fn form_color(values: &[f64], fallback: (f64, f64, f64)) -> (f64, f64, f64) {
    if values.len() < 3 {
        return fallback;
    }
    (
        values[0].clamp(0.0, 1.0),
        values[1].clamp(0.0, 1.0),
        values[2].clamp(0.0, 1.0),
    )
}

fn optional_form_color(values: &[f64]) -> Option<(f64, f64, f64)> {
    if values.len() < 3 {
        None
    } else {
        Some((
            values[0].clamp(0.0, 1.0),
            values[1].clamp(0.0, 1.0),
            values[2].clamp(0.0, 1.0),
        ))
    }
}

fn form_project_rect(raw: [f64; 4], page_w: f64, page_h: f64) -> [f64; 4] {
    let x0 = raw[0].clamp(0.0, 1.0) * page_w;
    let x1 = (raw[0] + raw[2].abs().max(0.001)).clamp(0.0, 1.0) * page_w;
    let y0 = (1.0 - raw[1].clamp(0.0, 1.0)) * page_h;
    let y1 = (1.0 - (raw[1] + raw[3].abs().max(0.001)).clamp(0.0, 1.0)) * page_h;
    [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]
}

fn form_state_name(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() { "Yes".into() } else { out }
}

fn unique_form_state(used: &mut HashMap<String, usize>, raw: &str) -> String {
    let base = form_state_name(raw);
    let count = used.entry(base.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base
    } else {
        format!("{base}_{}", *count)
    }
}

fn unique_form_name(used: &mut HashMap<String, usize>, raw: &str) -> String {
    let base = raw.trim();
    let base = if base.is_empty() { "Field" } else { base };
    let count = used.entry(base.to_string()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base.to_string()
    } else {
        format!("{base}_{}", *count)
    }
}

fn form_text_value(field: &FormField) -> String {
    if field.kind == "choice" {
        let values = if !field.values.is_empty() {
            field.values.clone()
        } else if field.value.is_empty() {
            Vec::new()
        } else {
            vec![field.value.clone()]
        };
        let mut labels = Vec::with_capacity(values.len());
        for value in values {
            let label = field
                .options
                .iter()
                .find(|option| option.export == value)
                .map(|option| option.display.clone())
                .unwrap_or(value);
            labels.push(label);
        }
        labels.join(", ")
    } else {
        field.value.replace(['\r', '\n'], " ")
    }
}

fn field_text_lines(field: &FormField, max_chars: usize) -> Vec<String> {
    let raw = if field.kind == "choice" {
        form_text_value(field)
    } else {
        field.value.clone()
    };
    let mut lines = Vec::new();
    for paragraph in raw.replace('\r', "").split('\n') {
        if !field.multiline {
            lines.push(paragraph.to_string());
            continue;
        }
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut remaining = paragraph.to_string();
        while remaining.chars().count() > max_chars {
            let mut split = 0usize;
            let mut last_space = None;
            for (idx, ch) in remaining.char_indices() {
                if idx >= max_chars {
                    break;
                }
                if ch == ' ' {
                    last_space = Some(idx);
                }
                split = idx + ch.len_utf8();
            }
            let at = last_space.filter(|idx| *idx > 0).unwrap_or(split);
            let (line, rest) = remaining.split_at(at);
            lines.push(line.trim_end().to_string());
            remaining = rest.trim_start().to_string();
        }
        lines.push(remaining);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn form_background_content(
    width: f64,
    height: f64,
    background: Option<(f64, f64, f64)>,
    border: Option<(f64, f64, f64)>,
    border_width: f64,
) -> String {
    let mut content = String::new();
    if let Some((r, g, b)) = background {
        content.push_str(&format!(
            "q {r:.4} {g:.4} {b:.4} rg 0 0 {width:.3} {height:.3} re f Q "
        ));
    }
    if let Some((r, g, b)) = border {
        let lw = border_width.clamp(0.5, 4.0);
        let inset = lw / 2.0;
        content.push_str(&format!(
            "q {r:.4} {g:.4} {b:.4} RG {lw:.3} w {x:.3} {y:.3} {w:.3} {h:.3} re S Q ",
            x = inset,
            y = inset,
            w = (width - lw).max(0.1),
            h = (height - lw).max(0.1),
        ));
    }
    content
}

fn circle_path(cx: f64, cy: f64, radius: f64) -> String {
    let k = radius * 0.552_284_749_8;
    format!(
        "{x0:.3} {y:.3} m {x1:.3} {y1:.3} {x2:.3} {y2:.3} {x:.3} {y2:.3} c \
         {x3:.3} {y2:.3} {x4:.3} {y1:.3} {x4:.3} {y:.3} c \
         {x4:.3} {y3:.3} {x3:.3} {y4:.3} {x:.3} {y4:.3} c \
         {x2:.3} {y4:.3} {x1:.3} {y3:.3} {x0:.3} {y:.3} c",
        x0 = cx + radius,
        x1 = cx + radius,
        x2 = cx + k,
        x3 = cx - k,
        x4 = cx - radius,
        y = cy,
        y1 = cy + k,
        y2 = cy + radius,
        y3 = cy - k,
        y4 = cy - radius,
        x = cx,
    )
}

fn button_appearance_content(width: f64, height: f64, field: &FormField, checked: bool) -> String {
    let background = optional_form_color(&field.background);
    let border = optional_form_color(&field.border_color);
    let mut content = form_background_content(
        width,
        height,
        background,
        border,
        if field.border_width > 0.0 {
            field.border_width
        } else {
            1.0
        },
    );
    if !checked {
        return content;
    }
    let color = form_color(&field.color, (0.1, 0.2, 0.45));
    let size = width.min(height).max(1.0);
    let lw = (size * 0.12).clamp(1.0, 3.0);
    if field.kind == "radio" {
        content.push_str(&format!(
            "q {r:.4} {g:.4} {b:.4} rg {} f Q",
            circle_path(width / 2.0, height / 2.0, size * 0.23),
            r = color.0,
            g = color.1,
            b = color.2,
        ));
    } else {
        let x0 = width * 0.18;
        let y0 = height * 0.50;
        let x1 = width * 0.42;
        let y1 = height * 0.20;
        let x2 = width * 0.84;
        let y2 = height * 0.80;
        content.push_str(&format!(
            "q {r:.4} {g:.4} {b:.4} RG {lw:.3} w 1 J {x0:.3} {y0:.3} m {x1:.3} {y1:.3} l {x2:.3} {y2:.3} l S Q",
            r = color.0,
            g = color.1,
            b = color.2,
        ));
    }
    content
}

fn form_text_appearance(
    out: &mut Document,
    field: &FormField,
    width: f64,
    height: f64,
    fonts: AnnotationFonts,
) -> ObjectId {
    let (font_key, font_id) = fonts.for_font(&field.font);
    let (r, g, b) = form_color(&field.color, (0.1, 0.1, 0.1));
    let background = optional_form_color(&field.background);
    let border = optional_form_color(&field.border_color);
    let size = field.font_size.clamp(4.0, 96.0);
    let pad = (size * 0.18).clamp(1.0, 4.0);
    let inner_w = (width - pad * 2.0).max(1.0);
    let inner_h = (height - pad * 2.0).max(1.0);
    let mut content =
        form_background_content(width, height, background, border, field.border_width);
    let text = form_text_value(field);
    if !text.is_empty() {
        if field.comb && field.max_len.unwrap_or(0) > 0 {
            let cells = field.max_len.unwrap_or(1).max(1) as f64;
            let cell_w = width / cells;
            let font_size = size.min(cell_w * 0.68).min(inner_h * 0.75).max(4.0);
            let base_y = (height - font_size * 0.72) / 2.0;
            content.push_str("q 0.45 0.45 0.45 RG 0.35 w ");
            for i in 1..cells as usize {
                let x = cell_w * i as f64;
                content.push_str(&format!("{x:.3} 1 m {x:.3} {} l S ", height - 1.0));
            }
            content.push_str("Q ");
            content.push_str(&format!(
                "q 0 0 {width:.3} {height:.3} re W n BT /{font_key} {font_size:.3} Tf {r:.4} {g:.4} {b:.4} rg "
            ));
            for (i, ch) in text
                .chars()
                .take(field.max_len.unwrap_or(0) as usize)
                .enumerate()
            {
                let ch_width = font_size * 0.52;
                let x = cell_w * i as f64 + (cell_w - ch_width) / 2.0;
                content.push_str(&format!(
                    "1 0 0 1 {x:.3} {base_y:.3} Tm ({t}) Tj ",
                    t = escape_pdf_text(&ch.to_string())
                ));
            }
            content.push_str("ET Q");
        } else {
            let max_chars = ((inner_w / (size * 0.52)).floor() as usize).max(1);
            let lines = field_text_lines(field, max_chars);
            let line_h = size * 1.2;
            let visible = ((inner_h / line_h).floor() as usize).max(1);
            let start = if lines.len() > visible {
                lines.len() - visible
            } else {
                0
            };
            let base_y = if field.multiline {
                height - pad - size * 0.8
            } else {
                (height - size * 0.72) / 2.0
            };
            content.push_str(&format!(
                "q 0 0 {width:.3} {height:.3} re W n BT /{font_key} {size:.3} Tf {r:.4} {g:.4} {b:.4} rg "
            ));
            for (index, line) in lines.iter().skip(start).enumerate() {
                let text_w = line.chars().count() as f64 * size * 0.52;
                let x = match field.alignment {
                    1 => (width - text_w) / 2.0,
                    2 => width - pad - text_w,
                    _ => pad,
                };
                let y = base_y - index as f64 * line_h;
                content.push_str(&format!(
                    "1 0 0 1 {x:.3} {y:.3} Tm ({t}) Tj ",
                    t = escape_pdf_text(line)
                ));
            }
            content.push_str("ET Q");
        }
    }
    appearance(
        out,
        [0.0, 0.0, width, height],
        content,
        Some(font_resources(font_key, font_id)),
    )
}

fn form_widget_rect(field: &FormField, page_w: f64, page_h: f64) -> [f64; 4] {
    form_project_rect(
        field.rect.unwrap_or([0.05, 0.05, 0.2, 0.04]),
        page_w,
        page_h,
    )
}

fn form_widget_dictionary(
    field: &FormField,
    name: &str,
    rect: [f64; 4],
    page_id: ObjectId,
    appearance_id: ObjectId,
    fonts: AnnotationFonts,
) -> Dictionary {
    let (font_key, _) = fonts.for_font(&field.font);
    let (r, g, b) = form_color(&field.color, (0.1, 0.1, 0.1));
    let size = field.font_size.clamp(4.0, 96.0);
    let da = format!("/{font_key} {size:.3} Tf {r:.4} {g:.4} {b:.4} rg");
    let mut dict = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Tx",
        "Rect" => Object::Array(rect.iter().map(|value| Object::Real(*value as f32)).collect()),
        "T" => pdf_string(name),
        "F" => Object::Integer(4),
        "P" => Object::Reference(page_id),
        "DA" => Object::string_literal(da.into_bytes()),
        "Q" => Object::Integer(field.alignment.min(2) as i64),
        "V" => pdf_string(&field.value),
        "DV" => pdf_string(&field.default_value),
        "AP" => dictionary! { "N" => Object::Reference(appearance_id) },
    };
    let mut flags = 0i64;
    if field.read_only {
        flags |= 1;
    }
    if field.required {
        flags |= 2;
    }
    if field.multiline {
        flags |= 1 << 12;
    }
    if field.comb {
        flags |= 1 << 24;
    }
    if flags != 0 {
        dict.set("Ff", Object::Integer(flags));
    }
    if let Some(max_len) = field.max_len.filter(|value| *value > 0) {
        dict.set("MaxLen", Object::Integer(max_len as i64));
    }
    if let Some(tooltip) = field
        .tooltip
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        dict.set("TU", pdf_string(tooltip.trim()));
    }
    let background = optional_form_color(&field.background);
    let border = optional_form_color(&field.border_color);
    if background.is_some() || border.is_some() {
        let mut mk = Dictionary::new();
        if let Some((r, g, b)) = background {
            mk.set(
                "BG",
                Object::Array(vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ]),
            );
        }
        if let Some((r, g, b)) = border {
            mk.set(
                "BC",
                Object::Array(vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ]),
            );
        }
        dict.set("MK", Object::Dictionary(mk));
    }
    dict
}

fn form_choice_widget_dictionary(
    field: &FormField,
    name: &str,
    rect: [f64; 4],
    page_id: ObjectId,
    appearance_id: ObjectId,
    fonts: AnnotationFonts,
) -> Dictionary {
    let mut dict = form_widget_dictionary(field, name, rect, page_id, appearance_id, fonts);
    dict.set("FT", Object::Name(b"Ch".to_vec()));
    let options: Vec<Object> = field
        .options
        .iter()
        .map(|option| {
            if option.display == option.export {
                pdf_string(&option.export)
            } else {
                Object::Array(vec![
                    pdf_string(&option.export),
                    pdf_string(&option.display),
                ])
            }
        })
        .collect();
    if !options.is_empty() {
        dict.set("Opt", Object::Array(options));
    }
    let values = if !field.values.is_empty() {
        field.values.clone()
    } else if field.value.is_empty() {
        Vec::new()
    } else {
        vec![field.value.clone()]
    };
    if values.len() > 1 {
        dict.set(
            "V",
            Object::Array(values.iter().map(|value| pdf_string(value)).collect()),
        );
    } else if let Some(value) = values.first() {
        dict.set("V", pdf_string(value));
    } else {
        dict.set("V", pdf_string(""));
    }
    let mut flags = if field.read_only { 1 } else { 0 };
    if field.required {
        flags |= 2;
    }
    if field.combo {
        flags |= 1 << 17;
    }
    if field.multi_select {
        flags |= 1 << 21;
    }
    if flags != 0 {
        dict.set("Ff", Object::Integer(flags));
    }
    dict
}

fn add_form_fields(
    out: &mut Document,
    forms: &[FormField],
    page_ids: &[ObjectId],
    page_sizes: &[(f64, f64)],
    fonts: AnnotationFonts,
    catalog_id: ObjectId,
) {
    if forms.is_empty() || page_ids.is_empty() {
        return;
    }
    let mut roots: Vec<Object> = Vec::new();
    let mut page_annots: Vec<Vec<Object>> = vec![Vec::new(); page_ids.len()];
    let mut used_names: HashMap<String, usize> = HashMap::new();

    let mut radio_groups: Vec<(String, Vec<&FormField>)> = Vec::new();
    for field in forms.iter().filter(|field| {
        field.kind == "radio" && field.page < page_ids.len() && field.rect.is_some()
    }) {
        let key = field
            .radio_group
            .as_deref()
            .unwrap_or(&field.name)
            .to_string();
        if let Some((_, group)) = radio_groups.iter_mut().find(|(name, _)| *name == key) {
            group.push(field);
        } else {
            radio_groups.push((key, vec![field]));
        }
    }

    for (_, group) in radio_groups {
        let first = group[0];
        let name = unique_form_name(&mut used_names, &first.name);
        let mut flags = 1i64 << 15;
        if first.read_only {
            flags |= 1;
        }
        if first.required {
            flags |= 2;
        }
        let parent_id = out.new_object_id();
        let mut kids = Vec::new();
        let mut used_states = HashMap::new();
        let mut selected_state = "Off".to_string();
        for field in group {
            let (page_w, page_h) = page_sizes[field.page];
            let rect = form_widget_rect(field, page_w, page_h);
            let width = (rect[2] - rect[0]).max(1.0);
            let height = (rect[3] - rect[1]).max(1.0);
            let state = unique_form_state(
                &mut used_states,
                field.export_value.as_deref().unwrap_or("Yes"),
            );
            let off_id = appearance(
                out,
                [0.0, 0.0, width, height],
                button_appearance_content(width, height, field, false),
                None,
            );
            let on_id = appearance(
                out,
                [0.0, 0.0, width, height],
                button_appearance_content(width, height, field, true),
                None,
            );
            let mut states = Dictionary::new();
            states.set("Off", Object::Reference(off_id));
            states.set(state.as_bytes().to_vec(), Object::Reference(on_id));
            let mut widget = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Widget",
                "Rect" => Object::Array(rect.iter().map(|value| Object::Real(*value as f32)).collect()),
                "F" => Object::Integer(4),
                "P" => Object::Reference(page_ids[field.page]),
                "Parent" => Object::Reference(parent_id),
                "AS" => Object::Name(if field.checked { state.clone() } else { "Off".into() }.into_bytes()),
                "AP" => dictionary! { "N" => Object::Dictionary(states) },
            };
            if let Some(tooltip) = field
                .tooltip
                .as_ref()
                .filter(|value| !value.trim().is_empty())
            {
                widget.set("TU", pdf_string(tooltip.trim()));
            }
            if field.checked {
                selected_state = state;
            }
            let widget_id = out.add_object(Object::Dictionary(widget));
            kids.push(Object::Reference(widget_id));
            page_annots[field.page].push(Object::Reference(widget_id));
        }
        if kids.is_empty() {
            continue;
        }
        let mut parent = dictionary! {
            "FT" => "Btn",
            "T" => pdf_string(&name),
            "Ff" => Object::Integer(flags),
            "V" => Object::Name(selected_state.clone().into_bytes()),
            "DV" => Object::Name(selected_state.into_bytes()),
            "Kids" => Object::Array(kids),
        };
        if let Some(tooltip) = first
            .tooltip
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            parent.set("TU", pdf_string(tooltip.trim()));
        }
        out.objects.insert(parent_id, Object::Dictionary(parent));
        roots.push(Object::Reference(parent_id));
    }

    for field in forms.iter().filter(|field| {
        field.kind != "radio" && field.page < page_ids.len() && field.rect.is_some()
    }) {
        let (page_w, page_h) = page_sizes[field.page];
        let rect = form_widget_rect(field, page_w, page_h);
        let width = (rect[2] - rect[0]).max(1.0);
        let height = (rect[3] - rect[1]).max(1.0);
        let name = unique_form_name(&mut used_names, &field.name);
        if field.kind == "checkbox" {
            let state = unique_form_state(
                &mut HashMap::new(),
                field.export_value.as_deref().unwrap_or("Yes"),
            );
            let off_id = appearance(
                out,
                [0.0, 0.0, width, height],
                button_appearance_content(width, height, field, false),
                None,
            );
            let on_id = appearance(
                out,
                [0.0, 0.0, width, height],
                button_appearance_content(width, height, field, true),
                None,
            );
            let mut states = Dictionary::new();
            states.set("Off", Object::Reference(off_id));
            states.set(state.as_bytes().to_vec(), Object::Reference(on_id));
            let mut flags = 0i64;
            if field.read_only {
                flags |= 1;
            }
            if field.required {
                flags |= 2;
            }
            let mut widget = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Widget",
                "FT" => "Btn",
                "Rect" => Object::Array(rect.iter().map(|value| Object::Real(*value as f32)).collect()),
                "T" => pdf_string(&name),
                "F" => Object::Integer(4),
                "P" => Object::Reference(page_ids[field.page]),
                "V" => Object::Name(if field.checked { state.clone() } else { "Off".into() }.into_bytes()),
                "AS" => Object::Name(if field.checked { state } else { "Off".into() }.into_bytes()),
                "Ff" => Object::Integer(flags),
                "AP" => dictionary! { "N" => Object::Dictionary(states) },
            };
            if let Some(tooltip) = field
                .tooltip
                .as_ref()
                .filter(|value| !value.trim().is_empty())
            {
                widget.set("TU", pdf_string(tooltip.trim()));
            }
            let widget_id = out.add_object(Object::Dictionary(widget));
            page_annots[field.page].push(Object::Reference(widget_id));
            roots.push(Object::Reference(widget_id));
            continue;
        }
        let appearance_id = form_text_appearance(out, field, width, height, fonts);
        let widget = if field.kind == "choice" {
            form_choice_widget_dictionary(
                field,
                &name,
                rect,
                page_ids[field.page],
                appearance_id,
                fonts,
            )
        } else {
            form_widget_dictionary(
                field,
                &name,
                rect,
                page_ids[field.page],
                appearance_id,
                fonts,
            )
        };
        let widget_id = out.add_object(Object::Dictionary(widget));
        page_annots[field.page].push(Object::Reference(widget_id));
        roots.push(Object::Reference(widget_id));
    }

    for (index, annots) in page_annots.into_iter().enumerate() {
        if annots.is_empty() {
            continue;
        }
        if let Ok(page) = out.get_dictionary_mut(page_ids[index]) {
            let mut current = match page.get(b"Annots") {
                Ok(Object::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            current.extend(annots);
            page.set("Annots", Object::Array(current));
        }
    }

    if roots.is_empty() {
        return;
    }
    let mut fonts_dict = Dictionary::new();
    fonts_dict.set("Helv", Object::Reference(fonts.helvetica));
    fonts_dict.set("TiRo", Object::Reference(fonts.times));
    fonts_dict.set("Cour", Object::Reference(fonts.courier));
    let acro_id = out.add_object(dictionary! {
        "Fields" => Object::Array(roots),
        "DA" => Object::string_literal(b"/Helv 0 Tf 0 g".to_vec()),
        "DR" => dictionary! { "Font" => Object::Dictionary(fonts_dict) },
        "NeedAppearances" => Object::Boolean(false),
    });
    if let Some(Object::Dictionary(catalog)) = out.objects.get_mut(&catalog_id) {
        catalog.set("AcroForm", Object::Reference(acro_id));
    }
}

fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = ((data[i + 2] as usize) << 8) | data[i + 3] as usize;
        if len < 2 {
            return None;
        }
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let h = ((data[i + 5] as u32) << 8) | data[i + 6] as u32;
            let w = ((data[i + 7] as u32) << 8) | data[i + 8] as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

/// Build the annotation dictionary for one annotation.
fn annotation_object(
    out: &mut Document,
    anno: &Anno,
    page_w: f64,
    page_h: f64,
    fonts: AnnotationFonts,
) -> Object {
    let (r, g, b) = color_of(anno);
    let raw = anno.rect.unwrap_or([0.0, 0.0, 0.02, 0.02]);
    let normalized_rects = anno
        .rects
        .as_ref()
        .filter(|rects| !rects.is_empty())
        .cloned()
        .unwrap_or_else(|| vec![raw]);

    // Convert every normalized top-left rect to a PDF page-space rect with a
    // bottom-left origin. A text highlight can contain one rect per selected
    // line, so the annotation Rect is the union while QuadPoints/AP retain the
    // individual lines.
    let abs_rects: Vec<[f64; 4]> = normalized_rects
        .iter()
        .map(|rect| {
            let x0 = rect[0] * page_w;
            let x1 = (rect[0] + rect[2].abs().max(0.004)) * page_w;
            let y_top = page_h - rect[1] * page_h;
            let y_bottom = page_h - (rect[1] + rect[3].abs().max(0.004)) * page_h;
            [
                x0.min(x1),
                y_bottom.min(y_top),
                x0.max(x1),
                y_bottom.max(y_top),
            ]
        })
        .collect();

    let left = abs_rects.iter().map(|r| r[0]).fold(f64::MAX, f64::min);
    let bottom = abs_rects.iter().map(|r| r[1]).fold(f64::MAX, f64::min);
    let right = abs_rects.iter().map(|r| r[2]).fold(f64::MIN, f64::max);
    let top = abs_rects.iter().map(|r| r[3]).fold(f64::MIN, f64::max);
    let width = (right - left).max(1.0);
    let height = (top - bottom).max(1.0);

    let line_width = anno.width.max(0.2);
    let rect_objects = vec![
        Object::Real(left as f32),
        Object::Real(bottom as f32),
        Object::Real(right as f32),
        Object::Real(top as f32),
    ];
    let mut quad_points = Vec::with_capacity(abs_rects.len() * 8);
    for [al, ab, ar, at] in &abs_rects {
        quad_points.extend([
            Object::Real(*al as f32),
            Object::Real(*at as f32),
            Object::Real(*ar as f32),
            Object::Real(*at as f32),
            Object::Real(*al as f32),
            Object::Real(*ab as f32),
            Object::Real(*ar as f32),
            Object::Real(*ab as f32),
        ]);
    }

    match anno.kind.as_str() {
        "highlight" => {
            let gs = dictionary! {
                "GS0" => dictionary! {
                    "Type" => "ExtGState",
                    "ca" => Object::Real(anno.opacity.clamp(0.05, 1.0) as f32),
                    "CA" => Object::Real(anno.opacity.clamp(0.05, 1.0) as f32),
                },
            };
            let mut content = format!("q /GS0 gs {r} {g} {b} rg ");
            for [al, ab, ar, at] in &abs_rects {
                content.push_str(&format!(
                    "{} {} {} {} re f ",
                    al - left,
                    ab - bottom,
                    (ar - al).max(0.1),
                    (at - ab).max(0.1)
                ));
            }
            content.push_str("Q");
            let ap = appearance(out, [0.0, 0.0, width, height], content, Some(gs));
            let mut dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Highlight",
                "Rect" => rect_objects,
                "QuadPoints" => quad_points,
                "C" => vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ],
                "CA" => Object::Real(anno.opacity.clamp(0.05, 1.0) as f32),
                "F" => 4,
                "AP" => dictionary! { "N" => Object::Reference(ap) },
            };
            if let Some(text) = anno
                .text
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                dict.set("Contents", pdf_string(text));
                dict.set("T", pdf_string("PDFzen"));
            }
            Object::Dictionary(dict)
        }
        "underline" | "strikeout" | "squiggly" => {
            let (subtype, content) = match anno.kind.as_str() {
                "underline" => {
                    let y = (height * 0.08).max(1.0);
                    (
                        "Underline",
                        format!(
                            "q {r} {g} {b} RG {lw} w 0 {y} m {w} {y} l S Q",
                            r = r,
                            g = g,
                            b = b,
                            lw = (height * 0.06).clamp(0.8, 3.0),
                            y = y,
                            w = width
                        ),
                    )
                }
                "strikeout" => {
                    let y = height * 0.5;
                    (
                        "StrikeOut",
                        format!(
                            "q {r} {g} {b} RG {lw} w 0 {y} m {w} {y} l S Q",
                            r = r,
                            g = g,
                            b = b,
                            lw = (height * 0.06).clamp(0.8, 3.0),
                            y = y,
                            w = width
                        ),
                    )
                }
                _ => {
                    let amp = (height * 0.18).max(1.0);
                    let step = (width / 16.0).max(1.0);
                    let mut path = String::new();
                    let mut x = 0.0;
                    let mut up = true;
                    path.push_str(&format!("0 {} m ", height * 0.5));
                    while x < width {
                        x = (x + step).min(width);
                        let y = if up {
                            height * 0.5 + amp
                        } else {
                            height * 0.5 - amp
                        };
                        path.push_str(&format!("{x} {y} l "));
                        up = !up;
                    }
                    (
                        "Squiggly",
                        format!(
                            "q {r} {g} {b} RG {lw} w 1 J {path}S Q",
                            r = r,
                            g = g,
                            b = b,
                            lw = (height * 0.05).clamp(0.8, 2.4),
                            path = path
                        ),
                    )
                }
            };
            let ap = appearance(out, [0.0, 0.0, width, height], content, None);
            let dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => subtype,
                "Rect" => rect_objects,
                "QuadPoints" => quad_points,
                "C" => vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ],
                "F" => 4,
                "AP" => dictionary! { "N" => Object::Reference(ap) },
            };
            Object::Dictionary(dict)
        }
        "ink" => {
            let points = anno.points.clone().unwrap_or_default();
            if points.len() < 2 {
                return Object::Null;
            }
            // Absolute page-space points (origin bottom-left).
            let abs: Vec<(f64, f64)> = points
                .iter()
                .map(|p| (p[0] * page_w, page_h - p[1] * page_h))
                .collect();
            let (mut min_x, mut min_y) = (f64::MAX, f64::MAX);
            let (mut max_x, mut max_y) = (f64::MIN, f64::MIN);
            for (x, y) in &abs {
                min_x = min_x.min(*x);
                min_y = min_y.min(*y);
                max_x = max_x.max(*x);
                max_y = max_y.max(*y);
            }
            let pad = line_width + 1.0;
            // Annotation rect in page space; the form BBox maps onto it.
            let ink_rect = [
                min_x - pad,
                min_y - pad,
                (max_x + pad).max(min_x + 1.0),
                (max_y + pad).max(min_y + 1.0),
            ];
            let form_box = [
                0.0,
                0.0,
                ink_rect[2] - ink_rect[0],
                ink_rect[3] - ink_rect[1],
            ];
            let pts: Vec<(f64, f64)> = abs
                .iter()
                .map(|(x, y)| (x - ink_rect[0], y - ink_rect[1]))
                .collect();
            let mut content = format!(
                "q {r} {g} {b} RG {lw} w 1 J 1 j ",
                r = r,
                g = g,
                b = b,
                lw = line_width
            );
            for (i, (x, y)) in pts.iter().enumerate() {
                let ox = x - form_box[0];
                let oy = y - form_box[1];
                if i == 0 {
                    content.push_str(&format!("{ox} {oy} m "));
                } else {
                    content.push_str(&format!("{ox} {oy} l "));
                }
            }
            content.push_str("S Q");
            let ap = appearance(out, form_box, content, None);
            let ink_list = abs
                .iter()
                .flat_map(|(x, y)| vec![Object::Real(*x as f32), Object::Real(*y as f32)])
                .collect::<Vec<_>>();
            let dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Ink",
                "Rect" => vec![
                    Object::Real(ink_rect[0] as f32),
                    Object::Real(ink_rect[1] as f32),
                    Object::Real(ink_rect[2] as f32),
                    Object::Real(ink_rect[3] as f32),
                ],
                "InkList" => vec![Object::Array(ink_list)],
                "C" => vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ],
                "Border" => vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Real(line_width as f32),
                ],
                "F" => 4,
                "AP" => dictionary! { "N" => Object::Reference(ap) },
            };
            Object::Dictionary(dict)
        }
        "square" | "redact" => {
            let is_redact = anno.kind == "redact";
            let (rr, gg, bb) = if is_redact {
                (0.0, 0.0, 0.0)
            } else {
                (r, g, b)
            };
            let content = if is_redact {
                format!("q 0 0 0 rg 0 0 {w} {h} re f Q", w = width, h = height)
            } else {
                format!(
                    "q {r} {g} {b} RG {lw} w {ix} {iy} {iw} {ih} re S Q",
                    r = rr,
                    g = gg,
                    b = bb,
                    lw = line_width,
                    ix = line_width / 2.0,
                    iy = line_width / 2.0,
                    iw = (width - line_width).max(1.0),
                    ih = (height - line_width).max(1.0)
                )
            };
            let ap = appearance(out, [0.0, 0.0, width, height], content, None);
            let dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Square",
                "Rect" => rect_objects,
                "C" => vec![
                    Object::Real(rr as f32),
                    Object::Real(gg as f32),
                    Object::Real(bb as f32),
                ],
                "IC" => vec![
                    Object::Real(rr as f32),
                    Object::Real(gg as f32),
                    Object::Real(bb as f32),
                ],
                "Border" => vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Real(line_width as f32),
                ],
                "F" => 4,
                "AP" => dictionary! { "N" => Object::Reference(ap) },
            };
            Object::Dictionary(dict)
        }
        "text" => {
            let text = anno.text.clone().unwrap_or_default();
            let font_size = anno.size.clamp(4.0, 96.0);
            let (font_key, font_id) = fonts.for_annotation(anno);
            let lead = font_size * 1.2;
            let mut content = format!("q BT /{font_key} {font_size} Tf {r} {g} {b} rg ");
            for (i, line) in text.lines().enumerate() {
                // Position the first baseline so the glyph top line sits at
                // the top edge of the annotation box, matching the in-page
                // editor (1.2 line height, 0.1 em half-leading).
                let y = height - font_size * 0.8 - (i as f64 * lead);
                content.push_str(&format!(
                    "1 0 0 1 0 {y} Tm ({t}) Tj ",
                    y = y,
                    t = escape_pdf_text(line)
                ));
            }
            content.push_str("ET Q");
            let ap = appearance(
                out,
                [0.0, 0.0, width, height],
                content,
                Some(font_resources(font_key, font_id)),
            );
            let da = format!("/{font_key} {size} Tf {r} {g} {b} rg", size = font_size);
            let dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => "FreeText",
                "Rect" => rect_objects,
                "Contents" => pdf_string(&text),
                "DA" => Object::string_literal(da.as_bytes()),
                "C" => vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ],
                "Border" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(0)],
                "F" => 4,
                "AP" => dictionary! { "N" => Object::Reference(ap) },
            };
            Object::Dictionary(dict)
        }
        "note" => {
            let text = anno.text.clone().unwrap_or_default();
            let dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Text",
                "Name" => "Comment",
                "Rect" => rect_objects,
                "Contents" => pdf_string(&text),
                "C" => vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ],
                "Open" => false,
                "F" => 4,
            };
            Object::Dictionary(dict)
        }
        "stamp" => {
            let label = anno
                .label
                .clone()
                .or_else(|| anno.text.clone())
                .unwrap_or_else(|| "APPROVED".to_string());
            let len = label.chars().count().max(1) as f64;
            let font_size = (height * 0.42)
                .min(width * 0.86 / (len * 0.6))
                .clamp(6.0, 72.0);
            let approx = len * font_size * 0.56;
            let x = ((width - approx) / 2.0).max(4.0);
            let y = (height - font_size) / 2.0;
            let content = format!(
                "q {r} {g} {b} RG 2 w 1 1 {w} {h} re S BT /Helv {size} Tf {r} {g} {b} rg 1 0 0 1 {x} {y} Tm ({t}) Tj ET Q",
                r = r,
                g = g,
                b = b,
                w = (width - 2.0).max(1.0),
                h = (height - 2.0).max(1.0),
                size = font_size,
                x = x,
                y = y,
                t = escape_pdf_text(&label),
            );
            let ap = appearance(
                out,
                [0.0, 0.0, width, height],
                content,
                Some(font_resources("Helv", fonts.helvetica)),
            );
            let dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Stamp",
                "Name" => "Draft",
                "Rect" => rect_objects,
                "Contents" => pdf_string(&label),
                "C" => vec![
                    Object::Real(r as f32),
                    Object::Real(g as f32),
                    Object::Real(b as f32),
                ],
                "F" => 4,
                "AP" => dictionary! { "N" => Object::Reference(ap) },
            };
            Object::Dictionary(dict)
        }
        "image" => {
            let encoded = match anno.image.as_deref() {
                Some(s) if !s.is_empty() => s,
                _ => return Object::Null,
            };
            let bytes = match general_purpose::STANDARD.decode(encoded) {
                Ok(b) => b,
                Err(_) => return Object::Null,
            };
            let (img_w, img_h, image_id) = if bytes.starts_with(&[0xFF, 0xD8]) {
                let (w, h) = jpeg_size(&bytes).unwrap_or((0, 0));
                if w == 0 || h == 0 {
                    return Object::Null;
                }
                let img_dict = dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "Width" => w as i64,
                    "Height" => h as i64,
                    "ColorSpace" => "DeviceRGB",
                    "BitsPerComponent" => 8,
                    "Filter" => "DCTDecode",
                };
                (w, h, out.add_object(Stream::new(img_dict, bytes)))
            } else {
                // PNG / other formats: decode, split alpha into an SMask so
                // signatures keep a transparent background.
                let decoded = match image::load_from_memory(&bytes) {
                    Ok(i) => i.to_rgba8(),
                    Err(_) => return Object::Null,
                };
                let (w, h) = (decoded.width(), decoded.height());
                if w == 0 || h == 0 {
                    return Object::Null;
                }
                let raw = decoded.into_raw();
                let mut rgb = Vec::with_capacity((w * h * 3) as usize);
                let mut alpha = Vec::with_capacity((w * h) as usize);
                for px in raw.chunks_exact(4) {
                    rgb.extend_from_slice(&px[..3]);
                    alpha.push(px[3]);
                }
                let smask_dict = dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "Width" => w as i64,
                    "Height" => h as i64,
                    "ColorSpace" => "DeviceGray",
                    "BitsPerComponent" => 8,
                    "Filter" => "FlateDecode",
                };
                let smask_id = out.add_object(Stream::new(smask_dict, deflate(&alpha)));
                let img_dict = dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "Width" => w as i64,
                    "Height" => h as i64,
                    "ColorSpace" => "DeviceRGB",
                    "BitsPerComponent" => 8,
                    "Filter" => "FlateDecode",
                    "SMask" => Object::Reference(smask_id),
                };
                (w, h, out.add_object(Stream::new(img_dict, deflate(&rgb))))
            };
            let _ = (img_w, img_h);
            let mut xobjects = Dictionary::new();
            xobjects.set("Im0", Object::Reference(image_id));
            let mut resources = Dictionary::new();
            resources.set("XObject", Object::Dictionary(xobjects));
            let content = format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q", w = width, h = height);
            let ap = appearance(out, [0.0, 0.0, width, height], content, Some(resources));
            let dict = dictionary! {
                "Type" => "Annot",
                "Subtype" => "Stamp",
                "Name" => "Draft",
                "Rect" => rect_objects,
                "Contents" => pdf_string(anno.label.as_deref().unwrap_or("Image")),
                "F" => 4,
                "AP" => dictionary! { "N" => Object::Reference(ap) },
            };
            Object::Dictionary(dict)
        }
        _ => Object::Null,
    }
}

fn resolve_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

fn resolve_stream<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Stream> {
    match obj {
        Object::Stream(s) => Some(s),
        Object::Reference(id) => match doc.get_object(*id) {
            Ok(Object::Stream(s)) => Some(s),
            _ => None,
        },
        _ => None,
    }
}

fn add_resource_entry(
    doc: &Document,
    resources: &mut Dictionary,
    category: &[u8],
    name: &[u8],
    value: Object,
) {
    let mut cat = match resources.get(category) {
        Ok(Object::Dictionary(d)) => d.clone(),
        Ok(Object::Reference(id)) => doc
            .get_dictionary(*id)
            .map(|d| d.clone())
            .unwrap_or_default(),
        _ => Dictionary::new(),
    };
    cat.set(name.to_vec(), value);
    resources.set(category.to_vec(), Object::Dictionary(cat));
}

/// Turn annotation appearance streams into page content. Returns the content
/// streams to append plus the XObject entries that must be added to the page
/// resources.
fn flatten_annots(out: &mut Document, annots: &[Object]) -> (Vec<Object>, Dictionary) {
    let mut streams: Vec<String> = Vec::new();
    let mut xobjects = Dictionary::new();
    for (i, anno) in annots.iter().enumerate() {
        let dict = match anno {
            Object::Dictionary(d) => d,
            _ => continue,
        };
        let rect = match dict.get(b"Rect").ok().and_then(|r| numbers(r)) {
            Some(v) if v.len() == 4 => v,
            _ => continue,
        };
        let ap = match dict
            .get(b"AP")
            .ok()
            .and_then(|ap| ap.as_dict().ok())
            .and_then(|ap| ap.get(b"N").ok())
        {
            Some(Object::Reference(id)) => *id,
            _ => continue,
        };
        let name = format!("Anno{}", i + 1);
        xobjects.set(name.as_bytes().to_vec(), Object::Reference(ap));
        streams.push(format!(
            "q 1 0 0 1 {x} {y} cm /{name} Do Q",
            x = rect[0],
            y = rect[1],
            name = name
        ));
    }
    let contents = streams
        .into_iter()
        .map(|content| {
            Object::Reference(out.add_object(Stream::new(Dictionary::new(), content.into_bytes())))
        })
        .collect();
    (contents, xobjects)
}

/// Add a content stream (as an indirect object) and return its reference.
fn add_content_stream(out: &mut Document, content: String) -> Object {
    Object::Reference(out.add_object(Stream::new(Dictionary::new(), content.into_bytes())))
}

fn watermark_stream(wm: &Watermark, center_x: f64, center_y: f64) -> Option<String> {
    let text = wm.text.trim();
    if text.is_empty() {
        return None;
    }
    let text = text.replace(['\r', '\n'], " ");
    let size = wm.size.clamp(8.0, 400.0);
    let angle = wm.angle.to_radians();
    let color = if wm.color.len() >= 3 {
        (
            wm.color[0].clamp(0.0, 1.0),
            wm.color[1].clamp(0.0, 1.0),
            wm.color[2].clamp(0.0, 1.0),
        )
    } else {
        (0.45, 0.45, 0.45)
    };
    let approx = text.chars().count() as f64 * size * 0.52;
    let cx = center_x - (approx / 2.0) * angle.cos() + (size * 0.35) * angle.sin();
    let cy = center_y - (approx / 2.0) * angle.sin() - (size * 0.35) * angle.cos();
    let content = format!(
        "q /GSwm gs BT /Helv {size} Tf {r} {g} {b} rg {c} {s} {ns} {c} {x} {y} Tm ({t}) Tj ET Q",
        size = size,
        r = color.0,
        g = color.1,
        b = color.2,
        c = angle.cos(),
        s = angle.sin(),
        ns = -angle.sin(),
        x = cx,
        y = cy,
        t = escape_pdf_text(&text),
    );
    Some(content)
}

fn page_number_stream(label: &str, center_x: f64, baseline_y: f64, size: f64) -> String {
    let approx = label.chars().count() as f64 * size * 0.5;
    let x = (center_x - approx / 2.0).max(2.0);
    format!(
        "q BT /Helv {size} Tf 0.35 0.35 0.35 rg 1 0 0 1 {x} {y} Tm ({t}) Tj ET Q",
        size = size,
        x = x,
        y = baseline_y,
        t = escape_pdf_text(label)
    )
}

/// Compose a new PDF from the given documents, page order, rotations and
/// annotations.
pub fn build(payload: Payload) -> Result<Vec<u8>, String> {
    let mut sources: HashMap<String, Document> = HashMap::new();
    for doc in &payload.docs {
        let bytes = base64_decode(&doc.data)?;
        let parsed = Document::load_mem(&bytes).map_err(|e| format!("load pdf: {e}"))?;
        sources.insert(doc.id.clone(), parsed);
    }

    let mut out = Document::with_version("1.5");
    let pages_id = out.new_object_id();
    let font_id = out.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let times_id = out.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Times-Roman",
        "Encoding" => "WinAnsiEncoding",
    });
    let courier_id = out.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
        "Encoding" => "WinAnsiEncoding",
    });
    let annotation_fonts = AnnotationFonts {
        helvetica: font_id,
        times: times_id,
        courier: courier_id,
    };

    let total_pages = payload.pages.len();
    let mut kids: Vec<Object> = Vec::new();
    let mut page_ids: Vec<ObjectId> = Vec::new();
    let mut page_sizes: Vec<(f64, f64)> = Vec::new();

    for (page_no, spec) in payload.pages.iter().enumerate() {
        let src = sources
            .get(&spec.doc)
            .ok_or_else(|| format!("unknown document {}", spec.doc))?;
        let pages = src.get_pages();
        let page_id = *pages
            .get(&(spec.index + 1))
            .ok_or_else(|| format!("page {} missing in {}", spec.index, spec.doc))?;
        let page_dict = src
            .get_dictionary(page_id)
            .map_err(|e| format!("read page: {e}"))?
            .clone();

        let mb = media_box(inherited(src, &page_dict, b"MediaBox"));
        let (llx, lly, urx, ury) = (mb[0], mb[1], mb[2], mb[3]);
        let w = (urx - llx).abs().max(1.0);
        let h = (ury - lly).abs().max(1.0);

        let angle = ((spec.rotation % 360) + 360) % 360;
        let (new_w, new_h) = if angle == 90 || angle == 270 {
            (h, w)
        } else {
            (w, h)
        };

        let mut map: IdMap = HashMap::new();

        // Resources (inherited if needed).
        let resources = match inherited(src, &page_dict, b"Resources") {
            Some(obj) => deep_copy(src, &mut out, &obj, &mut map),
            None => Object::Dictionary(Dictionary::new()),
        };

        // Contents, wrapped so the rotation can be baked in.
        let contents = match page_dict.get(b"Contents") {
            Ok(obj) => match deep_copy(src, &mut out, obj, &mut map) {
                Object::Stream(stream) => Object::Reference(out.add_object(stream)),
                other => other,
            },
            Err(_) => add_content_stream(&mut out, String::new()),
        };

        let (linear, offset) = match angle {
            90 => ([0.0, -1.0, 1.0, 0.0], (0.0 - lly, w + llx)),
            180 => ([-1.0, 0.0, 0.0, -1.0], (w + llx, h + lly)),
            270 => ([0.0, 1.0, -1.0, 0.0], (h + lly, 0.0 - llx)),
            _ => ([1.0, 0.0, 0.0, 1.0], (0.0 - llx, 0.0 - lly)),
        };

        let contents_obj = if angle == 0 {
            contents
        } else {
            let prefix = format!(
                "q {a} {b} {c} {d} {e} {f} cm\n",
                a = linear[0],
                b = linear[1],
                c = linear[2],
                d = linear[3],
                e = offset.0,
                f = offset.1
            );
            let prefix_id = out.add_object(Stream::new(Dictionary::new(), prefix.into_bytes()));
            let suffix_id = out.add_object(Stream::new(Dictionary::new(), b"Q".to_vec()));
            Object::Array(vec![
                Object::Reference(prefix_id),
                contents,
                Object::Reference(suffix_id),
            ])
        };

        let mut content_parts: Vec<Object> = match contents_obj {
            Object::Array(items) => items,
            other => vec![other],
        };

        // Annotations that belong to this output page.
        let mut annots: Vec<Object> = Vec::new();
        for anno in payload.annotations.iter().filter(|a| a.page == page_no) {
            let obj = annotation_object(&mut out, anno, new_w, new_h, annotation_fonts);
            if !matches!(obj, Object::Null) {
                annots.push(obj);
            }
        }

        let mut resources_dict = match &resources {
            Object::Dictionary(d) => d.clone(),
            Object::Reference(id) => out
                .get_dictionary(*id)
                .map(|d| d.clone())
                .unwrap_or_default(),
            _ => Dictionary::new(),
        };

        if payload.flatten && !annots.is_empty() {
            let (streams, xobjects) = flatten_annots(&mut out, &annots);
            for stream in streams {
                content_parts.push(stream);
            }
            if !xobjects.is_empty() {
                for (name, value) in xobjects.iter() {
                    add_resource_entry(&out, &mut resources_dict, b"XObject", name, value.clone());
                }
            }
            annots.clear();
        }

        // Crop rectangle in unrotated page coordinates. Page furniture must be
        // positioned inside it, otherwise a crop can clip page numbers away.
        let crop_rect = spec.crop.and_then(|crop| {
            let cx0 = crop[0].clamp(0.0, 1.0) * new_w;
            let cx1 = crop[2].clamp(0.0, 1.0) * new_w;
            let cy0 = (1.0 - crop[3].clamp(0.0, 1.0)) * new_h;
            let cy1 = (1.0 - crop[1].clamp(0.0, 1.0)) * new_h;
            if cx1 - cx0 > 2.0 && cy1 - cy0 > 2.0 {
                Some((cx0.min(cx1), cy0.min(cy1), cx0.max(cx1), cy0.max(cy1)))
            } else {
                None
            }
        });
        let (vis_x0, vis_y0, vis_x1, vis_y1) = crop_rect.unwrap_or((0.0, 0.0, new_w, new_h));

        if let Some(wm) = &payload.watermark {
            if let Some(content) =
                watermark_stream(wm, (vis_x0 + vis_x1) / 2.0, (vis_y0 + vis_y1) / 2.0)
            {
                let opacity = wm.opacity.clamp(0.02, 1.0);
                let gs = dictionary! {                    "Type" => "ExtGState",
                    "ca" => Object::Real(opacity as f32),
                    "CA" => Object::Real(opacity as f32),
                };
                add_resource_entry(
                    &out,
                    &mut resources_dict,
                    b"ExtGState",
                    b"GSwm",
                    Object::Dictionary(gs),
                );
                add_resource_entry(
                    &out,
                    &mut resources_dict,
                    b"Font",
                    b"Helv",
                    Object::Reference(font_id),
                );
                content_parts.push(add_content_stream(&mut out, content));
            }
        }

        if payload.page_numbers {
            add_resource_entry(
                &out,
                &mut resources_dict,
                b"Font",
                b"Helv",
                Object::Reference(font_id),
            );
            if vis_y1 - vis_y0 > 24.0 {
                let label = format!("{} / {}", page_no + 1, total_pages);
                content_parts.push(add_content_stream(
                    &mut out,
                    page_number_stream(&label, (vis_x0 + vis_x1) / 2.0, vis_y0 + 9.0, 9.0),
                ));
            }
        }

        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"Page".to_vec()));
        dict.set("Parent", Object::Reference(pages_id));
        dict.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(new_w as f32),
                Object::Real(new_h as f32),
            ]),
        );

        if let Some((cx0, cy0, cx1, cy1)) = crop_rect {
            dict.set(
                "CropBox",
                Object::Array(vec![
                    Object::Real(cx0 as f32),
                    Object::Real(cy0 as f32),
                    Object::Real(cx1 as f32),
                    Object::Real(cy1 as f32),
                ]),
            );
        }

        dict.set("Resources", Object::Dictionary(resources_dict));
        dict.set("Contents", Object::Array(content_parts));
        dict.set("Rotate", Object::Integer(0));
        if !annots.is_empty() {
            dict.set("Annots", Object::Array(annots));
        }

        let new_page_id = out.add_object(Object::Dictionary(dict));
        kids.push(Object::Reference(new_page_id));
        page_ids.push(new_page_id);
        page_sizes.push((new_w, new_h));
    }

    let count = kids.len() as i64;
    let pages_dict = dictionary! {
        "Type" => "Pages",
        "Kids" => Object::Array(kids),
        "Count" => count,
    };
    out.objects.insert(pages_id, Object::Dictionary(pages_dict));

    let catalog_id = out.new_object_id();
    out.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        }),
    );

    add_form_fields(
        &mut out,
        &payload.forms,
        &page_ids,
        &page_sizes,
        annotation_fonts,
        catalog_id,
    );

    let bookmarks: Vec<&BookmarkInput> = payload
        .bookmarks
        .iter()
        .filter(|bookmark| bookmark.page < page_ids.len() && !bookmark.title.trim().is_empty())
        .collect();
    if !bookmarks.is_empty() {
        let outlines_id = out.new_object_id();
        let item_ids: Vec<ObjectId> = bookmarks.iter().map(|_| out.new_object_id()).collect();
        for (index, bookmark) in bookmarks.iter().enumerate() {
            let mut item = Dictionary::new();
            let title = bookmark.title.trim();
            item.set("Title", pdf_string(title));
            item.set("Parent", Object::Reference(outlines_id));
            item.set(
                "Dest",
                Object::Array(vec![
                    Object::Reference(page_ids[bookmark.page]),
                    Object::Name(b"Fit".to_vec()),
                ]),
            );
            if index > 0 {
                item.set("Prev", Object::Reference(item_ids[index - 1]));
            }
            if index + 1 < item_ids.len() {
                item.set("Next", Object::Reference(item_ids[index + 1]));
            }
            out.objects
                .insert(item_ids[index], Object::Dictionary(item));
        }
        out.objects.insert(
            outlines_id,
            Object::Dictionary(dictionary! {
                "Type" => "Outlines",
                "First" => Object::Reference(item_ids[0]),
                "Last" => Object::Reference(item_ids[item_ids.len() - 1]),
                "Count" => Object::Integer(item_ids.len() as i64),
            }),
        );
        if let Some(Object::Dictionary(catalog)) = out.objects.get_mut(&catalog_id) {
            catalog.set("Outlines", Object::Reference(outlines_id));
        }
    }
    out.trailer.set("Root", Object::Reference(catalog_id));

    if !payload.remove_metadata {
        if let Some(info) = &payload.info {
            let mut info_dict = Dictionary::new();
            let fields = [
                ("Title", &info.title),
                ("Author", &info.author),
                ("Subject", &info.subject),
                ("Keywords", &info.keywords),
                ("Creator", &info.creator),
            ];
            for (key, value) in fields {
                if let Some(text) = value {
                    if !text.is_empty() {
                        info_dict.set(key, pdf_string(text));
                    }
                }
            }
            if !info_dict.is_empty() {
                let info_id = out.add_object(info_dict);
                out.trailer.set("Info", Object::Reference(info_id));
            }
        }
    }

    if let Some(security) = &payload.security {
        crate::security::encrypt_document(&mut out, security)?;
    }

    let mut buffer = Vec::new();
    out.save_to(&mut buffer)
        .map_err(|e| format!("write pdf: {e}"))?;

    if let Some(security) = &payload.security {
        buffer = crate::security::append_seal(buffer, &security.user_password)?;
    }
    Ok(buffer)
}

fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
    general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("decode pdf: {e}"))
}

fn deflate(data: &[u8]) -> Vec<u8> {
    use flate2::{Compression, write::ZlibEncoder};
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(out)
}

/// Create a single page blank PDF (US Letter by default).
pub fn blank(width: f64, height: f64) -> Result<Vec<u8>, String> {
    let width = width.clamp(36.0, 20000.0);
    let height = height.clamp(36.0, 20000.0);
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let contents_id = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
    let page = dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(width as f32),
            Object::Real(height as f32),
        ],
        "Resources" => Object::Dictionary(Dictionary::new()),
        "Contents" => Object::Reference(contents_id),
    };
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
    let mut buffer = Vec::new();
    doc.save_to(&mut buffer)
        .map_err(|e| format!("write pdf: {e}"))?;
    Ok(buffer)
}

/// Build a PDF where every page is one image, scaled to fit A4.
pub fn images_to_pdf(paths: &[String]) -> Result<Vec<u8>, String> {
    if paths.is_empty() {
        return Err("no images given".to_string());
    }
    let mut out = Document::with_version("1.5");
    let pages_id = out.new_object_id();
    let mut kids = Vec::new();

    for path in paths {
        let img = image::open(path)
            .map_err(|e| format!("open {path}: {e}"))?
            .to_rgb8();
        let (px_w, px_h) = img.dimensions();
        if px_w == 0 || px_h == 0 {
            continue;
        }
        let scale = (595.0 / px_w as f64).min(842.0 / px_h as f64).min(1.0);
        let page_w = (px_w as f64 * scale).max(36.0);
        let page_h = (px_h as f64 * scale).max(36.0);

        let raw = img.into_raw();
        let compressed = deflate(&raw);
        let img_dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => px_w as i64,
            "Height" => px_h as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
        };
        let img_id = out.add_object(Stream::new(img_dict, compressed));
        let content = format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q", w = page_w, h = page_h);
        let content_id = out.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let mut xobjects = Dictionary::new();
        xobjects.set("Im0", Object::Reference(img_id));
        let resources = dictionary! {
            "XObject" => Object::Dictionary(xobjects),
        };
        let page = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(page_w as f32),
                Object::Real(page_h as f32),
            ],
            "Resources" => Object::Dictionary(resources),
            "Contents" => Object::Reference(content_id),
        };
        kids.push(Object::Reference(out.add_object(page)));
    }

    let count = kids.len() as i64;
    out.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(kids),
            "Count" => count,
        }),
    );
    let catalog_id = out.new_object_id();
    out.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        }),
    );
    out.trailer.set("Root", Object::Reference(catalog_id));
    let mut buffer = Vec::new();
    out.save_to(&mut buffer)
        .map_err(|e| format!("write pdf: {e}"))?;
    Ok(buffer)
}

#[derive(Serialize)]
pub struct ExtractedImage {
    pub page: u32,
    pub name: String,
    pub ext: String,
    pub data: String,
}

/// Extract embedded raster images (JPEG passthrough, everything else as PNG).
pub fn extract_images(bytes: &[u8]) -> Result<Vec<ExtractedImage>, String> {
    let doc = Document::load_mem(bytes).map_err(|e| format!("load pdf: {e}"))?;
    let pages = doc.get_pages();
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (page_no, page_id) in pages {
        if items.len() >= 200 {
            break;
        }
        let page = match doc.get_dictionary(page_id) {
            Ok(p) => p.clone(),
            Err(_) => continue,
        };
        let resources = match inherited(&doc, &page, b"Resources") {
            Some(obj) => obj,
            None => continue,
        };
        let res_dict = match resolve_dict(&doc, &resources) {
            Some(d) => d,
            None => continue,
        };
        let xobj = match res_dict.get(b"XObject") {
            Ok(obj) => obj.clone(),
            Err(_) => continue,
        };
        let xobj_dict = match resolve_dict(&doc, &xobj) {
            Some(d) => d.clone(),
            None => continue,
        };
        for (name, value) in xobj_dict.iter() {
            if items.len() >= 200 {
                break;
            }
            let stream = match resolve_stream(&doc, value) {
                Some(s) => s,
                None => continue,
            };
            if stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|s| s.as_name().ok())
                != Some(b"Image".as_ref())
            {
                continue;
            }
            let filter = match stream.dict.get(b"Filter") {
                Ok(Object::Name(n)) => Some(n.clone()),
                Ok(Object::Array(arr)) => arr
                    .iter()
                    .find_map(|o| o.as_name().ok().map(|n| n.to_vec())),
                _ => None,
            };
            let width = stream
                .dict
                .get(b"Width")
                .ok()
                .and_then(|w| w.as_i64().ok())
                .unwrap_or(0);
            let height = stream
                .dict
                .get(b"Height")
                .ok()
                .and_then(|h| h.as_i64().ok())
                .unwrap_or(0);
            let key = format!(
                "{page_no}-{}-{}-{}",
                String::from_utf8_lossy(name),
                width,
                height
            );
            if width <= 0 || height <= 0 || !seen.insert(key) {
                continue;
            }

            if filter.as_deref() == Some(b"DCTDecode".as_slice()) {
                items.push(ExtractedImage {
                    page: page_no,
                    name: String::from_utf8_lossy(name).to_string(),
                    ext: "jpg".to_string(),
                    data: general_purpose::STANDARD.encode(&stream.content),
                });
                continue;
            }

            let raw = if filter.as_deref() == Some(b"FlateDecode".as_slice()) {
                match inflate(&stream.content) {
                    Some(v) => v,
                    None => continue,
                }
            } else if filter.is_none() {
                stream.content.clone()
            } else {
                continue;
            };
            let color_space = stream
                .dict
                .get(b"ColorSpace")
                .ok()
                .and_then(|c| c.as_name().ok())
                .map(|n| n.to_vec())
                .unwrap_or_else(|| b"DeviceRGB".to_vec());
            let bpc = stream
                .dict
                .get(b"BitsPerComponent")
                .ok()
                .and_then(|b| b.as_i64().ok())
                .unwrap_or(8);
            if bpc != 8 {
                continue;
            }
            let expected = width as usize * height as usize;
            let dyn_img = if color_space.as_slice() == b"DeviceRGB" && raw.len() >= expected * 3 {
                image::RgbImage::from_raw(width as u32, height as u32, raw[..expected * 3].to_vec())
                    .map(image::DynamicImage::ImageRgb8)
            } else if color_space.as_slice() == b"DeviceGray" && raw.len() >= expected {
                image::GrayImage::from_raw(width as u32, height as u32, raw[..expected].to_vec())
                    .map(image::DynamicImage::ImageLuma8)
            } else {
                None
            };
            let dyn_img = match dyn_img {
                Some(i) => i,
                None => continue,
            };
            let mut png = Vec::new();
            if dyn_img
                .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
                .is_err()
            {
                continue;
            }
            items.push(ExtractedImage {
                page: page_no,
                name: String::from_utf8_lossy(name).to_string(),
                ext: "png".to_string(),
                data: general_purpose::STANDARD.encode(png),
            });
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });

        let mut kids = Vec::new();
        for (i, label) in ["First page", "Second page"].iter().enumerate() {
            let content = format!("BT /F1 28 Tf 72 {} Td ({}) Tj ET", 700 - i * 20, label);
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
            kids.push(Object::Reference(doc.add_object(page)));
        }

        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => Object::Array(kids),
                "Count" => 2,
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

        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).unwrap();
        buffer
    }

    fn payload_for(annotations: Vec<Anno>) -> Payload {
        Payload {
            docs: vec![DocInput {
                id: "src".into(),
                data: general_purpose::STANDARD.encode(source_pdf()),
            }],
            pages: vec![
                PageSpec {
                    doc: "src".into(),
                    index: 1,
                    rotation: 0,
                    crop: None,
                },
                PageSpec {
                    doc: "src".into(),
                    index: 0,
                    rotation: 90,
                    crop: None,
                },
            ],
            annotations,
            forms: vec![],
            flatten: false,
            remove_metadata: false,
            info: None,
            watermark: None,
            page_numbers: false,
            bookmarks: vec![],
            security: None,
        }
    }

    fn plain_anno(kind: &str) -> Anno {
        Anno {
            page: 0,
            kind: kind.into(),
            color: vec![1.0, 0.8, 0.0],
            opacity: 0.35,
            width: 2.0,
            rect: Some([0.1, 0.1, 0.4, 0.06]),
            rects: None,
            points: None,
            text: None,
            size: 12.0,
            font: "helvetica".into(),
            image: None,
            label: None,
        }
    }

    fn form_field(kind: &str, name: &str, page: usize, rect: [f64; 4]) -> FormField {
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
            font_size: 12.0,
            font: "helvetica".into(),
            color: vec![0.1, 0.1, 0.1],
            background: Vec::new(),
            border_color: Vec::new(),
            border_width: 0.0,
            tooltip: None,
            options: Vec::new(),
            combo: false,
            multi_select: false,
            checked: false,
            export_value: None,
            radio_group: None,
        }
    }

    fn page_contents(doc: &Document, page_id: ObjectId) -> String {
        let page = doc.get_dictionary(page_id).unwrap();
        let contents = page.get(b"Contents").unwrap();
        let objs: Vec<Object> = match contents {
            Object::Array(items) => items.clone(),
            other => vec![other.clone()],
        };
        let mut out = String::new();
        for obj in objs {
            if let Object::Reference(id) = obj {
                if let Ok(Object::Stream(stream)) = doc.get_object(id) {
                    out.push_str(&String::from_utf8_lossy(&stream.content));
                }
            }
        }
        out
    }

    #[test]
    fn reorders_and_rotates_pages() {
        let out = build(payload_for(vec![])).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        assert_eq!(pages.len(), 2, "two output pages");

        let first = doc.get_dictionary(pages[&1]).unwrap();
        let mb = numbers(first.get(b"MediaBox").unwrap()).unwrap();
        assert_eq!(mb, vec![0.0, 0.0, 612.0, 792.0]);

        let second = doc.get_dictionary(pages[&2]).unwrap();
        let mb = numbers(second.get(b"MediaBox").unwrap()).unwrap();
        assert_eq!(
            mb,
            vec![0.0, 0.0, 792.0, 612.0],
            "rotation swaps dimensions"
        );
        assert_eq!(second.get(b"Rotate").unwrap(), &Object::Integer(0));
    }

    #[test]
    fn writes_annotations_with_appearance_streams() {
        let annotations = vec![
            plain_anno("highlight"),
            Anno {
                kind: "ink".into(),
                color: vec![0.1, 0.2, 0.9],
                opacity: 1.0,
                width: 3.0,
                rect: None,
                points: Some(vec![[0.2, 0.3], [0.3, 0.35], [0.4, 0.3]]),
                ..plain_anno("ink")
            },
            Anno {
                kind: "square".into(),
                color: vec![0.9, 0.1, 0.1],
                rect: Some([0.5, 0.5, 0.2, 0.1]),
                ..plain_anno("square")
            },
            Anno {
                page: 1,
                kind: "text".into(),
                color: vec![0.0, 0.0, 1.0],
                rect: Some([0.15, 0.55, 0.35, 0.1]),
                text: Some("Reviewed".into()),
                size: 14.0,
                ..plain_anno("text")
            },
        ];

        let out = build(payload_for(annotations)).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();

        let first = doc.get_dictionary(pages[&1]).unwrap();
        let annots = first.get(b"Annots").unwrap().as_array().unwrap();
        assert_eq!(annots.len(), 3, "three annotations on first page");
        for annot in annots {
            let dict = match annot {
                Object::Dictionary(d) => d,
                Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
                _ => panic!("unexpected annot object"),
            };
            assert!(dict.get(b"AP").is_ok(), "annotation has appearance stream");
            assert!(dict.get(b"Rect").is_ok());
        }

        // The ink annotation rect must track the stroke, not a default box.
        let ink = annots
            .iter()
            .find_map(|annot| {
                let dict = match annot {
                    Object::Dictionary(d) => d.clone(),
                    Object::Reference(id) => doc.get_dictionary(*id).unwrap().clone(),
                    _ => return None,
                };
                if dict.get(b"Subtype").unwrap().as_name().unwrap() == b"Ink" {
                    Some(dict)
                } else {
                    None
                }
            })
            .expect("ink annotation present");
        let rect = numbers(ink.get(b"Rect").unwrap()).unwrap();
        assert!(rect[0] > 100.0 && rect[0] < 130.0, "ink left edge {rect:?}");
        assert!(
            rect[1] > 495.0 && rect[1] < 520.0,
            "ink bottom edge {rect:?}"
        );
        assert!(
            rect[2] > 240.0 && rect[2] < 260.0,
            "ink right edge {rect:?}"
        );
        assert!(rect[3] > 550.0 && rect[3] < 570.0, "ink top edge {rect:?}");
        let ink_list = ink.get(b"InkList").unwrap().as_array().unwrap();
        let stroke = ink_list[0].as_array().unwrap();
        assert_eq!(stroke.len(), 6, "three points, two coordinates each");
        let first_x = stroke[0].as_float().unwrap();
        let first_y = stroke[1].as_float().unwrap();
        assert!((first_x - 122.4).abs() < 0.5, "first ink point x {first_x}");
        assert!((first_y - 554.4).abs() < 0.5, "first ink point y {first_y}");

        let second = doc.get_dictionary(pages[&2]).unwrap();
        let annots = second.get(b"Annots").unwrap().as_array().unwrap();
        assert_eq!(annots.len(), 1);
        let dict = match &annots[0] {
            Object::Dictionary(d) => d.clone(),
            Object::Reference(id) => doc.get_dictionary(*id).unwrap().clone(),
            _ => panic!("unexpected annot object"),
        };
        assert_eq!(
            dict.get(b"Subtype").unwrap().as_name().unwrap(),
            b"FreeText"
        );
        assert!(dict.get(b"Contents").is_ok());
    }

    #[test]
    fn writes_text_annotation_font_families() {
        let annotations = vec![
            Anno {
                kind: "text".into(),
                text: Some("Sans".into()),
                font: "helvetica".into(),
                rect: Some([0.1, 0.1, 0.2, 0.05]),
                ..plain_anno("text")
            },
            Anno {
                kind: "text".into(),
                text: Some("Serif".into()),
                font: "times".into(),
                rect: Some([0.1, 0.2, 0.2, 0.05]),
                ..plain_anno("text")
            },
            Anno {
                kind: "text".into(),
                text: Some("Mono".into()),
                font: "courier".into(),
                rect: Some([0.1, 0.3, 0.2, 0.05]),
                ..plain_anno("text")
            },
        ];
        let out = build(payload_for(annotations)).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        let page = doc.get_dictionary(pages[&1]).unwrap();
        let annots = page.get(b"Annots").unwrap().as_array().unwrap();
        let expected = ["/Helv ", "/TiRo ", "/Cour "];
        for (annot, key) in annots.iter().zip(expected) {
            let dict = match annot {
                Object::Dictionary(d) => d,
                Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
                _ => panic!("unexpected annot object"),
            };
            let da = std::str::from_utf8(dict.get(b"DA").unwrap().as_str().unwrap()).unwrap();
            assert!(da.contains(key), "expected {key} in DA {da}");
        }
    }

    #[test]
    fn writes_markup_and_note_annotations() {
        let annotations = vec![
            plain_anno("underline"),
            plain_anno("strikeout"),
            plain_anno("squiggly"),
            Anno {
                kind: "note".into(),
                text: Some("Please check this".into()),
                rect: Some([0.5, 0.5, 0.03, 0.03]),
                ..plain_anno("note")
            },
            Anno {
                kind: "stamp".into(),
                label: Some("APPROVED".into()),
                rect: Some([0.6, 0.7, 0.25, 0.08]),
                ..plain_anno("stamp")
            },
        ];
        let out = build(payload_for(annotations)).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        let page = doc.get_dictionary(pages[&1]).unwrap();
        let annots = page.get(b"Annots").unwrap().as_array().unwrap();
        assert_eq!(annots.len(), 5);
        let mut subtypes = Vec::new();
        for annot in annots {
            let dict = match annot {
                Object::Dictionary(d) => d,
                Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
                _ => continue,
            };
            subtypes.push(
                String::from_utf8_lossy(dict.get(b"Subtype").unwrap().as_name().unwrap())
                    .to_string(),
            );
        }
        for expected in ["Underline", "StrikeOut", "Squiggly", "Text", "Stamp"] {
            assert!(
                subtypes.contains(&expected.to_string()),
                "missing {expected}"
            );
        }
    }

    #[test]
    fn writes_interactive_acroform_fields() {
        let mut text = form_field("text", "full_name", 0, [0.1, 0.1, 0.3, 0.05]);
        text.value = "Ada Lovelace".into();
        text.background = vec![0.95, 0.95, 0.95];
        text.border_color = vec![0.2, 0.3, 0.8];
        text.border_width = 1.0;
        text.max_len = Some(40);
        text.font = "times".into();

        let mut check = form_field("checkbox", "agree", 0, [0.1, 0.2, 0.03, 0.03]);
        check.checked = true;
        check.export_value = Some("Yes".into());

        let mut radio_a = form_field("radio", "color", 0, [0.5, 0.1, 0.03, 0.03]);
        radio_a.radio_group = Some("color".into());
        radio_a.checked = true;
        radio_a.export_value = Some("Red".into());

        let mut radio_b = form_field("radio", "color", 1, [0.5, 0.15, 0.03, 0.03]);
        radio_b.radio_group = Some("color".into());
        radio_b.export_value = Some("Blue".into());

        let mut choice = form_field("choice", "country", 1, [0.1, 0.3, 0.35, 0.05]);
        choice.combo = true;
        choice.value = "ca".into();
        choice.values = vec!["ca".into()];
        choice.default_value = "ca".into();
        choice.default_values = vec!["ca".into()];
        choice.options = vec![
            FormOption {
                display: "Canada".into(),
                export: "ca".into(),
            },
            FormOption {
                display: "United States".into(),
                export: "us".into(),
            },
        ];

        let mut signature = form_field("signature", "sign_here", 0, [0.6, 0.8, 0.3, 0.06]);
        signature.value = "Ada".into();

        let mut payload = payload_for(vec![]);
        payload.forms = vec![text, check, radio_a, radio_b, choice, signature];
        let out = build(payload).expect("build form pdf");
        let doc = Document::load_mem(&out).expect("reparse");

        let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
        let catalog = doc.get_dictionary(root).unwrap();
        let acro = resolve_dict(&doc, catalog.get(b"AcroForm").unwrap()).expect("AcroForm");
        assert_eq!(
            acro.get(b"NeedAppearances").unwrap(),
            &Object::Boolean(false)
        );
        assert!(acro.get(b"DA").is_ok());
        let fields = acro.get(b"Fields").unwrap().as_array().unwrap();
        assert_eq!(
            fields.len(),
            5,
            "radio group, text, checkbox, choice, signature"
        );

        let mut by_name = HashMap::new();
        for field in fields {
            let dict = match field {
                Object::Reference(id) => doc.get_dictionary(*id).unwrap().clone(),
                Object::Dictionary(dict) => dict.clone(),
                _ => panic!("unexpected form field object"),
            };
            let name =
                String::from_utf8_lossy(dict.get(b"T").unwrap().as_str().unwrap()).to_string();
            by_name.insert(name, dict);
        }

        let text = by_name.get("full_name").expect("text field");
        assert_eq!(text.get(b"FT").unwrap().as_name().unwrap(), b"Tx");
        assert_eq!(text.get(b"V").unwrap().as_str().unwrap(), b"Ada Lovelace");
        assert_eq!(text.get(b"MaxLen").unwrap().as_i64().unwrap(), 40);
        assert!(text.get(b"AP").is_ok(), "text appearance stream");
        let da = std::str::from_utf8(text.get(b"DA").unwrap().as_str().unwrap()).unwrap();
        assert!(da.contains("/TiRo "), "serif DA: {da}");

        let check = by_name.get("agree").expect("checkbox field");
        assert_eq!(check.get(b"FT").unwrap().as_name().unwrap(), b"Btn");
        assert_eq!(check.get(b"V").unwrap().as_name().unwrap(), b"Yes");
        assert_eq!(check.get(b"AS").unwrap().as_name().unwrap(), b"Yes");
        assert!(check.get(b"AP").is_ok(), "checkbox appearance stream");

        let radio = by_name.get("color").expect("radio group");
        assert_eq!(radio.get(b"FT").unwrap().as_name().unwrap(), b"Btn");
        assert_eq!(radio.get(b"V").unwrap().as_name().unwrap(), b"Red");
        assert_eq!(radio.get(b"Kids").unwrap().as_array().unwrap().len(), 2);

        let choice = by_name.get("country").expect("choice field");
        assert_eq!(choice.get(b"FT").unwrap().as_name().unwrap(), b"Ch");
        assert_eq!(choice.get(b"V").unwrap().as_str().unwrap(), b"ca");
        assert_eq!(choice.get(b"Opt").unwrap().as_array().unwrap().len(), 2);
        assert!(choice.get(b"AP").is_ok(), "choice appearance stream");

        let signature = by_name.get("sign_here").expect("signature field");
        assert_eq!(signature.get(b"FT").unwrap().as_name().unwrap(), b"Tx");
        assert_eq!(signature.get(b"V").unwrap().as_str().unwrap(), b"Ada");

        let pages = doc.get_pages();
        let first = doc.get_dictionary(pages[&1]).unwrap();
        let first_annots = first.get(b"Annots").unwrap().as_array().unwrap();
        assert_eq!(first_annots.len(), 4, "four widgets on first page");
        for annot in first_annots {
            let dict = match annot {
                Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
                Object::Dictionary(dict) => dict,
                _ => panic!("unexpected widget object"),
            };
            assert_eq!(dict.get(b"Subtype").unwrap().as_name().unwrap(), b"Widget");
            assert!(dict.get(b"P").is_ok());
        }
        let second = doc.get_dictionary(pages[&2]).unwrap();
        assert_eq!(
            second.get(b"Annots").unwrap().as_array().unwrap().len(),
            2,
            "radio widget and choice field on second page"
        );
    }

    #[test]
    fn writes_multi_line_highlight_comment() {
        let annotations = vec![Anno {
            kind: "highlight".into(),
            rect: Some([0.1, 0.1, 0.4, 0.1]),
            rects: Some(vec![[0.1, 0.1, 0.4, 0.02], [0.1, 0.16, 0.28, 0.02]]),
            text: Some("Important passage".into()),
            ..plain_anno("highlight")
        }];
        let out = build(payload_for(annotations)).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        let page = doc.get_dictionary(pages[&1]).unwrap();
        let annots = page.get(b"Annots").unwrap().as_array().unwrap();
        assert_eq!(annots.len(), 1);
        let dict = match &annots[0] {
            Object::Dictionary(d) => d,
            Object::Reference(id) => doc.get_dictionary(*id).unwrap(),
            _ => panic!("unexpected annot object"),
        };
        assert_eq!(
            dict.get(b"Subtype").unwrap().as_name().unwrap(),
            b"Highlight"
        );
        assert_eq!(
            dict.get(b"Contents").unwrap().as_str().unwrap(),
            "Important passage".as_bytes()
        );
        let quads = dict.get(b"QuadPoints").unwrap().as_array().unwrap();
        assert_eq!(quads.len(), 16, "two line rects become two quads");
        let rect = numbers(dict.get(b"Rect").unwrap()).unwrap();
        assert!(rect[0] < 0.1 * 612.0 + 0.5);
        assert!(rect[2] > 0.49 * 612.0 - 0.5);
        assert!(rect[3] > 0.28 * 792.0 - 0.5);
    }

    #[test]
    fn crops_pages() {
        let mut payload = payload_for(vec![]);
        payload.pages[0].crop = Some([0.1, 0.1, 0.9, 0.9]);
        let out = build(payload).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        let page = doc.get_dictionary(pages[&1]).unwrap();
        let crop = numbers(page.get(b"CropBox").unwrap()).unwrap();
        assert!((crop[0] - 61.2).abs() < 0.5, "crop left {crop:?}");
        assert!((crop[1] - 79.2).abs() < 0.5, "crop bottom {crop:?}");
        assert!((crop[2] - 550.8).abs() < 0.5, "crop right {crop:?}");
        assert!((crop[3] - 712.8).abs() < 0.5, "crop top {crop:?}");
    }

    #[test]
    fn flattens_annotations_into_content() {
        let mut payload = payload_for(vec![plain_anno("highlight")]);
        payload.flatten = true;
        let out = build(payload).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        let page = doc.get_dictionary(pages[&1]).unwrap();
        assert!(
            page.get(b"Annots").is_err(),
            "flattened page has no live annotations"
        );
        let resources = resolve_dict(&doc, page.get(b"Resources").unwrap()).unwrap();
        let xobjects = resolve_dict(&doc, resources.get(b"XObject").unwrap()).unwrap();
        assert!(
            xobjects.get(b"Anno1").is_ok(),
            "appearance became an XObject"
        );
        let content = page_contents(&doc, pages[&1]);
        assert!(content.contains("/Anno1 Do"), "content invokes appearance");
    }

    #[test]
    fn adds_watermark_page_numbers_and_metadata() {
        let mut payload = payload_for(vec![]);
        payload.watermark = Some(Watermark {
            text: "CONFIDENTIAL".into(),
            size: 40.0,
            opacity: 0.2,
            angle: 45.0,
            color: vec![1.0, 0.0, 0.0],
        });
        payload.page_numbers = true;
        payload.info = Some(InfoInput {
            title: Some("Quarterly report".into()),
            author: Some("Ada".into()),
            ..Default::default()
        });
        let out = build(payload).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        let content = page_contents(&doc, pages[&1]);
        assert!(content.contains("CONFIDENTIAL"), "watermark in content");
        assert!(content.contains("1 / 2"), "page number in content");
        let info_id = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let info = doc.get_dictionary(info_id).unwrap();
        assert_eq!(
            info.get(b"Title").unwrap().as_str().unwrap(),
            b"Quarterly report"
        );
    }

    #[test]
    fn page_numbers_stay_inside_crop() {
        let mut payload = payload_for(vec![]);
        payload.page_numbers = true;
        payload.pages[0].crop = Some([0.1, 0.2, 0.9, 0.8]);
        let out = build(payload).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        let page = doc.get_dictionary(pages[&1]).unwrap();
        let crop = numbers(page.get(b"CropBox").unwrap()).unwrap();
        let content = page_contents(&doc, pages[&1]);
        assert!(content.contains("(1 / 2) Tj"), "page number present");
        let marker = "(1 / 2) Tj";
        let idx = content.find(marker).expect("page number text");
        let prefix = &content[..idx];
        let tm_idx = prefix.rfind(" Tm ").expect("text matrix");
        let numbers: Vec<&str> = prefix[..tm_idx].split_whitespace().collect();
        let y: f64 = numbers
            .last()
            .expect("text matrix y")
            .parse()
            .expect("numeric y");
        assert!(
            y >= crop[1] && y + 9.0 <= crop[3],
            "page number y {y} must be inside crop {crop:?}"
        );
    }

    #[test]
    fn removes_metadata_when_requested() {
        let mut payload = payload_for(vec![]);
        payload.remove_metadata = true;
        payload.info = Some(InfoInput {
            title: Some("Secret".into()),
            ..Default::default()
        });
        let out = build(payload).expect("build pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        assert!(doc.trailer.get(b"Info").is_err(), "no Info dictionary");
    }

    #[test]
    fn rejects_unknown_document() {
        let payload = Payload {
            docs: vec![],
            pages: vec![PageSpec {
                doc: "missing".into(),
                index: 0,
                rotation: 0,
                crop: None,
            }],
            annotations: vec![],
            forms: vec![],
            flatten: false,
            remove_metadata: false,
            info: None,
            watermark: None,
            page_numbers: false,
            bookmarks: vec![],
            security: None,
        };
        assert!(build(payload).is_err());
    }

    #[test]
    fn creates_blank_page() {
        let out = blank(500.0, 700.0).expect("blank pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let pages = doc.get_pages();
        assert_eq!(pages.len(), 1);
        let page = doc.get_dictionary(pages[&1]).unwrap();
        let mb = numbers(page.get(b"MediaBox").unwrap()).unwrap();
        assert_eq!(mb, vec![0.0, 0.0, 500.0, 700.0]);
    }

    #[test]
    fn writes_an_editable_outline() {
        let mut payload = payload_for(vec![]);
        payload.bookmarks = vec![BookmarkInput {
            title: "Start".into(),
            page: 1,
        }];
        let out = build(payload).expect("build bookmarked pdf");
        let doc = Document::load_mem(&out).expect("reparse");
        let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
        let catalog = doc.get_dictionary(root).unwrap();
        let outlines_id = catalog.get(b"Outlines").unwrap().as_reference().unwrap();
        let outlines = doc.get_dictionary(outlines_id).unwrap();
        assert_eq!(outlines.get(b"Count").unwrap().as_i64().unwrap(), 1);
        let first = outlines.get(b"First").unwrap().as_reference().unwrap();
        let item = doc.get_dictionary(first).unwrap();
        assert_eq!(
            std::str::from_utf8(item.get(b"Title").unwrap().as_str().unwrap()).unwrap(),
            "Start"
        );
    }

    #[test]
    fn builds_a_password_protected_and_sealed_pdf() {
        let mut payload = payload_for(vec![]);
        payload.security = Some(SecurityInput {
            user_password: "hunter2".into(),
            owner_password: String::new(),
            allow_print: true,
            allow_copy: true,
            allow_modify: false,
            allow_annotate: false,
            allow_assemble: false,
        });
        let out = build(payload).expect("build protected pdf");
        let opened =
            crate::security::open_pdf_bytes(&out, Some("hunter2")).expect("reopen protected pdf");
        assert!(opened.encrypted);
        assert_eq!(opened.seal, crate::security::SealStatus::Valid);
        let doc = Document::load_mem(&opened.data).expect("reparse decrypted pdf");
        assert_eq!(doc.get_pages().len(), 2);
    }
}
