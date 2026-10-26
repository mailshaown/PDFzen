// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{Engine as _, engine::general_purpose};
use pdfzen_lib::{
    ExtractedImage, Payload, SealStatus, blank, build, extract_images, images_to_pdf,
    open_pdf_bytes,
};
use serde::Serialize;

#[derive(Serialize)]
struct OpenPdfReply {
    data: String,
    encrypted: bool,
    seal: SealStatus,
}

#[tauri::command]
fn open_pdf(path: String, password: Option<String>) -> Result<OpenPdfReply, String> {
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let outcome = open_pdf_bytes(&bytes, password.as_deref())?;
    Ok(OpenPdfReply {
        data: general_purpose::STANDARD.encode(outcome.data),
        encrypted: outcome.encrypted,
        seal: outcome.seal,
    })
}

#[tauri::command]
fn save_pdf(path: String, data: String) -> Result<(), String> {
    match general_purpose::STANDARD.decode(data) {
        Ok(bytes) => std::fs::write(path, bytes).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Compose an edited document from a JSON payload and return base64 bytes.
#[tauri::command]
fn build_pdf(payload: String) -> Result<String, String> {
    let parsed: Payload =
        serde_json::from_str(&payload).map_err(|e| format!("parse payload: {e}"))?;
    let bytes = build(parsed)?;
    Ok(general_purpose::STANDARD.encode(bytes))
}

/// Create a one page blank PDF of the given size (points).
#[tauri::command]
fn blank_pdf(width: f64, height: f64) -> Result<String, String> {
    let bytes = blank(width, height)?;
    Ok(general_purpose::STANDARD.encode(bytes))
}

/// Create a PDF whose pages are the given image files.
#[tauri::command]
fn create_pdf_from_images(paths: Vec<String>) -> Result<String, String> {
    let bytes = images_to_pdf(&paths)?;
    Ok(general_purpose::STANDARD.encode(bytes))
}

/// Extract embedded raster images from a base64 PDF.
#[tauri::command]
fn extract_pdf_images(data: String) -> Result<Vec<ExtractedImage>, String> {
    let bytes = general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("decode pdf: {e}"))?;
    extract_images(&bytes)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            open_pdf,
            save_pdf,
            build_pdf,
            blank_pdf,
            create_pdf_from_images,
            extract_pdf_images
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    run();
}
